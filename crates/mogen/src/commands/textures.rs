use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use mogen_llm::gemini::GeminiClient;

use crate::commands::build::build;
use crate::common::{ensure_parent_dir, resolve_api_key};
use crate::format::format_duration;
use crate::spinner::Spinner;

pub(crate) fn textures_cmd(args: mogen_llm::textures::TexturesArgs) -> Result<()> {
    let src = fs::read_to_string(&args.input)
        .with_context(|| format!("reading {}", args.input.display()))?;
    let ast = mogen_dsl::parse(&src)?;

    let plans = mogen_llm::textures::build_plan(&ast, &args);

    if plans.is_empty() {
        println!("textures: no `material` declarations found in {}", args.input.display());
        return Ok(());
    }

    // Summary line first so users see what's about to happen.
    let mut to_gen = 0usize;
    let mut to_derive = 0usize;
    let mut to_existing = 0usize;
    let mut to_skip = 0usize;
    for p in &plans {
        match p.action {
            mogen_llm::textures::PlanAction::Generate => to_gen += 1,
            mogen_llm::textures::PlanAction::Derive => to_derive += 1,
            mogen_llm::textures::PlanAction::UseExisting => to_existing += 1,
            mogen_llm::textures::PlanAction::Skip(_) => to_skip += 1,
        }
    }
    println!(
        "textures: {} slot{} · {} to generate · {} to derive · {} existing · {} skipped",
        plans.len(),
        if plans.len() == 1 { "" } else { "s" },
        to_gen,
        to_derive,
        to_existing,
        to_skip,
    );
    for p in &plans {
        let tag = match p.action {
            mogen_llm::textures::PlanAction::Generate => "gen",
            mogen_llm::textures::PlanAction::Derive => "drv",
            mogen_llm::textures::PlanAction::UseExisting => "exist",
            mogen_llm::textures::PlanAction::Skip(reason) => reason,
        };
        println!(
            "  [{tag:>4}] {:<16} {:<10}  →  {}",
            p.material,
            p.kind.short_name(),
            p.rel_path.display()
        );
    }

    if args.dry_run {
        return Ok(());
    }

    // Only bring up a client if we'll actually need one. Cache-only and
    // derive-only runs don't need a key.
    let client = if to_gen > 0 {
        let api_key = resolve_api_key(args.api_key.clone())?;
        Some(GeminiClient::new(api_key))
    } else {
        None
    };

    let base_dir = args
        .input
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let start = Instant::now();
    let mut spinner = Spinner::new(
        &format!(
            "textures: {to_gen} albedo image{}, {to_derive} derived map{}",
            if to_gen == 1 { "" } else { "s" },
            if to_derive == 1 { "" } else { "s" },
        ),
        &[
            "fetching from Gemini",
            "decoding PNG",
            "deriving normals",
            "deriving roughness",
            "deriving occlusion",
            "writing texture files",
        ],
    );

    let edits = match mogen_llm::textures::run_plan(
        client.as_ref(),
        &args.model,
        &args,
        &ast,
        &plans,
        &base_dir,
        None,
    ) {
        Ok(e) => e,
        Err(e) => {
            spinner.abandon_with_message(format!("textures: failed — {e}"));
            return Err(e);
        }
    };

    spinner.set_message(format!("textures: splicing {} attribute{}", edits.len(), if edits.len() == 1 { "" } else { "s" }));
    let new_src = mogen_llm::textures::splice_textures(&src, &edits)?;

    let dsl_out = args.out.clone().unwrap_or_else(|| args.input.clone());
    ensure_parent_dir(&dsl_out)?;
    fs::write(&dsl_out, &new_src)
        .with_context(|| format!("writing {}", dsl_out.display()))?;

    spinner.finish_with_message(format!(
        "textures: wrote {} PNG{}, updated {} in {}",
        edits.len(),
        if edits.len() == 1 { "" } else { "s" },
        dsl_out.display(),
        format_duration(start.elapsed()),
    ));

    if args.no_build {
        return Ok(());
    }

    let glb_out = args.glb.clone().unwrap_or_else(|| args.input.with_extension("glb"));
    build(dsl_out, glb_out)
}
