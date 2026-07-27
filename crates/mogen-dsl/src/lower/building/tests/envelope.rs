//! The floor-division seal.
//!
//! A multi-storey building's exterior is not one surface. Each storey's
//! perimeter walls stop at that storey's ceiling, and the next storey's start
//! above it — so at every floor division there is a `ceiling_thickness` band
//! where no wall exists. The only thing closing it is the **slab rim**: the
//! floor slab runs a wall-thickness past the floorplate bounds, and that
//! under-wall strip is what bridges one storey's wall to the next.
//!
//! [`emit::shell`] carries a long comment about this because it has been
//! broken before: an `exit_pad` that extended stairwell cutouts outward
//! removed the strip wherever a stair or lift touched a wall, opening a hole
//! straight through the building's shell. From inside it was invisible — the
//! perimeter wall hides the strip — and from outside it read as a gap in the
//! floor-line band.
//!
//! That is the top-ranked risk of retargeting the generator onto the shared
//! architectural IR, because the obvious refactor computes the slab outline
//! independently of the walls and there is nothing to keep the two agreeing.
//! These tests are written *before* the port, so a regression is attributable.

use super::lower_src;
use glam::{Mat4, Vec3};
use mogen_core::{NodeId, SceneGraph};

/// Three storeys with a staircase and a lift, both of which punch cutouts
/// through the floor slabs, and at least one of which will land against a
/// perimeter wall.
const STACKED: &str = r#"
material "concrete" (color=[0.8, 0.8, 0.8])
building "tower" (
  seed=11, style="office-core", roof="flat", floors_above=3,
  floor_area=220, rooms=10, windows=6, entrances=1,
  staircases=1, elevators=1, mat="concrete",
) {
  room_type "office" (kind=staff_only, density=1)
}
"#;

fn world_of(g: &SceneGraph, id: NodeId) -> Mat4 {
    let mut m = Mat4::IDENTITY;
    let mut cur = Some(id);
    while let Some(i) = cur {
        let n = &g.nodes[i.0 as usize];
        m = Mat4::from_scale_rotation_translation(
            n.transform.scale,
            n.transform.rotation,
            n.transform.translation,
        ) * m;
        cur = n.parent;
    }
    m
}

/// Every triangle of a node's mesh, in world space.
fn world_tris(g: &SceneGraph, id: NodeId) -> Vec<[Vec3; 3]> {
    let n = &g.nodes[id.0 as usize];
    let Some(mesh) = &n.mesh else { return Vec::new() };
    let w = world_of(g, id);
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            [
                w.transform_point3(Vec3::from(mesh.positions[t[0] as usize])),
                w.transform_point3(Vec3::from(mesh.positions[t[1] as usize])),
                w.transform_point3(Vec3::from(mesh.positions[t[2] as usize])),
            ]
        })
        .collect()
}

fn nodes_named<'a>(g: &'a SceneGraph, name: &'a str) -> impl Iterator<Item = NodeId> + 'a {
    g.nodes
        .iter()
        .enumerate()
        .filter(move |(_, n)| n.name == name && n.mesh.is_some())
        .map(|(i, _)| NodeId(i as u32))
}

/// Whether a point in the ground plane falls inside a triangle's XZ shadow.
fn covers_xz(tri: &[Vec3; 3], p: [f32; 2]) -> bool {
    let side = |a: Vec3, b: Vec3| (b.x - a.x) * (p[1] - a.z) - (b.z - a.z) * (p[0] - a.x);
    let (d0, d1, d2) = (
        side(tri[0], tri[1]),
        side(tri[1], tri[2]),
        side(tri[2], tri[0]),
    );
    let neg = d0 < 0.0 || d1 < 0.0 || d2 < 0.0;
    let pos = d0 > 0.0 || d1 > 0.0 || d2 > 0.0;
    !(neg && pos)
}

/// World XZ bounds of every mesh whose node name starts with `prefix`.
fn xz_bounds(g: &SceneGraph, prefix: &str) -> [[f32; 2]; 2] {
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for (i, n) in g.nodes.iter().enumerate() {
        if !n.name.starts_with(prefix) || n.mesh.is_none() {
            continue;
        }
        for t in world_tris(g, NodeId(i as u32)) {
            for v in t {
                lo[0] = lo[0].min(v.x);
                hi[0] = hi[0].max(v.x);
                lo[1] = lo[1].min(v.z);
                hi[1] = hi[1].max(v.z);
            }
        }
    }
    [[lo[0], hi[0]], [lo[1], hi[1]]]
}

#[test]
fn a_floor_slab_reaches_past_the_walls_it_seals() {
    // The slab's footprint has to extend at least to the outer face of the
    // perimeter walls. If it stopped at the interior face, the band between
    // one storey's wall and the next would be open to the sky.
    let g = lower_src(STACKED);
    let slabs: Vec<NodeId> = nodes_named(&g, "slab_ceiling").collect();
    assert!(!slabs.is_empty(), "a three-storey building has floor divisions");

    let walls = xz_bounds(&g, "wall_");
    assert!(walls[0][0].is_finite(), "expected perimeter walls");

    for id in slabs {
        let mut lo = [f32::INFINITY; 2];
        let mut hi = [f32::NEG_INFINITY; 2];
        for t in world_tris(&g, id) {
            for v in t {
                lo[0] = lo[0].min(v.x);
                hi[0] = hi[0].max(v.x);
                lo[1] = lo[1].min(v.z);
                hi[1] = hi[1].max(v.z);
            }
        }
        assert!(
            lo[0] <= walls[0][0] + 1e-3 && hi[0] >= walls[0][1] - 1e-3,
            "slab spans x {lo:?}..{hi:?}, walls span {walls:?}",
        );
        assert!(
            lo[1] <= walls[1][0] + 1e-3 && hi[1] >= walls[1][1] - 1e-3,
            "slab spans z {lo:?}..{hi:?}, walls span {walls:?}",
        );
    }
}

#[test]
fn no_stairwell_cutout_punches_through_the_slab_rim() {
    // The regression that has actually happened. A cutout inflated outward
    // removes the under-wall strip wherever a stair or lift touches a wall,
    // and the resulting hole is invisible from inside the shaft.
    //
    // Rather than trust the bounds, this walks the rim itself: sample points
    // just inside the slab's outer edge, all the way round, and require every
    // one to sit under some slab triangle. A cutout that reached the edge
    // would leave a run of them uncovered.
    let g = lower_src(STACKED);

    for id in nodes_named(&g, "slab_ceiling") {
        let tris = world_tris(&g, id);
        assert!(!tris.is_empty(), "slab has no geometry");

        let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
        for t in &tris {
            for v in t {
                lo[0] = lo[0].min(v.x);
                hi[0] = hi[0].max(v.x);
                lo[1] = lo[1].min(v.z);
                hi[1] = hi[1].max(v.z);
            }
        }

        // A quarter of the wall thickness in from the edge: inside the rim
        // strip, and clear of the boundary itself where a point-in-triangle
        // test is ambiguous.
        let inset = 0.05_f32;
        let steps = 60;
        let mut open = Vec::new();
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = lo[0] + t * (hi[0] - lo[0]);
            let z = lo[1] + t * (hi[1] - lo[1]);
            for p in [
                [x, lo[1] + inset],
                [x, hi[1] - inset],
                [lo[0] + inset, z],
                [hi[0] - inset, z],
            ] {
                if !tris.iter().any(|t| covers_xz(t, p)) {
                    open.push(p);
                }
            }
        }
        assert!(
            open.is_empty(),
            "{} rim sample(s) uncovered, first {:?} — the floor-division seal \
             is broken there",
            open.len(),
            open.first(),
        );
    }
}

#[test]
fn the_seal_holds_for_every_style() {
    // The shaft placement depends on the layout, so a style whose stair lands
    // in a different place exercises a different part of the rim.
    for style in ["grid", "apartment-block", "hotel-corridor", "office-core", "maze"] {
        let src = STACKED.replace("office-core", style);
        let g = lower_src(&src);
        let walls = xz_bounds(&g, "wall_");

        for id in nodes_named(&g, "slab_ceiling") {
            let tris = world_tris(&g, id);
            let inset = 0.05_f32;
            let mid_z = 0.5 * (walls[1][0] + walls[1][1]);
            let mid_x = 0.5 * (walls[0][0] + walls[0][1]);
            for p in [
                [walls[0][0] + inset, mid_z],
                [walls[0][1] - inset, mid_z],
                [mid_x, walls[1][0] + inset],
                [mid_x, walls[1][1] - inset],
            ] {
                assert!(
                    tris.iter().any(|t| covers_xz(t, p)),
                    "{style}: slab does not reach {p:?}",
                );
            }
        }
    }
}
