use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, Result};
use mogen_dsl::module::FsLoader;
use mogen_moghub_client::MoghubClient;
use mogen_registry::{
    extract_use_graph,
    loader::LockMode,
    lockfile::{read_lock, write_lock},
    RegistryLoader,
};

use crate::format::{format_duration, print_build_summary};
use crate::spinner::Spinner;

/// Where the lockfile lives relative to the input. We follow Cargo's
/// convention: a file named `mog.lock` next to the entry `.mog`. Only
/// written when the source actually contains registry refs (or one
/// already exists).
fn lockfile_path(input: &Path) -> PathBuf {
    input
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mog.lock")
}

pub(crate) fn build(
    input: PathBuf,
    out: PathBuf,
    frozen: bool,
    offline: bool,
) -> Result<()> {
    let start = Instant::now();
    let label = input
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.to_string_lossy().into_owned());
    let filename = input.to_string_lossy().to_string();

    let mut spinner = Spinner::new(&format!("build {label}: reading"), &[]);

    let src = match fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: couldn't read file"));
            return Err(anyhow::Error::new(e).context(format!("reading {}", input.display())));
        }
    };

    spinner.set_message(format!("build {label}: parsing"));
    let ast = match mogen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: parse error"));
            return Err(e);
        }
    };

    spinner.set_message(format!("build {label}: validating DSL"));
    let diags = mogen_validate::validate_ast_with_source(&ast, input.parent());
    if mogen_core::has_errors(&diags) {
        spinner.abandon_with_message(format!("build {label}: validation failed"));
        mogen_validate::render_human(&filename, &src, &diags);
        return Err(anyhow!("refusing to build: validation errors"));
    }
    if !diags.is_empty() {
        // Warnings — render them, but keep going. Render between spinner state
        // changes so codespan output doesn't collide with the live spinner line.
        spinner.handle().pb.suspend(|| {
            mogen_validate::render_human(&filename, &src, &diags);
        });
    }

    spinner.set_message(format!("build {label}: lowering scene"));
    // Inspect the AST for registry refs. When present (or when a prior
    // build wrote a `mog.lock` we should honour) we lower through a
    // `RegistryLoader` so `use "@user/slug[@v]"` is resolved against the
    // local cache + the live MoGHub API. When absent we keep the
    // zero-network `FsLoader` path so a typical local build never opens
    // a socket.
    let lock_path = lockfile_path(&input);
    let initial_lock = read_lock(&lock_path)
        .map_err(|e| anyhow!("reading {}: {e}", lock_path.display()))?;
    let use_graph = extract_use_graph(&ast);
    let needs_registry = !use_graph.registry.is_empty() || initial_lock.is_some();

    let lower_result = if needs_registry {
        let mode = match (frozen, offline) {
            (true, _) => LockMode::Strict,
            (_, true) => LockMode::Offline,
            _ => LockMode::Honour,
        };
        let lock = initial_lock.clone().unwrap_or_default();
        let client = MoghubClient::from_env()
            .map_err(|e| anyhow!("constructing moghub client: {e}"))?;
        let mut loader = RegistryLoader::new(&client, lock, mode);
        let scene = mogen_dsl::lower::lower_with_loader(&ast, input.parent(), &mut loader);
        match scene {
            Ok(scene) => {
                if loader.lock_dirty {
                    write_lock(&lock_path, &loader.lock_after).map_err(|e| {
                        anyhow!("writing {}: {e}", lock_path.display())
                    })?;
                }
                Ok(scene)
            }
            Err(e) => Err(e),
        }
    } else {
        let mut loader = FsLoader::new();
        mogen_dsl::lower::lower_with_loader(&ast, input.parent(), &mut loader)
    };
    let mut scene = match lower_result {
        Ok(s) => s,
        Err(e) => {
            spinner.abandon_with_message(format!("build {label}: lowering failed"));
            return Err(e);
        }
    };

    spinner.set_message(format!("build {label}: checking scene graph"));
    let graph_diags = mogen_validate::validate_graph(&scene);
    if mogen_core::has_errors(&graph_diags) {
        spinner.abandon_with_message(format!("build {label}: graph validation failed"));
        mogen_validate::render_human(&filename, &src, &graph_diags);
        return Err(anyhow!("refusing to build: post-lowering validation errors"));
    }
    if !graph_diags.is_empty() {
        spinner.handle().pb.suspend(|| {
            mogen_validate::render_human(&filename, &src, &graph_diags);
        });
    }

    // Texture paths in the DSL are authored relative to the source `.mog` file,
    // not the process cwd. Resolve them here so the exporter sees absolute
    // paths regardless of how mogen was invoked.
    if let Some(base) = input.parent() {
        scene.resolve_texture_paths(base);
    }

    spinner.set_message(format!("build {label}: writing GLB"));
    if let Err(e) = mogen_export::write_glb(&scene, &out) {
        spinner.abandon_with_message(format!("build {label}: GLB export failed"));
        return Err(e);
    }

    let elapsed = start.elapsed();
    spinner.finish_with_message(format!(
        "build {label}: done in {}",
        format_duration(elapsed)
    ));
    print_build_summary(&out, &scene, elapsed);
    Ok(())
}
