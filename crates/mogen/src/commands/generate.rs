use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_llm::{
    apply_style_to_prompt, embed_seed_header, generate_with_repair, stamp_style_header,
    GenerateConfig, Provider, RepairConfig, Style, ThinkingLevel,
};

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, build_llm_client, ensure_parent_dir, format_cached_tokens,
    pick_default_seed, resolve_model, summarize_repair_errors,
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
    /// CLI override; `None` opts out of any style guidance (no prompt
    /// suffix, no `meta(style=…)` line). Generate has no prior file to
    /// inherit from, so this is the only source.
    pub style: Option<Style>,
}

pub(crate) fn generate(args: GenerateArgs) -> Result<()> {
    if !args.dry_run && args.out.is_none() {
        bail!("--out is required unless --dry-run is set");
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

    let client = build_llm_client(args.provider, args.api_key)?;
    let model = resolve_model(args.provider, args.model);

    let seed = args.seed.unwrap_or_else(pick_default_seed);

    // Prepend the visual-style guidance block when the user picked one.
    // `None` is a passthrough so existing prompts and goldens stay
    // byte-identical.
    let user_prompt = apply_style_to_prompt(&args.prompt, args.style);
    let mut cfg = GenerateConfig::new(user_prompt);
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

    let provider_label = args.provider.label();
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

    let outcome = match generate_with_repair(&client, cfg, &repair) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("generate: {provider_label} error — {e}"));
            return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &args.prompt, Some(effective_thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));
    let wrapped = stamp_style_header(&wrapped, args.style);

    if !outcome.is_ok() {
        pb.abandon_with_message(format!(
            "generate: DSL still invalid after {} call{} ({} tokens)",
            outcome.call_count,
            if outcome.call_count == 1 { "" } else { "s" },
            outcome.usage.total_tokens
        ));
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

    pb.finish_with_message(format!(
        "generate: DSL ready — {} call{}, {} tokens (prompt={}, response={}{})",
        outcome.call_count,
        if outcome.call_count == 1 { "" } else { "s" },
        outcome.usage.total_tokens,
        outcome.usage.prompt_tokens,
        outcome.usage.response_tokens,
        format_cached_tokens(&outcome.usage),
    ));

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
    build(dsl_path, out_path, None)
}
