use crate::lower::lower;
use crate::parser::parse;
use mogen_core::SceneGraph;

mod layouts;
mod multi_storey;
mod roof_and_cellar;
mod single_storey;

pub(super) fn lower_src(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

pub(super) fn count_kind(g: &SceneGraph, kind: &str) -> usize {
    g.nodes.iter().filter(|n| n.kind == kind).count()
}

pub(super) fn has_tag(g: &SceneGraph, tag: &str) -> bool {
    g.nodes
        .iter()
        .any(|n| n.tags.iter().any(|t| t == tag))
}

pub(super) fn count_role(g: &SceneGraph, role: &str) -> usize {
    g.nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some(role))
        .count()
}

/// Accumulate translations up the parent chain to get a node's world position.
/// Works for axis-aligned building hierarchies (no rotation along the path).
pub(super) fn world_translation(g: &SceneGraph, idx: usize) -> (f32, f32, f32) {
    let mut x = 0.0_f32;
    let mut y = 0.0_f32;
    let mut z = 0.0_f32;
    let mut cur = Some(mogen_core::NodeId(idx as u32));
    while let Some(id) = cur {
        let n = &g.nodes[id.0 as usize];
        x += n.transform.translation.x;
        y += n.transform.translation.y;
        z += n.transform.translation.z;
        cur = n.parent;
    }
    (x, y, z)
}

pub(super) fn slab_ceiling_count(g: &SceneGraph) -> usize {
    g.nodes
        .iter()
        .filter(|n| n.name == "slab_ceiling")
        .count()
}

pub(super) const MIN_GRID_SRC: &str = r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "shed" (
  seed=5, style="grid", floor_area=40, rooms=4, windows=2, entrances=1,
  mat="concrete",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;

pub(super) const MULTI_FLOOR_SRC: &str = r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "tower" (
  seed=3, style="grid",
  floor_area=80, rooms=8,
  floors_above=2, floors_below=1,
  staircases=1, elevators=1,
  mat="concrete",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;

pub(super) const ROOFTEST_SRC: &str = r#"
material "stone" (color=[0.62, 0.55, 0.5])
building "house" (
  seed=4, style="apartment-block",
  floor_area=80, rooms=4, windows=2, entrances=1,
  roof="ROOFKIND",
  mat="stone",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;
