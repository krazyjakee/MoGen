//! End-to-end lowering tests for `terrain` carving: `hole` voids and `road`
//! corridors. (Field sources, LOD seams and water live in `field.rs` / `emit.rs`
//! unit tests.)

use glam::Vec2;
use mogen_core::SceneGraph;

use crate::lower::lower;
use crate::parser::parse;

use super::carve;
use super::config::{ColliderMode, SourceKind, TerrainCfg};
use super::field;

fn lower_src(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

/// Every terrain chunk mesh in the graph (role == "terrain").
fn terrain_meshes(g: &SceneGraph) -> Vec<&mogen_core::Mesh> {
    g.nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("terrain"))
        .filter_map(|n| n.mesh.as_ref())
        .collect()
}

fn min_y(g: &SceneGraph) -> f32 {
    // Chunk meshes are local to their chunk centre, but Y is world-space.
    terrain_meshes(g)
        .iter()
        .flat_map(|m| m.positions.iter())
        .map(|p| p[1])
        .fold(f32::MAX, f32::min)
}

const FLAT_BASE: &str = r#"
terrain "ground" (
  seed=4,
  size=[40, 6, 40],
  source=fbm,
  resolution=64,
  chunks=2,
  lod_levels=1,
)
"#;

#[test]
fn open_hole_carves_a_void_below_the_surface() {
    // With lod_levels=1 there are no downward skirts, so a plain patch never dips
    // below y=0. An open hole punches rim walls down to a negative floor, so the
    // minimum vertex Y must drop well below the surface.
    let plain = lower_src(FLAT_BASE);
    assert!(
        min_y(&plain) >= -1e-3,
        "unholed terrain unexpectedly dips below 0 (min_y={})",
        min_y(&plain)
    );

    let holed = lower_src(
        r#"
terrain "ground" (
  seed=4,
  size=[40, 6, 40],
  source=fbm,
  resolution=64,
  chunks=2,
  lod_levels=1,
) {
  hole ( at=[0, 0], radius=6, depth=4 )
}
"#,
    );
    assert!(
        min_y(&holed) < -1.0,
        "open hole did not carve a void (min_y={})",
        min_y(&holed)
    );
}

#[test]
fn floor_cap_seals_the_pit_with_an_upward_floor() {
    // A `cap="floor"` hole adds flat, upward-facing quads at the pit floor; an
    // open hole has only near-vertical rim walls. So only the capped pit has a
    // low vertex whose normal points up.
    let has_low_up_facing = |g: &SceneGraph| {
        terrain_meshes(g).iter().any(|m| {
            m.positions
                .iter()
                .zip(&m.normals)
                .any(|(p, n)| p[1] < -1.0 && n[1] > 0.99)
        })
    };

    let open = lower_src(
        r#"
terrain "ground" (
  seed=4, size=[40, 6, 40], source=fbm, resolution=64, chunks=2, lod_levels=1,
) { hole ( at=[0, 0], radius=6, depth=4, cap="open" ) }
"#,
    );
    let floored = lower_src(
        r#"
terrain "ground" (
  seed=4, size=[40, 6, 40], source=fbm, resolution=64, chunks=2, lod_levels=1,
) { hole ( at=[0, 0], radius=6, depth=4, cap="floor" ) }
"#,
    );
    assert!(
        !has_low_up_facing(&open),
        "open hole should not have an up-facing floor"
    );
    assert!(
        has_low_up_facing(&floored),
        "floor cap did not seal the pit with an up-facing floor"
    );
}

#[test]
fn road_recolours_the_corridor() {
    // The corridor centreline is fully flattened and tinted toward COL_ROAD
    // ([0.30, 0.26, 0.21]); a plain patch never produces that colour.
    let road = lower_src(
        r#"
terrain "ground" (
  seed=4, size=[40, 6, 40], source=fbm, resolution=64, chunks=2, lod_levels=1,
) { road ( path=[[-18, 0], [18, 0]], width=4 ) }
"#,
    );
    let near_road = |g: &SceneGraph| {
        terrain_meshes(g).iter().any(|m| {
            m.colors.iter().any(|c| {
                (c[0] - 0.30).abs() < 0.04
                    && (c[1] - 0.26).abs() < 0.04
                    && (c[2] - 0.21).abs() < 0.04
            })
        })
    };
    assert!(near_road(&road), "road corridor was not tinted to the road colour");
    assert!(
        !near_road(&lower_src(FLAT_BASE)),
        "plain terrain unexpectedly contains the road colour"
    );
}

fn carve_cfg() -> TerrainCfg {
    TerrainCfg {
        seed: 4,
        mat_style: String::new(),
        size: [40.0, 6.0, 40.0],
        source: SourceKind::Fbm,
        octaves: 4,
        frequency: 0.06,
        persistence: 0.5,
        resolution: 64,
        chunks: 2,
        lod_levels: 1,
        smooth: 0,
        terrace: 0,
        sea_level: 0.0,
        colliders: ColliderMode::None,
        peaks: 0,
        flat_spots: 0,
        shore_points: 0,
        lod_scale: 1.0,
        debug_show_poi: false,
    }
}

#[test]
fn carve_roads_flattens_the_cross_section() {
    // Down a straight road along +X (z = 0), the height across the corridor
    // (varying z near the centreline) should be near-constant after carving.
    let cfg = carve_cfg();
    let mut f = field::build(&cfg);
    let road = carve::Road {
        pts: vec![Vec2::new(-18.0, 0.0), Vec2::new(18.0, 0.0)],
        half_width: 2.0,
        shoulder: 0.0,
    };
    let n = f.n;
    let segs = (n - 1) as f32;
    let world_z = |j: usize| -20.0 + (j as f32 / segs) * 40.0;

    // Cross-section heights at x≈0 BEFORE carving, restricted to |z| <= 2.
    let mid_i = n / 2;
    let before: Vec<f32> = (0..n)
        .filter(|&j| world_z(j).abs() <= 2.0)
        .map(|j| f.at(mid_i, j))
        .collect();
    let var = |v: &[f32]| {
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32
    };
    let before_var = var(&before);

    carve::carve_roads(&mut f, &cfg, std::slice::from_ref(&road));

    let after: Vec<f32> = (0..n)
        .filter(|&j| world_z(j).abs() <= 2.0)
        .map(|j| f.at(mid_i, j))
        .collect();
    let after_var = var(&after);
    assert!(
        after_var <= before_var + 1e-9 && after_var < 1e-4,
        "road did not flatten the cross-section (var {before_var} -> {after_var})"
    );
    // And the corridor is marked in the road mask.
    assert!(
        f.road_mask[(n / 2) * n + mid_i] > 0.5,
        "road mask not set on the corridor centre"
    );
}
