use std::path::PathBuf;
use std::sync::mpsc::Sender;

use mogen_llm::gemini::GeminiClient;
use mogen_llm::textures::{
    build_plan, default_textures_dir, run_plan, splice_textures, PlanAction, TextureProgress,
    TexturesArgs,
};
use mogen_llm::{Usage, DEFAULT_IMAGE_MODEL};

use crate::app::types::{LlmKind, LlmMessage, LlmOutcome, LlmProgress, TextureUiConfig};

/// Run the textures pipeline (image generation + splice) on a background
/// thread and shape the result into an [`LlmOutcome`] so it rides the same
/// channel as the text-LLM paths. Reports "PNGs written" in the `calls` slot
/// so `poll_llm` can display a counter without adding a new field.
///
/// Note: parsing and `build_plan` happen on this thread too — the previous
/// version did them on the UI thread before spawning, which stalled the
/// frame for big scenes.
pub(in crate::app) fn run_llm_textures(
    src: String,
    mg_path: PathBuf,
    api_key: String,
    cfg: TextureUiConfig,
    material_filter: Option<Vec<String>>,
    tx: Sender<LlmMessage>,
) -> LlmOutcome {
    let send_progress = |p: LlmProgress| {
        let _ = tx.send(LlmMessage::Progress(p));
    };
    let texture_model = DEFAULT_IMAGE_MODEL.to_string();
    let ast = match mogen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            return LlmOutcome {
                dsl: src,
                diagnostics: Vec::new(),
                usage: Usage::default(),
                calls: 0,
                model: texture_model,
                image_calls: 0,
                retry_prompt: None,
                error: Some(crate::app::types::LlmErrorInfo {
                    headline: "Parse error".into(),
                    detail: format!("Could not parse the .mog source: {e}"),
                    class: crate::app::types::LlmErrorClass::BadRequest,
                    retryable: false,
                }),
                kind: LlmKind::Textures,
            };
        }
    };

    // A per-material regenerate (right-click → Regenerate) implies "redo this
    // material's slots from scratch", so override `force` for the filtered
    // run regardless of what the panel checkbox says.
    let force = cfg.force || material_filter.is_some();
    let args = TexturesArgs {
        textures_dir: default_textures_dir(&mg_path),
        input: mg_path.clone(),
        out: None,
        glb: None,
        style: cfg.style.clone(),
        model: texture_model.clone(),
        force,
        dry_run: false,
        no_build: true,
        api_key: Some(api_key.clone()),
        no_pbr: false,
        no_normal: cfg.no_normal,
        no_metallic_roughness: cfg.no_metallic_roughness,
        no_occlusion: cfg.no_occlusion,
        texture_size: cfg.texture_size,
    };

    let plans: Vec<_> = build_plan(&ast, &args)
        .into_iter()
        .filter(|p| match &material_filter {
            Some(only) => only.iter().any(|m| m == &p.material),
            None => true,
        })
        .collect();

    // If nothing needs generating *or* deriving, leave the source untouched so
    // the editor doesn't get marked dirty.
    let anything_to_do = plans.iter().any(|p| {
        matches!(
            p.action,
            PlanAction::Generate | PlanAction::Derive | PlanAction::UseExisting
        )
    });
    if !anything_to_do {
        return LlmOutcome {
            dsl: src,
            diagnostics: Vec::new(),
            usage: Usage::default(),
            calls: 0,
            model: texture_model,
            image_calls: 0,
            retry_prompt: None,
            error: Some(crate::app::types::LlmErrorInfo {
                headline: "Nothing to generate".into(),
                detail: "Every material already has a full PBR texture set.".into(),
                class: crate::app::types::LlmErrorClass::BadRequest,
                retryable: false,
            }),
            kind: LlmKind::Textures,
        };
    }

    // Count the image-API calls we're about to make so we can charge the
    // session meter a per-image cost. Only albedo `Generate` plans hit the
    // API; cache hits and derivations are local.
    let image_call_count = plans
        .iter()
        .filter(|p| matches!(p.action, PlanAction::Generate))
        .count() as u32;

    // Image generation is Gemini-only — `LlmClient::new(provider, …)` doesn't
    // expose a synthesis API for the other backends. Callers MUST pass the
    // `GEMINI_API_KEY` here regardless of `settings.provider()` so this path
    // keeps working even when the user has selected OpenAI/Anthropic/Ollama
    // for the text DSL.
    let client = GeminiClient::new(api_key);
    let base_dir = mg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let tx_for_progress = tx.clone();
    let progress_cb = move |ev: TextureProgress| {
        let _ = tx_for_progress.send(LlmMessage::Progress(LlmProgress::Texture {
            current: ev.current,
            total: ev.total,
            material: ev.material,
            stage: ev.stage,
        }));
    };

    let report = run_plan(
        Some(&client),
        &args.model,
        &args,
        &ast,
        &plans,
        &base_dir,
        Some(&progress_cb),
    );
    let edits = report.edits;
    let failures = report.failures;

    // Build a single error banner that lists the failed materials. The whole
    // run is still treated as "ran to completion" (we still splice whatever
    // succeeded) — `error` here is informational so the user can see which
    // materials need a retry, not a fatal abort.
    let partial_error = if failures.is_empty() {
        None
    } else {
        let lines: Vec<String> = failures
            .iter()
            .map(|f| format!("• {}: {}", f.material, f.error))
            .collect();
        let detail = format!(
            "{} material{} failed to generate. The successful ones were spliced \
             into the DSL — Retry will only re-attempt the missing slots.\n\n{}",
            failures.len(),
            if failures.len() == 1 { "" } else { "s" },
            lines.join("\n"),
        );
        let total = count_materials_with_work(&plans);
        Some(crate::app::types::LlmErrorInfo {
            headline: format!(
                "{} of {} material{} failed",
                failures.len(),
                total.max(failures.len()),
                if total == 1 { "" } else { "s" },
            ),
            detail,
            class: crate::app::types::LlmErrorClass::Other,
            retryable: true,
        })
    };

    send_progress(LlmProgress::Status(format!(
        "splicing {} texture path(s) into DSL…",
        edits.len()
    )));

    match splice_textures(&src, &edits) {
        Ok(new_src) => LlmOutcome {
            dsl: mogen_dsl::stamp_mogen_version(&new_src, env!("CARGO_PKG_VERSION")),
            diagnostics: Vec::new(),
            usage: Usage::default(),
            calls: edits.len() as u32,
            model: texture_model,
            image_calls: image_call_count,
            retry_prompt: None,
            error: partial_error,
            kind: LlmKind::Textures,
        },
        Err(e) => LlmOutcome {
            dsl: src,
            diagnostics: Vec::new(),
            usage: Usage::default(),
            calls: 0,
            model: texture_model,
            image_calls: image_call_count,
            retry_prompt: None,
            error: Some(crate::app::types::LlmErrorInfo {
                headline: "Texture splice failed".into(),
                detail: format!("PNGs were written but rewriting the DSL failed: {e}"),
                class: crate::app::types::LlmErrorClass::Other,
                retryable: false,
            }),
            kind: LlmKind::Textures,
        },
    }
}

/// Count of distinct materials in `plans` that have at least one slot
/// requiring real work (`Generate` / `Derive` / `UseExisting`). Mirrors the
/// `total_materials` denominator that [`mogen_llm::textures::run_plan`]
/// computes internally — recomputed here so the partial-failure banner can
/// say "N of M materials failed" without leaking that field through the API.
fn count_materials_with_work(plans: &[mogen_llm::textures::Plan]) -> usize {
    use std::collections::HashSet;
    let mut materials: HashSet<&str> = HashSet::new();
    for p in plans {
        if matches!(
            p.action,
            PlanAction::Generate | PlanAction::Derive | PlanAction::UseExisting
        ) {
            materials.insert(p.material.as_str());
        }
    }
    materials.len()
}
