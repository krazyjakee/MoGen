use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mogen_core::SceneGraph;
use mogen_export::ExportOptions;

use crate::app::types::BuildOutcome;
use crate::pipeline::write_glb_with_source_and_options;

pub(in crate::app) fn run_build(
    scene: SceneGraph,
    out: PathBuf,
    source_dir: Option<PathBuf>,
    opts: ExportOptions,
    stage: Arc<Mutex<String>>,
    file_index: usize,
) -> BuildOutcome {
    // Keep a copy of the *effective* scene (after merge, if enabled) so we
    // can pass it back to the UI for a viewer refresh. The merge transform
    // is the expensive stage, so rather than run it twice we compute once
    // and hand the result to the exporter ourselves.
    let effective_scene: SceneGraph = if opts.merge_sibling_meshes {
        {
            let mut s = stage.lock().unwrap();
            *s = "merging sibling meshes".into();
        }
        mogen_export::merge::merge_sibling_meshes(&scene, |_| {})
    } else {
        scene
    };

    // With `merge_sibling_meshes` already applied, the exporter below only
    // needs to run the non-merge passes. Construct a new opts that leaves
    // merge off so the exporter doesn't redo the work.
    let post_merge_opts = ExportOptions {
        merge_sibling_meshes: false,
        ..opts
    };

    let stage_for_progress = Arc::clone(&stage);
    let progress = move |label: &str| {
        if let Ok(mut s) = stage_for_progress.lock() {
            *s = label.to_string();
        }
    };

    let write_result = write_glb_with_source_and_options(
        &effective_scene,
        &out,
        source_dir.as_deref(),
        &post_merge_opts,
        progress,
    );

    match write_result {
        Ok(()) => {
            let bytes = fs::metadata(&out).map(|m| m.len()).ok();
            BuildOutcome {
                file_index,
                path: out,
                exported_scene: opts
                    .merge_sibling_meshes
                    .then_some(effective_scene),
                bytes,
                error: None,
            }
        }
        Err(e) => BuildOutcome {
            file_index,
            path: out,
            exported_scene: None,
            bytes: None,
            error: Some(format!("{e:#}")),
        },
    }
}
