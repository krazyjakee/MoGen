use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use mogen_llm::{
    embed_seed_header, generate_with_repair, parse_prompt_header, parse_seed_header,
    parse_thinking_header, GenerateConfig, Provider, RepairConfig, ThinkingLevel,
};

use crate::commands::build::build;
use crate::common::{
    attach_system_instruction, build_llm_client, ensure_parent_dir, format_cached_tokens,
    pick_default_seed, resolve_model, summarize_repair_errors,
};
use crate::spinner::{Spinner, LLM_FLAVORS};

pub(crate) struct AnimateArgs {
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
}

pub(crate) fn animate(args: AnimateArgs) -> Result<()> {
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

    // Preserve the original `meta(prompt=…)` from the existing file so edits
    // don't clobber the provenance line with the animate instruction.
    let header_prompt = parse_prompt_header(&existing).unwrap_or_else(|| args.prompt.clone());

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

    let user_prompt = format!(
        "You are editing an existing mogen DSL file. Add or update ONLY animation \
declarations to satisfy this request:\n\n\
    {anim_prompt}\n\n\
Animation in mogen lives at the top level of the file (outside `scene {{ … }}`) \
and uses these node kinds:\n\
  • `joint \"name\" (type=hinge|slider|ball|rotor, axis=[x,y,z], pivot=\"node\", limits=[lo,hi])`\n\
  • `clip \"name\" (seconds=N) {{ track \"joint_or_node\" (from=0, to=V, prop=\"rotation\"|\"translation\"|\"scale\") }}`\n\
  • procedural templates (one-liners): `spin`, `open_close`, `wave`, `flap`, `idle`\n\
    e.g. `spin \"rotor_spin\" (target=\"rotor\", axis=[0,0,1], rpm=30)`\n\
         `open_close \"door_swing\" (target=\"door_hinge\", angle=90, seconds=1.2)`\n\
         `wave \"antenna_wave\" (target=\"antenna\", axis=[1,0,0], amplitude=15, hz=1.0)`\n\
         `flap \"wing_flap\" (target=\"wing\", axis=[0,0,1], amplitude=30, hz=2.0)`\n\
         `idle \"body_idle\" (target=\"body\", amplitude=0.02, hz=0.5)`\n\
When a template targets a scene node directly (not a joint), it MUST pass an \
explicit `axis` (except `idle`, which is a scale breathe with no axis).\n\n\
Do not touch geometry. Preserve every `scene`, `material`, `mesh`, `primitive`, \
`group`, `array`, `mirror`, `attach`, `connector`, `socket`, `plug`, `use`, and \
`module` exactly as written — same names, same order, same attributes. Your \
edits are limited to adding, removing, or tweaking top-level `joint`, `clip`, \
`spin`, `open_close`, `wave`, `flap`, and `idle` declarations.\n\n\
Every animation `target=` and `joint pivot=` must reference a node that already \
exists in the scene. If the request needs a new articulation, reuse existing \
node names — do not invent or rename nodes. If the scene lacks a suitable \
target for the requested motion, emit the closest reasonable animation on the \
existing nodes and keep going.\n\n\
Reply with ONLY the full updated DSL — no commentary, no markdown fences, no \
diff markers. Emit the entire file, not just the animation section. Do not \
write a `meta(...)` block; the caller stamps it after generation.\n\n\
Existing file:\n\n{existing}",
        existing = existing.trim_end(),
        anim_prompt = args.prompt.trim(),
    );

    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = model;
    cfg.budget_tokens = args.budget_tokens;
    if let Some(t) = args.temperature {
        cfg.temperature = Some(t);
    }
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(effective_thinking);
    attach_system_instruction(&mut cfg, &client, args.cached_content, args.no_cache, "animate");

    let total_attempts = args.max_repair_iters + 1;
    let mut pb = Spinner::new(
        &format!("animate: calling {provider_label} (attempt 1/{total_attempts})"),
        LLM_FLAVORS,
    );

    let pb_cb = pb.handle();
    let repair = RepairConfig {
        max_iters: args.max_repair_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let summary = summarize_repair_errors(diags);
            let attempt = iter + 1;
            pb_cb.set_message(format!(
                "animate: repair {attempt}/{total_attempts} — fixing {summary}"
            ));
        })),
    };

    let outcome = match generate_with_repair(&client, cfg, &repair) {
        Ok(o) => o,
        Err(e) => {
            pb.abandon_with_message(format!("animate: {provider_label} error — {e}"));
            return Err(anyhow!("{}: {e}", provider_label.to_lowercase()));
        }
    };

    let wrapped = embed_seed_header(&outcome.dsl, seed, &header_prompt, Some(effective_thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));

    if !outcome.is_ok() {
        pb.abandon_with_message(format!(
            "animate: DSL still invalid after {} call{} ({} tokens)",
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
        bail!("refusing to build: validation errors in animated DSL");
    }

    pb.finish_with_message(format!(
        "animate: DSL ready — {} call{}, {} tokens (prompt={}, response={}{})",
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

    build(dsl_path, out_path)
}
