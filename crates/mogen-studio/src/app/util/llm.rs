use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use mogen_llm::{
    cacheable_block, default_cache_path, embed_seed_header, format_imports_preserve_block,
    generate_with_repair, inline_block, parse_prompt_header, parse_seed_header, repair_message,
    resolve_or_create_cache, summarize_imports, validate_text, GenerateConfig, ImageInput,
    LlmClient, Provider, RepairConfig, StdlibIndex, ThinkingLevel, Usage, DEFAULT_TTL_SECONDS,
};

use crate::app::error_class::classify;
use crate::app::types::{LlmKind, LlmMessage, LlmOutcome, LlmProgress};

/// Tuning knobs for one LLM run. Gathered into a struct rather than a long
/// parameter list because `run_llm` already takes seven positional args and
/// every new setting would push another through every call site.
#[derive(Clone)]
pub(in crate::app) struct LlmRunConfig {
    pub model: String,
    pub thinking: ThinkingLevel,
    pub temperature: f32,
    pub max_repair_iters: u32,
    /// `None` → pick from the DSL header if present, else random; `Some(v)` →
    /// use exactly that seed (so the user can reproduce a prior generation).
    pub seed_override: Option<u64>,
    /// Path to the `claude` binary. Honoured only when the active provider is
    /// [`Provider::ClaudeCode`] (other providers ignore it). Empty/blank is a
    /// valid value — the underlying client falls back to `claude` on `PATH`.
    pub claude_code_path: String,
    /// Directory of the file being edited (for `Modify`/`Animate`/`Repair`),
    /// used to resolve relative `import "X.mog"` paths so the prompt can
    /// quote bounds for each `use`. `None` for unsaved buffers — the prompt
    /// still lists imports verbatim, just without AABBs.
    pub base_dir: Option<PathBuf>,
}

/// Pin the system instruction onto `cfg`. For Gemini, upload `cacheable_block`
/// (~17 KB of grammar/kinds/allowlist that's stable across sessions) as a
/// `cachedContents` resource and pair it with `inline_block(idx)` (~22 KB of
/// rules/conventions/fewshots/output) sent fresh per request. The cache is
/// persisted under `$HOME/.cache/mogen/` so repeat calls across sessions pay
/// the cached-input rate on the static portion. Falls back to the full
/// `sys_instr` inline on any failure (no cache dir, API down, instruction
/// below the model's minimum cacheable size). Mirrors the CLI's
/// `attach_system_instruction` so the two frontends share cache state.
fn attach_system_instruction(
    cfg: &mut GenerateConfig,
    client: &LlmClient,
    sys_instr: &Arc<String>,
    send_progress: &dyn Fn(LlmProgress),
) {
    if let Some(g) = client.as_gemini() {
        if let Some(cache_path) = default_cache_path() {
            let cacheable = cacheable_block();
            match resolve_or_create_cache(
                g,
                &cfg.model,
                &cacheable,
                &cache_path,
                DEFAULT_TTL_SECONDS,
            ) {
                Ok(name) => {
                    let idx =
                        StdlibIndex::from_registry(mogen_dsl::stdlib_registry());
                    cfg.cached_content = Some(name);
                    cfg.system_instruction = Some(inline_block(&idx));
                    return;
                }
                Err(e) => {
                    send_progress(LlmProgress::Status(format!(
                        "cache unavailable ({e}); sending system instruction inline"
                    )));
                }
            }
        }
    }
    cfg.system_instruction = Some((**sys_instr).clone());
}

/// Construct an [`LlmClient`] honoring Studio-only settings that don't fit
/// the bare `LlmClient::new(provider, api_key)` signature. Today that's just
/// the Claude Code binary path.
pub(in crate::app) fn build_provider_client(
    provider: Provider,
    api_key: String,
    claude_code_path: &str,
) -> LlmClient {
    if matches!(provider, Provider::ClaudeCode) {
        LlmClient::with_base_url(provider, api_key, claude_code_path)
    } else {
        LlmClient::new(provider, api_key)
    }
}

pub(in crate::app) fn run_llm(
    kind: LlmKind,
    prompt: String,
    existing: Option<String>,
    provider: Provider,
    image: Option<ImageInput>,
    api_key: String,
    run_cfg: LlmRunConfig,
    sys_instr: Arc<String>,
    tx: Sender<LlmMessage>,
) -> LlmOutcome {
    let send_progress = |p: LlmProgress| {
        // If the receiver is gone (user cancelled / closed the tab) just drop
        // the message — worker keeps running so the HTTP client can finish,
        // but we're no longer obliged to report progress.
        let _ = tx.send(LlmMessage::Progress(p));
    };

    let client = build_provider_client(provider, api_key, &run_cfg.claude_code_path);
    let seed = run_cfg.seed_override.unwrap_or_else(|| {
        existing
            .as_deref()
            .and_then(parse_seed_header)
            .unwrap_or_else(pick_default_seed)
    });

    // For edit-an-existing-file kinds, keep the original `meta(prompt=…)` value
    // so the provenance line isn't overwritten with the modify/animate text.
    let header_prompt = match kind {
        LlmKind::Generate | LlmKind::Textures => {
            // When an image was attached, annotate the prompt so the stamped
            // `meta(prompt=…)` records *why* the file looks the way it does
            // (otherwise an image-only generate writes an empty prompt, which
            // is misleading).
            if image.is_some() {
                let trimmed = prompt.trim();
                if trimmed.is_empty() {
                    "[image attached]".to_string()
                } else {
                    format!("[image attached] {trimmed}")
                }
            } else {
                prompt.clone()
            }
        }
        LlmKind::Modify | LlmKind::Animate | LlmKind::Repair => existing
            .as_deref()
            .and_then(parse_prompt_header)
            .unwrap_or_else(|| prompt.clone()),
    };

    let user_prompt = match kind {
        LlmKind::Generate => {
            // When the only input is an image, a non-empty text part still
            // helps the model commit to the DSL output mode (the system
            // instruction handles the schema, but Gemini's vision path
            // sometimes regresses to describing the image otherwise).
            // Concatenate the user's text with a short directive when an
            // image is attached; pass the prompt through unchanged when
            // there's no image so the legacy flow stays bit-for-bit.
            if image.is_some() {
                let trimmed = prompt.trim();
                if trimmed.is_empty() {
                    "Generate a mogen DSL scene that recreates the attached \
                     reference image as a 3D model."
                        .to_string()
                } else {
                    format!(
                        "Generate a mogen DSL scene that recreates the attached \
                         reference image as a 3D model. Additional guidance from \
                         the user:\n\n{trimmed}",
                    )
                }
            } else {
                prompt.clone()
            }
        }
        LlmKind::Modify => {
            let imports_block = existing
                .as_deref()
                .and_then(|src| {
                    format_imports_preserve_block(&summarize_imports(
                        src,
                        run_cfg.base_dir.as_deref(),
                    ))
                })
                .map(|s| format!("{s}\n"))
                .unwrap_or_default();
            format!(
                "You are editing an existing mogen DSL file. Apply this modification:\n\n\
                {mod_prompt}\n\n\
                {imports_block}\
                Make the smallest edit that satisfies the request. Do not rename, reorder, \
                reformat, or restyle parts the modification does not touch.\n\n\
                Reply with ONLY the full modified DSL — no commentary, no markdown fences. \
                Do not write a `meta(...)` block; the caller stamps it after generation.\n\n\
                Existing file:\n\n{existing}",
                mod_prompt = prompt.trim(),
                existing = existing.as_deref().unwrap_or("").trim_end(),
            )
        }
        LlmKind::Animate => {
            let imports_block = existing
                .as_deref()
                .and_then(|src| {
                    format_imports_preserve_block(&summarize_imports(
                        src,
                        run_cfg.base_dir.as_deref(),
                    ))
                })
                .map(|s| format!("{s}\n"))
                .unwrap_or_default();
            format!(
            "You are editing an existing mogen DSL file. APPEND new animation and rigging \
            declarations to satisfy this request:\n\n\
            {anim_prompt}\n\n\
            {imports_block}\
            mogen supports two rigging strategies. Pick the SIMPLER one that fits the request:\n\n\
            A) Node-transform animation (for articulations that can be expressed as rigid \
            transforms of existing scene nodes — door hinges, wheels, rotors, pistons, \
            breathing). Place these at the top level of the file (outside `scene {{ … }}`):\n\
              • `joint \"name\" (type=hinge|slider|ball|rotor, axis=[x,y,z], pivot=\"node\", limits=[lo,hi])`\n\
              • `clip \"name\" (seconds=N) {{ track \"joint_or_node\" (from=0, to=V, prop=\"rotation\"|\"translation\"|\"scale\") }}`\n\
              • procedural templates (one-liners): `spin`, `open_close`, `wave`, `flap`, `idle`\n\
                e.g. `spin \"rotor_spin\" (target=\"rotor\", axis=[0,0,1], rpm=30)`\n\
                     `open_close \"door_swing\" (target=\"door_hinge\", angle=90, seconds=1.2)`\n\
            When a template targets a scene node directly (not a joint), it MUST pass an \
            explicit `axis` (except `idle`, which is a scale breathe with no axis).\n\n\
            B) Skeletal skinning (for meshes that must deform smoothly — limbs bending, \
            tails whipping, any continuous body). Declare a `skeleton` INSIDE `scene {{ … }}` \
            and bind a primitive to it by adding `skin=\"skel_name\"` to its attrs:\n\
              • `skeleton \"skel_name\" {{ bone \"b1\" (pos=[x,y,z], envelope=R) {{ bone \"b2\" (pos=[…], envelope=R) {{ … }} }} }}`\n\
                — bones nest to form the chain; `pos` is RELATIVE to the parent bone; `envelope` \
                is the radius (in world units) within which vertices get weight from this bone.\n\
              • Any primitive in the same scene can bind to it by adding `skin=\"skel_name\"` \
                to its attribute list (e.g. `cylinder \"arm\" (…, skin=\"skel_name\")`). \
                Weights are assigned automatically by nearest-bone envelope falloff.\n\
              • Drive the deformation by rotating the bone scene nodes via a `clip` with \
                `track \"bone_name\" (prop=rotation, from=0, to=…)`. `from`/`to` are in \
                degrees when `prop=rotation`.\n\
            Minimal skinned example:\n\
              ```\n\
              scene {{\n\
                skeleton \"arm_skel\" {{\n\
                  bone \"shoulder\" (pos=[0,0,0], envelope=0.75) {{\n\
                    bone \"elbow\" (pos=[0,0.5,0], envelope=0.75)\n\
                  }}\n\
                }}\n\
                cylinder \"arm_mesh\" (pos=[0,0.5,0], radius=0.12, height=1.0, skin=\"arm_skel\")\n\
              }}\n\
              clip \"swing\" (seconds=1.0) {{ track \"elbow\" (prop=rotation, from=0, to=60) }}\n\
              ```\n\n\
            RULES:\n\
            - Prefer (A) for any rig the user describes in terms of hinges/sliders/spins. \
              Only reach for (B) when the request implies smooth continuous deformation of a \
              single mesh.\n\
            - Do not touch geometry. Preserve every `import`, `scene`, `material`, `mesh`, \
              `primitive`, `group`, `array`, `mirror`, `attach`, `connector`, `socket`, `plug`, \
              `use`, and `module` exactly as written — except you MAY add a single `skin=\"…\"` \
              attribute to the one primitive that a new (B)-style rig deforms.\n\
            - Preserve every existing `joint`, `clip`, `skeleton`, `spin`, `open_close`, \
              `wave`, `flap`, and `idle` declaration exactly as written. ADD new ones \
              alongside them; do not rewrite, rename, merge, or delete existing animation \
              or rigging. Only modify an existing declaration if the user's request \
              explicitly names it and asks to change it.\n\
            - Every animation `target=`, `joint pivot=`, and `track` name must reference a \
              node that already exists in the scene (bones become scene nodes once the \
              `skeleton` block is added). Do not invent or rename other nodes.\n\
            - New `joint`, `clip`, `skeleton`, and template names must not collide with \
              existing ones — pick a fresh unique name.\n\n\
            Reply with ONLY the full updated DSL — no commentary, no markdown fences. Do \
            not write a `meta(...)` block; the caller stamps it after generation.\n\n\
            Existing file:\n\n{existing}",
            anim_prompt = prompt.trim(),
            existing = existing.as_deref().unwrap_or("").trim_end(),
            )
        }
        LlmKind::Repair => {
            // The validator already ran in `start_llm_repair` before we got
            // here, but we re-run it on the worker thread to get the exact
            // diagnostics + spans. `repair_message` folds the previous DSL,
            // every diagnostic (with caret excerpts), and each code's fix
            // hint into the prompt — the same shape the repair loop uses on
            // subsequent iterations.
            let existing_src = existing.as_deref().unwrap_or("");
            let diags = validate_text(existing_src);
            repair_message(&header_prompt, existing_src, &diags, &[])
        }
        LlmKind::Textures => unreachable!("run_llm is text-only; textures uses run_llm_textures"),
    };

    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = run_cfg.model.clone();
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(run_cfg.thinking);
    cfg.temperature = Some(run_cfg.temperature);
    attach_system_instruction(&mut cfg, &client, &sys_instr, &send_progress);
    if let Some(img) = image {
        // Carried through every repair iteration: `repair.rs` rewrites
        // `cfg.user_prompt` but leaves `cfg.user_images` alone, so the model
        // keeps the visual reference while it fixes validator errors.
        cfg.user_images.push(img);
    }

    send_progress(LlmProgress::Status(format!(
        "calling {} ({}) — thinking={:?}",
        provider.label(),
        kind.label(),
        run_cfg.thinking,
    )));

    let max_iters = run_cfg.max_repair_iters;
    let tx_for_repair = tx.clone();
    let repair = RepairConfig {
        max_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let errors = diags
                .iter()
                .filter(|d| matches!(d.severity, mogen_core::Severity::Error))
                .count();
            let _ = tx_for_repair.send(LlmMessage::Progress(LlmProgress::Repair {
                iter,
                max: max_iters,
                errors,
            }));
        })),
    };

    match generate_with_repair(&client, cfg, &repair) {
        Ok(outcome) => {
            send_progress(LlmProgress::Status(format!(
                "done — {} call(s), {} tokens",
                outcome.call_count, outcome.usage.total_tokens
            )));
            let wrapped = embed_seed_header(
                &outcome.dsl,
                seed,
                &header_prompt,
                Some(run_cfg.thinking),
            );
            let wrapped =
                mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));
            LlmOutcome {
                dsl: wrapped,
                diagnostics: outcome.diagnostics,
                usage: outcome.usage,
                calls: outcome.call_count,
                model: run_cfg.model,
                image_calls: 0,
                retry_prompt: Some(prompt),
                error: None,
                kind,
            }
        }
        Err(e) => {
            let info = classify(&e);
            LlmOutcome {
                dsl: existing.unwrap_or_default(),
                diagnostics: Vec::new(),
                usage: Usage::default(),
                calls: 0,
                model: run_cfg.model,
                image_calls: 0,
                retry_prompt: Some(prompt),
                error: Some(info),
                kind,
            }
        }
    }
}

pub(in crate::app) fn pick_default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}
