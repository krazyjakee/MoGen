//! `mogen relock` — re-resolve every `use "@user/slug[@v]"` ref in a
//! `.mog` source to the registry's current latest, rewriting `mog.lock`
//! so the next build pins against the new versions.
//!
//! Cargo-style equivalent of `cargo update`. Exits non-zero if any ref
//! can't be resolved (broken registry connection, deleted upstream
//! module). Doesn't fetch full file bodies — only enough metadata to
//! pin — so it's cheap.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use mogen_moghub_client::MoghubClient;
use mogen_registry::{
    extract_use_graph,
    loader::LockMode,
    lockfile::{read_lock, write_lock},
    LockFile, RegistryLoader,
};

pub(crate) fn relock(input: PathBuf) -> Result<()> {
    let src = std::fs::read_to_string(&input)
        .map_err(|e| anyhow!("reading {}: {e}", input.display()))?;
    let ast = mogen_dsl::parse(&src)?;
    let graph = extract_use_graph(&ast);
    if graph.registry.is_empty() {
        println!("no registry refs in {} — nothing to relock", input.display());
        return Ok(());
    }

    let lock_path = input
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mog.lock");

    // Drop any existing pins so the loader re-fetches every ref as
    // latest. We keep imports/uses lists from the prior lock if it
    // existed — they're recomputed below from the use graph anyway.
    let prior = read_lock(&lock_path)
        .map_err(|e| anyhow!("reading {}: {e}", lock_path.display()))?;
    let mut empty_lock = LockFile::from_use_graph(&graph);
    empty_lock.resolved.clear();

    let client = MoghubClient::from_env()
        .map_err(|e| anyhow!("constructing moghub client: {e}"))?;
    let mut loader = RegistryLoader::new(&client, empty_lock, LockMode::Honour);
    // Walking the AST drives `Loader::load_registry` for every reachable
    // ref. We don't need to fully lower — `resolve_imports_with_loader`
    // is enough to fetch every transitive registry source into the
    // cache and stamp pins into `loader.lock_after`.
    let _ = mogen_dsl::module::resolve_imports_with_loader(&ast, input.parent(), &mut loader)?;

    let prior_count = prior.as_ref().map(|p| p.resolved.len()).unwrap_or(0);
    let new_count = loader.lock_after.resolved.len();
    write_lock(&lock_path, &loader.lock_after)
        .map_err(|e| anyhow!("writing {}: {e}", lock_path.display()))?;

    println!(
        "relocked {}: {} refs pinned ({} previously) → {}",
        input.display(),
        new_count,
        prior_count,
        lock_path.display()
    );
    for pin in &loader.lock_after.resolved {
        println!("  {} → v{}", pin.raw, pin.version);
    }
    Ok(())
}
