//! Bake an affine transform into a mesh's vertex positions and normals.

use glam::{Mat3, Mat4, Vec3, Vec4, Vec4Swizzles};

use mogen_core::Mesh;

/// Transform `mesh` by `m`. Positions use the full 4x4 (so translations
/// apply); normals use the inverse-transpose of the upper-left 3x3 so they
/// stay correct under non-uniform scale.
pub fn transform_mesh(mesh: &Mesh, m: Mat4) -> Mesh {
    let n_mat: Mat3 = Mat3::from_mat4(m).inverse().transpose();
    let positions: Vec<[f32; 3]> = mesh
        .positions
        .iter()
        .map(|p| {
            let v = m * Vec4::new(p[0], p[1], p[2], 1.0);
            [v.x, v.y, v.z]
        })
        .collect();
    let normals: Vec<[f32; 3]> = mesh
        .normals
        .iter()
        .map(|n| {
            let v = (n_mat * Vec3::from_array(*n)).normalize_or_zero();
            [v.x, v.y, v.z]
        })
        .collect();
    Mesh {
        positions,
        normals,
        indices: mesh.indices.clone(),
        uvs: mesh.uvs.clone(),
        joints: mesh.joints.clone(),
        weights: mesh.weights.clone(),
        colors: mesh.colors.clone(),
    }
}

/// Glam's Mat4::inverse is fine, but Vec4Swizzles is only pulled in by
/// downstream code so silence the unused-import lint.
#[allow(dead_code)]
fn _keep_swizzles(v: Vec4) -> Vec3 {
    v.xyz()
}
