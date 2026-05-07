use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_llm::{
    apply_style_to_prompt, embed_seed_header, format_imports_preserve_block, generate_with_repair,
    parse_prompt_header, parse_seed_header, parse_style_header, parse_thinking_header,
    stamp_style_header, summarize_imports, GenerateConfig, Provider, RepairConfig, Style,
    ThinkingLevel,
};

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, build_llm_client, ensure_parent_dir, format_cached_tokens,
    pick_default_seed, resolve_model, summarize_repair_errors,
};
use crate::spinner::{Spinner, LLM_FLAVORS};

pub(crate) struct ModifyArgs {
    pub input: PathBuf,
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
    /// CLI override; `None` falls through to the file's
    /// `meta(thinking=…)` attribute, then the library default.
    pub thinking: Option<ThinkingLevel>,
    /// CLI override; `None` falls through to the file's
    /// `meta(style=…)` attribute. So a styled file stays styled across
    /// edits without re-passing the flag.
    pub style: Option<Style>,
}

pub(crate) fn modify(args: ModifyArgs) -> Result<()> {
    let existing = fs::read_to_string(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;

    let seed = args
        .seed
        .or_else(|| parse_seed_header(&existing))
        .unwrap_or_else(pick_default_seed);

    // Precedence: CLI flag > per-file header > library default.
    let effective_thinking = args
        .thinking
        .or_else(|| parse_thinking_header(&existing))
        .unwrap_or(ThinkingLevel::High);

    // Precedence: CLI flag > per-file header > none. Without this, the
    // second prompt to a styled file would silently regress to the
    // default look.
    let effective_style = args.style.or_else(|| parse_style_header(&existing));

    // Preserve the original `meta(prompt=…)` from the existing file so edits
    // don't clobber the provenance line with the modify instruction.
    let header_prompt = parse_prompt_header(&existing).unwrap_or_else(|| args.prompt.clone());

    // Resolve output paths up front so we can create their parent directories
    // before burning tokens on the LLM call.
    let resolved_dsl_out = args.dsl_out.clone().unwrap_or_else(|| args.input.clone());
    let resolved_out = args
        .out
        .clone()
        .unwrap_or_else(|| args.input.with_extension("glb"));
    if !args.dry_run {
        ensure_parent_dir(&resolved_dsl_out)?;
        ensure_parent_dir(&resolved_out)?;
    }

    let client = build_llm_client(args.provider, args.api_key)?;
    let model = resolve_model(args.provider, args.model);
    let provider_label = args.provider.label();

    // Summarise top-level `import "X.mog"` declarations so the LLM (a) sees
    // each verbatim import line called out as load-bearing — without this
    // models routinely rewrite them into empty `module "X" {}` stubs and
    // silently strip every imported asset — and (b) knows how big each
    // `use "X"` will be in its local frame so composition prompts ("position
    // the items on the scene") don't end up with overlapping or floating
    // placements.
    let imports_block = format_imports_preserve_block(&summarize_imports(
        &existing,
        args.input.parent(),
    ));
    let imports_block = match imports_block {
        Some(s) => format!("{s}\n"),
        None => String::new(),
    };

    let user_prompt = format!(
        "You are editing an existing mogen DSL file. Apply this modification:\n\n\
    {mod_prompt}\n\n\
{imports_block}\
Make the smallest edit that satisfies the request. Do not rename, reorder, \
reformat, or restyle parts the modification does not touch — preserve their \
names, materials, transforms, connectors, attaches, joints, clips, and \
tracks verbatim. Do not \"improve\" unrelated geometry.\n\n\
When the edit adds a new primitive, it still needs a `material` (declare one \
or reuse an existing name) AND either an `attach` joining it to the rest of \
the scene or `tags=\"floating\"` on itself or an ancestor — otherwise the \
geometric connectivity validator (E1101) will reject it. When the edit \
removes or renames a node, update every reference to that name: `attach \
parent=`/`child=`, `joint pivot=`, animation `target=`, and any `socket`/\
`plug` that pointed at a removed connector.\n\n\
Reply with ONLY the full modified DSL — no commentary, no markdown fences, \
no diff markers. Emit the entire file, not just the changed region. Do not \
write a `meta(...)` block; the caller stamps it after generation.\n\n\
Existing file:\n\n{existing}",
        existing = existing.trim_end(),
        mod_prompt = args.prompt.trim(),
        imports_block = imports_block,
    );

    // Prepend the style block to the assembled scaffold prompt. `None` is
    // a passthrough so the existing prompt stays byte-for-byte identical.
    let user_prompt = apply_style_to_prompt(&user_prompt, effective_style);
    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = model;
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(effective_thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "modify");

    let total_attempts = args.max_repair_iters + 1;
    let mut pb = Spinner::new(
        &format!("modify: calling {provider_label} (attempt 1/{total_attempts})"),
        LLM_FLAVORS,
    );

    let pb_cb = pb.handle();
    let repair = RepairConfig {
        max_iters: args.max_repair_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let summary = summarize_repair_errors(diags);
            let attempt = iter + 1;
            pb_cb.set_message(format!(
                "modify: repair {attempt}/{total_attempts} — fixing {summary}"
            ));
        })),
    };

    let outcome = match generate_with_repair(&client, cfg, &repair) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("modify: {provider_label} error — {e}"));
            return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &header_prompt, Some(effective_thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));
    let wrapped = stamp_style_header(&wrapped, effective_style);

    if !outcome.is_ok() {
        pb.abandon_with_message(format!(
            "modify: DSL still invalid after {} call{} ({} tokens)",
            outcome.call_count,
            if outcome.call_count == 1 { "" } else { "s" },
            outcome.usage.total_tokens
        ));
        let filename = args.input.to_string_lossy().to_string();
        if args.dry_run {
            eprintln!(
                "{}",
                mogen_validate::render_json(&filename, &outcome.diagnostics)
            );
            println!("{}", wrapped);
        } else {
            mogen_validate::render_human(&filename, &wrapped, &outcome.diagnostics);
        }
        bail!("refusing to build: validation errors in modified DSL");
    }

    pb.finish_with_message(format!(
        "modify: DSL ready — {} call{}, {} tokens (prompt={}, response={}{})",
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

    let dsl_path = resolved_dsl_out;
    let out_path = resolved_out;

    fs::write(&dsl_path, &wrapped)
        .with_context(|| format!("writing {}", dsl_path.display()))?;

    build(dsl_path, out_path, None)
}
