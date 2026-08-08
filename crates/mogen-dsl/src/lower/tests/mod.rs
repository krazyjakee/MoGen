use super::*;
use glam::Vec3;

mod branch;
mod control_flow;
mod deform;
mod extrude_sweep_loft;
mod faced_box;
mod geometry_identity;
mod hull;
mod layout;
mod lights;
mod lod;
mod materials;
mod parametric_surfaces;
mod physics;
mod poly;
mod primitives;
mod shader;

pub(super) fn lower_src(src: &str) -> SceneGraph {
    let ast = crate::parser::parse(src).expect("parse");
    lower(&ast).expect("lower")
}

pub(super) fn find_mesh_node<'a>(g: &'a SceneGraph, name: &str) -> &'a mogen_core::SceneNode {
    g.nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node named {name}"))
}

pub(super) fn mesh_aabb(g: &SceneGraph, name: &str) -> (Vec3, Vec3) {
    let mesh = find_mesh_node(g, name).mesh.as_ref().unwrap();
    let min = mesh.positions.iter().fold(Vec3::splat(f32::INFINITY), |a, p| {
        a.min(Vec3::from_array(*p))
    });
    let max = mesh.positions.iter().fold(Vec3::splat(f32::NEG_INFINITY), |a, p| {
        a.max(Vec3::from_array(*p))
    });
    (min, max)
}
