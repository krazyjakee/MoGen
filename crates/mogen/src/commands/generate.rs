use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_llm::{
    compose_coder_prompt, embed_seed_header, generate_plan, generate_with_repair, visual_refine,
    GenerateConfig, ImageInput, Provider, RepairConfig, ThinkingLevel, Usage,
};
use crate::refine_render::render_dsl_to_png;

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, build_llm_client, ensure_parent_dir, format_cached_tokens,
    pick_default_seed, resolve_model, summarize_repair_errors, GeminiAuthMode,
};
use crate::spinner::{Spinner, LLM_FLAVORS};

pub(crate) struct GenerateArgs {
    pub prompt: String,
    pub out: Option<PathBuf>,
    pub dsl_out: Option<PathBuf>,
    pub seed: Option<u64>,
    pub provider: Provider,
    /// `None` -> use the provider's default model.
    pub model: Option<String>,
    pub dry_run: bool,
    pub budget_tokens: Option<u32>,
    pub max_repair_iters: u32,
    pub api_key: Option<String>,
    pub cached_content: Option<String>,
    pub no_cache: bool,
    pub temperature: Option<f32>,
    /// CLI override; `None` falls through to the library default.
    pub thinking: Option<ThinkingLevel>,
    /// Run the Architect agent before the Coder pass.
    pub plan: bool,
    /// Visual auto-refinement iteration count. `0` skips refinement.
    pub auto_refine: u32,
    /// Gemini credential-resolution mode (see [`GeminiAuthMode`]). Derived
    /// from `--provider`: `auto`/`gemini-oauth`/`antigravity`/`gemini` each
    /// pick a different credential path under [`Provider::Gemini`]. Ignored
    /// for non-Gemini providers.
    pub auth: GeminiAuthMode,
}

pub(crate) fn generate(args: GenerateArgs) -> Result<()> {
    if !args.dry_run && args.out.is_none() {
        bail!("--out is required unless --dry-run is set");
    }

    let provider = args.provider;

    // Visual refinement requires a vision-capable provider — bail early so
    // the user doesn't burn tokens on a Coder pass that can't be refined.
    // Today that's Gemini only.
    if args.auto_refine > 0 && !provider.supports_images() {
        bail!(
            "--auto-refine requires a vision-capable provider; {} does not support image input",
            provider.label()
        );
    }

    // Resolve output paths up front so we can create their parent directories
    // before burning tokens on the LLM call.
    let resolved_out = args.out.clone();
    let resolved_dsl_out: Option<PathBuf> = if args.dry_run {
        args.dsl_out.clone()
    } else {
        Some(
            args.dsl_out
                .clone()
                .unwrap_or_else(|| resolved_out.as_ref().unwrap().with_extension("mog")),
        )
    };
    if let Some(p) = resolved_out.as_deref() {
        ensure_parent_dir(p)?;
    }
    if let Some(p) = resolved_dsl_out.as_deref() {
        ensure_parent_dir(p)?;
    }

    let client = build_llm_client(provider, args.api_key, args.auth)?;
    let model = resolve_model(provider, args.model);

    let seed = args.seed.unwrap_or_else(pick_default_seed);

    let mut cfg = GenerateConfig::new(&args.prompt);
    cfg.model = model.clone();
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    // Generate has no prior file to read a header from; use CLI or library default.
    let effective_thinking = args.thinking.unwrap_or(ThinkingLevel::High);
    cfg.thinking_level = Some(effective_thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "generate");

    let provider_label = provider.label();

    // Total token usage rolls in the planner + every refinement call so the
    // final summary is honest about the full cost of the run.
    let mut total_usage = Usage::default();
    let mut total_calls: u32 = 0;

    // Phase 1 — Architect agent. Optional. Folds a Markdown plan into the
    // Coder pass's user prompt so the heavy spatial reasoning happens in
    // natural language before any DSL is emitted.
    if args.plan {
        let mut pb = Spinner::new(
            &format!("generate: planning with {provider_label}"),
            LLM_FLAVORS,
        );
        let plan_outcome = match generate_plan(&client, &cfg, &args.prompt) {
            Ok(o) => o,
            Err(e) => {
                pb.abandon_with_message(format!(
                    "generate: {provider_label} planner error — {e}"
                ));
                return Err(anyhow!("{}: planner: {e}", provider_label.to_lowercase()));
            }
        };
        pb.finish_with_message(format!(
            "generate: plan ready — {} prompt+{} response tokens",
            plan_outcome.usage.prompt_tokens, plan_outcome.usage.response_tokens,
        ));
        total_usage.add(&plan_outcome.usage);
        total_calls += 1;
        cfg.user_prompt = compose_coder_prompt(&args.prompt, &plan_outcome.plan);
    }

    // Phase 2 — Coder pass + repair loop.
    let total_attempts = args.max_repair_iters + 1;
    let mut pb = Spinner::new(
        &format!("generate: calling {provider_label} (attempt 1/{total_attempts})"),
        LLM_FLAVORS,
    );

    let pb_cb = pb.handle();
    let repair = RepairConfig {
        max_iters: args.max_repair_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let summary = summarize_repair_errors(diags);
            let attempt = iter + 1;
            pb_cb.set_message(format!(
                "generate: repair {attempt}/{total_attempts} — fixing {summary}"
            ));
        })),
    };

    let mut outcome = match generate_with_repair(&client, cfg.clone(), &repair) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("generate: {provider_label} error — {e}"));
            return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
        }
    };
    total_usage.add(&outcome.usage);
    total_calls += outcome.call_count;
    pb.finish_with_message(format!(
        "generate: DSL ready — {} call{}, {} tokens",
        outcome.call_count,
        if outcome.call_count == 1 { "" } else { "s" },
        outcome.usage.total_tokens,
    ));

    // Phase 3 — Visual auto-refinement. Each iteration renders the current
    // DSL, hands the PNG + DSL back to the Reviewer agent, and runs the
    // returned file through the validate+repair loop. Bails the whole
    // command if a refinement iteration fails to produce a clean DSL —
    // refusing to silently regress is the only safe behaviour: if the
    // reviewer's output doesn't compile, falling back to the unrefined
    // file would mask the failure.
    if args.auto_refine > 0 && outcome.is_ok() {
        let registry = mogen_dsl::stdlib_registry();
        for iter in 0..args.auto_refine {
            let label = format!("refine {}/{}", iter + 1, args.auto_refine);
            let mut pb = Spinner::new(
                &format!("generate: rendering for {label}"),
                LLM_FLAVORS,
            );

            let png = match render_dsl_to_png(&outcome.dsl, resolved_dsl_out.as_deref()) {
                Ok(bytes) => bytes,
                Err(e) => {
                    pb.abandon_with_message(format!(
                        "generate: {label} render failed — {e}; keeping unrefined DSL"
                    ));
                    // Render failure is recoverable — the unrefined DSL
                    // already validated. Stop iterating but don't fail the
                    // command.
                    break;
                }
            };

            pb.set_message(format!(
                "generate: critiquing with {provider_label} ({label}, attempt 1/{total_attempts})"
            ));
            let pb_cb = pb.handle();
            let label_for_cb = label.clone();
            let refine_repair = RepairConfig {
                max_iters: args.max_repair_iters,
                on_iteration: Some(Box::new(move |it, diags| {
                    let summary = summarize_repair_errors(diags);
                    let attempt = it + 1;
                    pb_cb.set_message(format!(
                        "generate: {label_for_cb} repair {attempt}/{total_attempts} — fixing {summary}"
                    ));
                })),
            };

            let image = ImageInput {
                mime_type: "image/png".to_string(),
                data: png,
            };

            // Z.ai vision auto-swap. The Coder pass on Z.ai runs against
            // a text model (`glm-5.1` by default); the Reviewer needs to
            // read the rendered PNG, so it has to route through
            // `glm-5v-turbo` instead. Mirrors the Studio-side override
            // in `app/util/llm.rs::run_llm_refine`.
            let mut refine_cfg = cfg.clone();
            if provider == Provider::Zai {
                refine_cfg.model = mogen_llm::ZAI_DEFAULT_VISION_MODEL.to_string();
            }
            let refined = match visual_refine(
                &client,
                &refine_cfg,
                &refine_repair,
                registry,
                &args.prompt,
                &outcome.dsl,
                image,
            ) {
                Ok(o) => o,
                Err(e) => {
                    // Treat a transient Reviewer LLM failure (network, 5xx,
                    // truncated response) the same as a render failure: keep
                    // the latest valid `outcome` and break. Only fatal-by-bail
                    // when the very first iteration fails — caught by the
                    // outcome.is_ok() check after the loop.
                    pb.abandon_with_message(format!(
                        "generate: {provider_label} {label} reviewer error — {e}; keeping last valid DSL"
                    ));
                    break;
                }
            };
            total_usage.add(&refined.usage);
            total_calls += refined.call_count;

            if !refined.is_ok() {
                pb.abandon_with_message(format!(
                    "generate: {label} produced invalid DSL after {} call{}",
                    refined.call_count,
                    if refined.call_count == 1 { "" } else { "s" },
                ));
                outcome = refined;
                break;
            }

            pb.finish_with_message(format!(
                "generate: {label} done — {} call{}, {} tokens",
                refined.call_count,
                if refined.call_count == 1 { "" } else { "s" },
                refined.usage.total_tokens,
            ));
            outcome = refined;
        }
    }

    let wrapped = embed_seed_header(&outcome.dsl, seed, &args.prompt, Some(effective_thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));

    if !outcome.is_ok() {
        eprintln!(
            "generate: DSL still invalid after {} total call{} ({} tokens{})",
            total_calls,
            if total_calls == 1 { "" } else { "s" },
            total_usage.total_tokens,
            format_cached_tokens(&total_usage),
        );
        // Print diagnostics so the user knows what the LLM missed.
        let filename = "<generated>".to_string();
        if args.dry_run {
            eprintln!("{}", mogen_validate::render_json(&filename, &outcome.diagnostics));
            println!("{}", wrapped);
        } else {
            mogen_validate::render_human(&filename, &wrapped, &outcome.diagnostics);
        }
        bail!("refusing to build: validation errors in generated DSL");
    }

    eprintln!(
        "generate: total — {} call{}, {} tokens (prompt={}, response={}{})",
        total_calls,
        if total_calls == 1 { "" } else { "s" },
        total_usage.total_tokens,
        total_usage.prompt_tokens,
        total_usage.response_tokens,
        format_cached_tokens(&total_usage),
    );

    if args.dry_run {
        println!("{}", wrapped);
        if let Some(dsl_path) = resolved_dsl_out {
            fs::write(&dsl_path, &wrapped)
                .with_context(|| format!("writing {}", dsl_path.display()))?;
        }
        return Ok(());
    }

    let out_path = resolved_out.expect("out checked above");
    let dsl_path = resolved_dsl_out.expect("dsl_out resolved above for non-dry-run");

    fs::write(&dsl_path, &wrapped)
        .with_context(|| format!("writing {}", dsl_path.display()))?;

    // Reuse the regular build path so the user gets the same progress line.
    build(dsl_path, out_path)
}