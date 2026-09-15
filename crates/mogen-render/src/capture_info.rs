use crate::OrbitCamera;
use mogen_core::{views::CaptureInfo, NodeId, SceneGraph, CAMERA_CONVENTION_VERSION};

/// Inspect actual rendered vertices against the same square camera matrix.
/// A close-up only diagnoses the selected subtree, not intentionally excluded
/// surrounding geometry. This never changes the camera.
pub fn capture_info(
    scene: &SceneGraph,
    camera: &OrbitCamera,
    revision: &str,
    view: &str,
    part: Option<&str>,
) -> CaptureInfo {
    let worlds = scene.world_transforms();
    let matrix = camera.view_proj(1.0);
    let selected = part
        .and_then(|name| scene.nodes.iter().position(|n| n.name == name))
        .map(|i| NodeId(i as u32));
    let mut count = 0;
    let mut names = vec![];
    for (i, node) in scene.nodes.iter().enumerate() {
        if selected.is_some_and(|id| !scene.is_ancestor(id, NodeId(i as u32))) {
            continue;
        }
        let Some(mesh) = &node.mesh else {
            continue;
        };
        let mut used = vec![false; mesh.positions.len()];
        for &index in &mesh.indices {
            if let Some(slot) = used.get_mut(index as usize) {
                *slot = true;
            }
        }
        let bad = mesh
            .positions
            .iter()
            .zip(used)
            .filter(|(p, used)| {
                if !*used {
                    return false;
                }
                let clip = matrix * worlds[i] * glam::Vec3::from_array(**p).extend(1.0);
                !clip.is_finite()
                    || clip.w <= 0.0
                    || clip.x.abs() > clip.w
                    || clip.y.abs() > clip.w
                    || clip.z.abs() > clip.w
            })
            .count();
        if bad != 0 {
            count += bad;
            names.push(node.name.clone());
        }
    }
    CaptureInfo {
        convention_version: CAMERA_CONVENTION_VERSION,
        revision: revision.into(),
        view: view.into(),
        yaw: camera.yaw,
        pitch: camera.pitch,
        target: camera.target.to_array(),
        distance: camera.distance(),
        out_of_frame_vertices: count,
        cropped_parts: names,
    }
}
