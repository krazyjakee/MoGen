use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_core::has_errors;
use mogen_llm::{
    embed_seed_header, generate_with_repair, parse_prompt_header, parse_seed_header,
    parse_thinking_header, repair_message, validate_text, GenerateConfig, Provider, RepairConfig,
    ThinkingLevel,
};

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, build_client, ensure_parent_dir, format_cached_tokens,
    pick_default_seed, resolve_api_key, resolve_model, summarize_repair_errors,
};
use crate::spinner::{Spinner, LLM_FLAVORS};

pub(crate) struct RepairArgs {
    pub input: PathBuf,
    pub out: Option<PathBuf>,
    pub dsl_out: Option<PathBuf>,
    pub seed: Option<u64>,
    pub provider: Provider,
    /// `None` -> use the provider's default model.
    pub model: Option<String>,
    pub dry_run: bool,
    pub no_build: bool,
    pub budget_tokens: Option<u32>,
    pub max_repair_iters: u32,
    pub api_key: Option<String>,
    pub cached_content: Option<String>,
    pub no_cache: bool,
    pub temperature: Option<f32>,
    /// CLI override; `None` falls through to the file's
    /// `meta(thinking=…)` attribute, then the library default.
    pub thinking: Option<ThinkingLevel>,
}

/// LLM-repair an existing .mog file. Validates first; exits early (success)
/// when the file already parses cleanly. Otherwise folds the diagnostics
/// (with source excerpts, carets, and fix hints) into the user prompt and
/// calls [`generate_with_repair`] so the bounded retry loop can converge on a
/// valid file.
pub(crate) fn repair(args: RepairArgs) -> Result<()> {
    let existing = fs::read_to_string(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;

    let diags = validate_text(&existing);
    if !has_errors(&diags) {
        eprintln!("repair: no errors to fix — file already validates");
        if args.dry_run {
            println!("{}", existing);
            return Ok(());
        }
        if args.no_build {
            return Ok(());
        }
        let out = args
            .out
            .unwrap_or_else(|| args.input.with_extension("glb"));
        ensure_parent_dir(&out)?;
        return build(args.input, out, false, false);
    }

    let filename = args.input.to_string_lossy().to_string();
    // Show the human-readable diagnostics up front so the user sees what
    // we're about to send to the model before any tokens are spent.
    eprintln!("repair: {} starting with these errors:", &filename);
    mogen_validate::render_human(&filename, &existing, &diags);

    let seed = args
        .seed
        .or_else(|| parse_seed_header(&existing))
        .unwrap_or_else(pick_default_seed);

    // Precedence: CLI flag > per-file header > library default.
    let effective_thinking = args
        .thinking
        .or_else(|| parse_thinking_header(&existing))
        .unwrap_or(ThinkingLevel::High);

    let header_prompt = parse_prompt_header(&existing)
        .unwrap_or_else(|| "repair validation errors".to_string());

    let resolved_dsl_out = args.dsl_out.clone().unwrap_or_else(|| args.input.clone());
    let resolved_out = args
        .out
        .clone()
        .unwrap_or_else(|| args.input.with_extension("glb"));
    if !args.dry_run {
        ensure_parent_dir(&resolved_dsl_out)?;
        if !args.no_build {
            ensure_parent_dir(&resolved_out)?;
        }
    }

    let api_key = resolve_api_key(args.provider, args.api_key)?;
    let client = build_client(args.provider, api_key);
    let model = resolve_model(args.provider, args.model);
    let provider_label = args.provider.label();

    // The repair message already contains the previous DSL, every diagnostic
    // (with caret excerpts), and each code's fix hint — exactly the shape the
    // repair loop sends on subsequent iterations. Using it as the initial
    // user prompt lets `generate_with_repair` treat "repair an existing
    // broken file" as just the first iteration of its normal loop.
    let user_prompt = repair_message(&header_prompt, &existing, &diags, &[]);

    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = model;
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(effective_thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "repair");

    let total_attempts = args.max_repair_iters + 1;
    let starting_summary = summarize_repair_errors(&diags);
    let mut pb = Spinner::new(
        &format!(
            "repair: calling {provider_label} (attempt 1/{total_attempts}) — fixing {starting_summary}"
        ),
        LLM_FLAVORS,
    );

    let pb_cb = pb.handle();
    let repair_cfg = RepairConfig {
        max_iters: args.max_repair_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let summary = summarize_repair_errors(diags);
            let attempt = iter + 1;
            pb_cb.set_message(format!(
                "repair: repair {attempt}/{total_attempts} — fixing {summary}"
            ));
        })),
    };

    let outcome = match generate_with_repair(&client, cfg, &repair_cfg) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("repair: {provider_label} error — {e}"));
            return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &header_prompt, Some(effective_thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));

    if !outcome.is_ok() {
        pb.abandon_with_message(format!(
            "repair: DSL still invalid after {} call{} ({} tokens)",
            outcome.call_count,
            if outcome.call_count == 1 { "" } else { "s" },
            outcome.usage.total_tokens
        ));
        if args.dry_run {
            eprintln!(
                "{}",
                mogen_validate::render_json(&filename, &outcome.diagnostics)
            );
            println!("{}", wrapped);
        } else {
            mogen_validate::render_human(&filename, &wrapped, &outcome.diagnostics);
        }
        bail!("refusing to build: validation errors still present after repair");
    }

    pb.finish_with_message(format!(
        "repair: DSL fixed — {} call{}, {} tokens (prompt={}, response={}{})",
        outcome.call_count,
        if outcome.call_count == 1 { "" } else { "s" },
        outcome.usage.total_tokens,
        outcome.usage.prompt_tokens,
        outcome.usage.response_tokens,
        format_cached_tokens(&outcome.usage),
    ));

    if args.dry_run {
        println!("{}", wrapped);
        return Ok(());
    }

    fs::write(&resolved_dsl_out, &wrapped)
        .with_context(|| format!("writing {}", resolved_dsl_out.display()))?;

    if args.no_build {
        return Ok(());
    }

    build(resolved_dsl_out, resolved_out, false, false)
}
