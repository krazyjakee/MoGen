use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_llm::{
    apply_style_to_prompt, compose_coder_prompt, embed_seed_header, format_imports_preserve_block,
    generate_edits_with_repair, generate_plan, generate_with_repair, parse_prompt_header,
    parse_seed_header, parse_style_header, parse_thinking_header, stamp_style_header,
    summarize_imports, visual_refine, GenerateConfig, ImageInput, Provider, RepairConfig, Style,
    ThinkingLevel, Usage, EDIT_BLOCK_INSTRUCTIONS,
};
use crate::refine_render::render_dsl_to_png;

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, build_llm_client, ensure_parent_dir, format_cached_tokens,
    pick_default_seed, resolve_model, summarize_repair_errors, GeminiAuthMode,
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
    /// Run the Architect agent before the Coder pass.
    pub plan: bool,
    /// Visual auto-refinement iteration count. `0` skips refinement.
    pub auto_refine: u32,
    /// Force a full file rewrite instead of asking the model for
    /// SEARCH/REPLACE edit blocks against the existing file. Useful when the
    /// requested change is broad enough that surgical edits would be more
    /// fragile than a clean restatement.
    pub rewrite: bool,
    /// Gemini credential-resolution mode derived from `--provider`.
    pub auth: GeminiAuthMode,
}

pub(crate) fn modify(args: ModifyArgs) -> Result<()> {
    let provider = args.provider;
    if args.auto_refine > 0 && !provider.supports_images() {
        bail!(
            "--auto-refine requires a vision-capable provider; {} does not support image input",
            provider.label()
        );
    }

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

    let client = build_llm_client(provider, args.api_key, args.auth)?;
    let model = resolve_model(provider, args.model);
    let provider_label = provider.label();

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

    // Phase 1 — optional Architect agent. The plan is computed against the
    // user's modification request only; the existing file is appended later
    // inside the Coder turn so the planner doesn't have to wade through the
    // whole DSL just to plan a small change.
    let mut total_usage = Usage::default();
    let mut total_calls: u32 = 0;
    let coder_request_prompt = if args.plan {
        let mut planner_cfg = GenerateConfig::new(&args.prompt);
        planner_cfg.model = model.clone();
        planner_cfg.budget_tokens = args.budget_tokens;
        if let Some(t) = args.temperature {
            planner_cfg.temperature = Some(t);
        }
        planner_cfg.seed = Some(seed);
        planner_cfg.thinking_level = Some(effective_thinking);

        let mut pb = Spinner::new(
            &format!("modify: planning with {provider_label}"),
            LLM_FLAVORS,
        );
        let plan_outcome = match generate_plan(&client, &planner_cfg, &args.prompt) {
            Ok(o) => o,
            Err(e) => {
                pb.abandon_with_message(format!(
                    "modify: {provider_label} planner error — {e}"
                ));
                return Err(anyhow!("{}: planner: {e}", provider_label.to_lowercase()));
            }
        };
        pb.finish_with_message(format!(
            "modify: plan ready — {} prompt+{} response tokens",
            plan_outcome.usage.prompt_tokens, plan_outcome.usage.response_tokens,
        ));
        total_usage.add(&plan_outcome.usage);
        total_calls += 1;
        compose_coder_prompt(&args.prompt, &plan_outcome.plan)
    } else {
        args.prompt.clone()
    };

    // Edit mode is the default for `modify`: the existing file is the
    // baseline and the model returns SEARCH/REPLACE blocks instead of
    // re-emitting the whole DSL. The repair loop transparently falls back
    // to a full rewrite if the response can't be parsed/applied as edit
    // blocks, so this is safe even when the model ignores the format.
    // `--rewrite` opts back into the legacy full-file path.
    let edit_mode = !args.rewrite;
    let trimmed_existing = existing.trim_end();
    let shared_constraints = "Make the smallest edit that satisfies the request. Do not rename, reorder, \
reformat, or restyle parts the modification does not touch — preserve their \
names, materials, transforms, connectors, attaches, joints, clips, and \
tracks verbatim. Do not \"improve\" unrelated geometry.\n\n\
When the edit adds a new primitive, it still needs a `material` (declare one \
or reuse an existing name) AND either an `attach` joining it to the rest of \
the scene or `tags=\"floating\"` on itself or an ancestor — otherwise the \
geometric connectivity validator (E1101) will reject it. When the edit \
removes or renames a node, update every reference to that name: `attach \
parent=`/`child=`, `joint pivot=`, animation `target=`, and any `socket`/\
`plug` that pointed at a removed connector.";
    let user_prompt = if edit_mode {
        format!(
            "You are editing an existing mogen DSL file. Apply this modification:\n\n\
{mod_prompt}\n\n\
{imports_block}\
{shared_constraints}\n\n\
Reply with one or more SEARCH/REPLACE blocks that apply your edit to the \
existing file below. Do not write a `meta(...)` block; the caller stamps it \
after generation. {edit_spec}\n\n\
Existing file:\n\n{existing}",
            existing = trimmed_existing,
            mod_prompt = coder_request_prompt.trim(),
            imports_block = imports_block,
            shared_constraints = shared_constraints,
            edit_spec = EDIT_BLOCK_INSTRUCTIONS,
        )
    } else {
        format!(
            "You are editing an existing mogen DSL file. Apply this modification:\n\n\
{mod_prompt}\n\n\
{imports_block}\
{shared_constraints}\n\n\
Reply with ONLY the full modified DSL — no commentary, no markdown fences, \
no diff markers. Emit the entire file, not just the changed region. Do not \
write a `meta(...)` block; the caller stamps it after generation.\n\n\
Existing file:\n\n{existing}",
            existing = trimmed_existing,
            mod_prompt = coder_request_prompt.trim(),
            imports_block = imports_block,
            shared_constraints = shared_constraints,
        )
    };

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
    // Spend-tracker attribution (issue 60). The scene path is the input
    // `.mog` so subsequent modifies of the same file accumulate against
    // one row in the per-file pill.
    cfg.spend_context = mogen_llm::CallContext {
        operation: mogen_llm::Operation::Modify.as_str().to_string(),
        scene_path: Some(args.input.display().to_string()),
        session_id: None,
    };
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
        allow_edit_mode: true,
    };

    let mut outcome = if edit_mode {
        match generate_edits_with_repair(&client, cfg.clone(), &repair, &existing) {
            Ok(o) => o,
            Err(e) => {
                pb.abandon_with_message(format!("modify: {provider_label} error — {e}"));
                return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
            }
        }
    } else {
        match generate_with_repair(&client, cfg.clone(), &repair) {
            Ok(o) => o,
            Err(e) => {
                pb.abandon_with_message(format!("modify: {provider_label} error — {e}"));
                return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
            }
        }
    };
    total_usage.add(&outcome.usage);
    total_calls += outcome.call_count;
    pb.finish_with_message(format!(
        "modify: DSL ready — {} call{}, {} tokens",
        outcome.call_count,
        if outcome.call_count == 1 { "" } else { "s" },
        outcome.usage.total_tokens,
    ));

    // Phase 3 — Visual auto-refinement. Mirrors `generate.rs`: render the
    // current DSL, hand the PNG back to the Reviewer agent, and feed its
    // critique into the repair loop. The Reviewer's "original prompt" is
    // synthesised from the existing file's `meta(prompt=…)` header plus the
    // applied edit so the model can critique geometry against the asset
    // description, not just the verb-phrase modification instruction.
    if args.auto_refine > 0 && outcome.is_ok() {
        let registry = mogen_dsl::stdlib_registry();
        let reviewer_prompt = format!(
            "{header}\n\nMost recent edit applied: {edit}",
            header = header_prompt.trim(),
            edit = args.prompt.trim(),
        );
        for iter in 0..args.auto_refine {
            let label = format!("refine {}/{}", iter + 1, args.auto_refine);
            let mut pb = Spinner::new(
                &format!("modify: rendering for {label}"),
                LLM_FLAVORS,
            );

            let png = match render_dsl_to_png(&outcome.dsl, Some(&resolved_dsl_out)) {
                Ok(bytes) => bytes,
                Err(e) => {
                    pb.abandon_with_message(format!(
                        "modify: {label} render failed — {e}; keeping unrefined DSL"
                    ));
                    break;
                }
            };

            pb.set_message(format!(
                "modify: critiquing with {provider_label} ({label}, attempt 1/{total_attempts})"
            ));
            let pb_cb = pb.handle();
            let label_for_cb = label.clone();
            let refine_repair = RepairConfig {
                max_iters: args.max_repair_iters,
                on_iteration: Some(Box::new(move |it, diags| {
                    let summary = summarize_repair_errors(diags);
                    let attempt = it + 1;
                    pb_cb.set_message(format!(
                        "modify: {label_for_cb} repair {attempt}/{total_attempts} — fixing {summary}"
                    ));
                })),
                allow_edit_mode: true,
            };

            let image = ImageInput {
                mime_type: "image/png".to_string(),
                data: png,
            };

            // Z.ai vision auto-swap. The Modify pass on Z.ai runs against
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
                &reviewer_prompt,
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
                        "modify: {provider_label} {label} reviewer error — {e}; keeping last valid DSL"
                    ));
                    break;
                }
            };
            total_usage.add(&refined.usage);
            total_calls += refined.call_count;

            if !refined.is_ok() {
                pb.abandon_with_message(format!(
                    "modify: {label} produced invalid DSL after {} call{}",
                    refined.call_count,
                    if refined.call_count == 1 { "" } else { "s" },
                ));
                outcome = refined;
                break;
            }

            pb.finish_with_message(format!(
                "modify: {label} done — {} call{}, {} tokens",
                refined.call_count,
                if refined.call_count == 1 { "" } else { "s" },
                refined.usage.total_tokens,
            ));
            outcome = refined;
        }
    }

    let wrapped = embed_seed_header(&outcome.dsl, seed, &header_prompt, Some(effective_thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));
    let wrapped = stamp_style_header(&wrapped, effective_style);

    if !outcome.is_ok() {
        eprintln!(
            "modify: DSL still invalid after {} total call{} ({} tokens{})",
            total_calls,
            if total_calls == 1 { "" } else { "s" },
            total_usage.total_tokens,
            format_cached_tokens(&total_usage),
        );
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

    eprintln!(
        "modify: total — {} call{}, {} tokens (prompt={}, response={}{})",
        total_calls,
        if total_calls == 1 { "" } else { "s" },
        total_usage.total_tokens,
        total_usage.prompt_tokens,
        total_usage.response_tokens,
        format_cached_tokens(&total_usage),
    );

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
