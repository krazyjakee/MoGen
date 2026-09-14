//! Shared, non-mutating contract for geometry sent to renderers and exporters.
//! Diagnostics are bounded per channel/part, rather than per bad vertex.
use std::fmt;

use glam::{DVec3, Mat4, Vec3};

use crate::{has_errors, Diagnostic, Mesh, NodeId, SceneGraph, SceneNode, Severity};

/// A rejected scene, retaining the same structured diagnostics as graph checks.
#[derive(Debug)]
pub struct MeshContractError {
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for MeshContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keep codes, source locations and counts available through anyhow and
        // the session tool's existing string error envelope.
        write!(
            f,
            "{}",
            serde_json::json!({"mesh_contract": "failed", "diagnostics": self.diagnostics})
        )
    }
}

impl std::error::Error for MeshContractError {}

pub fn ensure_renderable_scene(scene: &SceneGraph) -> Result<(), MeshContractError> {
    let diagnostics = validate_renderable_scene(scene);
    if has_errors(&diagnostics) {
        Err(MeshContractError { diagnostics })
    } else {
        Ok(())
    }
}

/// Validate mesh streams without changing geometry or generating normals.
/// Optional channels may be absent; present channels must match positions.
/// Zero-area faces and meshes with no triangles are advisory. Finite unit
/// normals are required at vertices of every nondegenerate rendered triangle.
pub fn validate_renderable_mesh(mesh: &Mesh) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let vertices = mesh.positions.len();
    for (name, len, required) in [
        ("NORMAL", mesh.normals.len(), !mesh.indices.is_empty()),
        ("TEXCOORD_0", mesh.uvs.len(), false),
        ("COLOR_0", mesh.colors.len(), false),
        ("JOINTS_0", mesh.joints.len(), false),
        ("WEIGHTS_0", mesh.weights.len(), false),
    ] {
        if (required || len != 0) && len != vertices {
            diags.push(Diagnostic::error(
                "E1200",
                format!(
                "{name} has {len} entries; expected {vertices}. Supply one entry per position{}.",
                if required { "; generate missing normals in the geometry constructor/importer" }
                else { " or omit the entire optional channel" }
            ),
            ));
        }
    }
    finite_channel(&mesh.positions, "POSITION", "E1202", &mut diags);
    finite_channel(&mesh.normals, "NORMAL", "E1204", &mut diags);
    finite_channel(&mesh.uvs, "TEXCOORD_0", "E1205", &mut diags);
    finite_channel(&mesh.colors, "COLOR_0", "E1206", &mut diags);
    finite_channel(&mesh.weights, "WEIGHTS_0", "E1207", &mut diags);
    if mesh.joints.is_empty() != mesh.weights.is_empty() {
        diags.push(Diagnostic::error("E1207",
            "JOINTS_0 and WEIGHTS_0 must be supplied together. Supply both skin channels or omit both."));
    }
    let bad_weights = mesh
        .weights
        .iter()
        .filter(|row| {
            row.iter().all(|v| v.is_finite())
                && (row.iter().any(|v| *v < 0.0) || (row.iter().sum::<f32>() - 1.0).abs() > 1e-3)
        })
        .count();
    if bad_weights != 0 {
        diags.push(Diagnostic::error("E1207", format!(
            "{bad_weights} WEIGHTS_0 rows have negative weights or do not sum to 1 (tolerance 0.001). Repair the skin weights."
        )));
    }
    if mesh.indices.len() % 3 != 0 {
        diags.push(Diagnostic::error("E1203", format!(
            "{} indices do not form complete triangles (expected a multiple of 3). Repair the triangle index stream.", mesh.indices.len()
        )));
    }
    let bad_indices = mesh
        .indices
        .iter()
        .filter(|&&i| i as usize >= vertices)
        .count();
    if bad_indices != 0 {
        diags.push(Diagnostic::error("E1203", format!(
            "{bad_indices} indices are out of range for {vertices} positions. Repair the triangle indices before rendering."
        )));
    }
    let mut used = vec![false; vertices];
    let mut degenerate = 0;
    for tri in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        if [a, b, c].iter().any(|&i| i >= vertices) {
            continue;
        }
        // f64 avoids underflow/overflow when classifying f32 coordinates.
        // No scene-unit epsilon: legitimately tiny open triangles stay valid.
        let p = |i: usize| DVec3::from_array(mesh.positions[i].map(f64::from));
        let cross = (p(b) - p(a)).cross(p(c) - p(a));
        if !cross.is_finite() {
            continue; // POSITION already diagnoses the non-finite vertex.
        }
        if cross.length_squared() == 0.0 {
            degenerate += 1;
        } else {
            for i in [a, b, c] {
                used[i] = true;
            }
        }
    }
    if mesh.normals.len() == vertices {
        let bad = mesh
            .normals
            .iter()
            .zip(used)
            .filter(|(n, used)| {
                if !*used || n.iter().any(|v| !v.is_finite()) {
                    return false;
                }
                let length = n.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt();
                (length - 1.0).abs() > 1e-3
            })
            .count();
        if bad != 0 {
            diags.push(Diagnostic::error("E1204", format!(
                "{bad} vertices on nondegenerate faces have zero or non-unit NORMAL vectors (length tolerance 0.001). Generate finite unit normals in the constructor/importer, preserving hard edges and UV seams."
            )));
        }
    }
    if degenerate != 0 {
        diags.push(Diagnostic::warning("W1208", format!(
            "{degenerate} zero-area triangles contribute no visible surface. Check collapsed dimensions or repeated points; triangles are retained unchanged."
        )));
    }
    if mesh.indices.is_empty() {
        diags.push(Diagnostic::warning("W1209",
            "Mesh has 0 triangles and will not be exported as a surface. Add faces if this part should be visible."));
    }
    diags
}

fn finite_channel<const N: usize>(
    channel: &[[f32; N]],
    name: &str,
    code: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let count = channel
        .iter()
        .filter(|row| row.iter().any(|v| !v.is_finite()))
        .count();
    if count != 0 {
        diags.push(Diagnostic::error(code, format!(
            "{count} {name} entries contain NaN/Inf. Correct the source parameters or importer producing non-finite values."
        )));
    }
}

fn for_node(mut diag: Diagnostic, node: &SceneNode, index: usize) -> Diagnostic {
    diag.message = format!("part {:?} (node #{index}): {}", node.name, diag.message);
    diag.span = node.source_span;
    diag.file = node.origin.as_ref().map(|p| p.display().to_string());
    diag
}

/// Check every vertex and local/world transform, never relying on bounds.
/// Traversal is iterative and rejects malformed hierarchy before downstream
/// recursive graph consumers can index or recurse into it.
pub fn validate_renderable_scene(scene: &SceneGraph) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut worlds = vec![None; scene.nodes.len()];
    let mut stack: Vec<_> = scene
        .roots
        .iter()
        .map(|&id| (id, None, Mat4::IDENTITY))
        .collect();
    let mut hierarchy_errors = 0;
    while let Some((NodeId(index), parent, parent_world)) = stack.pop() {
        let i = index as usize;
        let Some(node) = scene.nodes.get(i) else {
            hierarchy_errors += 1;
            continue;
        };
        if worlds[i].is_some() {
            hierarchy_errors += 1;
            continue;
        }
        if node.parent != parent {
            hierarchy_errors += 1;
        }
        let t = node.transform;
        let local_ok = t.translation.is_finite()
            && t.scale.is_finite()
            && t.rotation.is_finite()
            && (t.rotation.length_squared() - 1.0).abs() <= 1e-3;
        let world = if local_ok {
            parent_world * t.to_mat4()
        } else {
            parent_world
        };
        worlds[i] = Some(world);
        if !local_ok || !world.is_finite() {
            diags.push(for_node(Diagnostic::error("E1201",
                "Transform is non-finite or its rotation is not a unit quaternion. Correct this part's and its ancestors' transforms."), node, i));
        }
        stack.extend(
            node.children
                .iter()
                .map(|&id| (id, Some(NodeId(index)), world)),
        );
    }
    hierarchy_errors += worlds.iter().filter(|w| w.is_none()).count();
    if hierarchy_errors != 0 {
        diags.push(Diagnostic::error("E1210", format!(
            "{hierarchy_errors} invalid, duplicate, cyclic or unreachable node links. Repair roots, parent and child references before rendering."
        )));
    }
    for (i, node) in scene.nodes.iter().enumerate() {
        let Some(mesh) = &node.mesh else {
            continue;
        };
        diags.extend(
            validate_renderable_mesh(mesh)
                .into_iter()
                .map(|d| for_node(d, node, i)),
        );
        if let Some(world) = worlds[i].filter(|w| w.is_finite()) {
            if !mesh.indices.is_empty() && !world.inverse().is_finite() {
                diags.push(for_node(Diagnostic::error("E1201",
                    "World transform cannot produce a finite normal matrix. Remove zero/underflowing scale from this part or its ancestors."), node, i));
            }
            let bad = mesh
                .positions
                .iter()
                .filter(|p| {
                    p.iter().all(|v| v.is_finite())
                        && !world.transform_point3(Vec3::from_array(**p)).is_finite()
                })
                .count();
            if bad != 0 {
                diags.push(for_node(Diagnostic::error("E1202", format!(
                    "{bad} finite local positions become NaN/Inf in world space. Reduce coordinate/transform magnitudes."
                )), node, i));
            }
        }
        if !mesh.joints.is_empty() || !mesh.weights.is_empty() || node.skin.is_some() {
            let skin = node.skin.and_then(|id| scene.skins.get(id.0 as usize));
            let valid = skin.is_some_and(|skin| {
                mesh.joints.len() == mesh.positions.len()
                    && mesh.weights.len() == mesh.positions.len()
                    && !skin.joints.is_empty()
                    && mesh
                        .joints
                        .iter()
                        .flatten()
                        .all(|&j| (j as usize) < skin.joints.len())
            });
            if !valid {
                diags.push(for_node(Diagnostic::error("E1207",
                    "Skin binding/channels are missing or joint indices are out of range, including zero-weight slots. Bind a valid skin and supply matching joint/weight rows."), node, i));
            }
        }
    }
    for skin in &scene.skins {
        if skin.joints.len() != skin.inverse_bind_matrices.len()
            || skin
                .joints
                .iter()
                .any(|id| id.0 as usize >= scene.nodes.len())
            || skin
                .skeleton_root
                .is_some_and(|id| id.0 as usize >= scene.nodes.len())
            || skin
                .inverse_bind_matrices
                .iter()
                .flatten()
                .flatten()
                .any(|v| !v.is_finite())
        {
            diags.push(Diagnostic::error("E1207", format!(
                "skin {:?}: invalid joint references or inverse-bind matrices ({} joints, {} matrices). Supply valid nodes and one finite inverse-bind matrix per joint.",
                skin.name, skin.joints.len(), skin.inverse_bind_matrices.len()
            )));
        }
    }
    diags
}

/// Identify hard contract failures in combined graph diagnostics.
pub fn has_mesh_contract_errors(diags: &[Diagnostic]) -> bool {
    diags
        .iter()
        .any(|d| d.severity == Severity::Error && d.code.starts_with("E12"))
}
