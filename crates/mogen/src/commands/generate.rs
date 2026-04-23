use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_llm::gemini::{GeminiClient, GenerateConfig};
use mogen_llm::{embed_seed_header, generate_with_repair, RepairConfig, ThinkingLevel};

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, ensure_parent_dir, format_cached_tokens, pick_default_seed,
    resolve_api_key, summarize_repair_errors,
};
use crate::spinner::{Spinner, GEMINI_FLAVORS};

pub(crate) struct GenerateArgs {
    pub prompt: String,
    pub out: Option<PathBuf>,
    pub dsl_out: Option<PathBuf>,
    pub seed: Option<u64>,
    pub model: String,
    pub dry_run: bool,
    pub budget_tokens: Option<u32>,
    pub max_repair_iters: u32,
    pub api_key: Option<String>,
    pub cached_content: Option<String>,
    pub no_cache: bool,
    pub temperature: Option<f32>,
    /// CLI override; `None` falls through to the library default.
    pub thinking: Option<ThinkingLevel>,
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
                .unwrap_or_else(|| resolved_out.as_ref().unwrap().with_extension("mg")),
        )
    };
    if let Some(p) = resolved_out.as_deref() {
        ensure_parent_dir(p)?;
    }
    if let Some(p) = resolved_dsl_out.as_deref() {
        ensure_parent_dir(p)?;
    }

    let api_key = resolve_api_key(args.api_key)?;
    let client = GeminiClient::new(api_key);

    let seed = args.seed.unwrap_or_else(pick_default_seed);

    let mut cfg = GenerateConfig::new(&args.prompt);
    cfg.model = args.model;
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    // Generate has no prior file to read a header from; use CLI or library default.
    let effective_thinking = args.thinking.unwrap_or(ThinkingLevel::High);
    cfg.thinking_level = Some(effective_thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "generate");

    let total_attempts = args.max_repair_iters + 1;
    let mut pb = Spinner::new(
        &format!("generate: calling Gemini (attempt 1/{total_attempts})"),
        GEMINI_FLAVORS,
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
            pb.abandon_with_message(format!("generate: Gemini error — {e}"));
            return Err(anyhow!("gemini: {e}"));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &args.prompt, Some(effective_thinking));

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
    build(dsl_path, out_path)
}
