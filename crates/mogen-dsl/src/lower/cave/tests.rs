//! End-to-end lowering tests for the `cave` node.

use crate::lower::lower;
use crate::parser::parse;
use mogen_core::SceneGraph;

fn lower_src(src: &str) -> SceneGraph {
    let ast = parse(src).expect("parse");
    lower(&ast).expect("lower")
}

fn count_role(g: &SceneGraph, role: &str) -> usize {
    g.nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some(role))
        .count()
}

const BASIC: &str = r#"
cave "den" (
  seed=3,
  size=[20, 13, 20],
  chambers=5,
  levels=2,
  resolution=48,
  entrances=1,
)
"#;

const BASIC_ALT_SEED: &str = r#"
cave "den" (
  seed=99,
  size=[20, 9, 20],
  chambers=5,
  levels=2,
  resolution=48,
  entrances=1,
)
"#;

#[test]
fn cave_emits_a_rock_shell() {
    let g = lower_src(BASIC);
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .expect("rock node");
    let mesh = rock.mesh.as_ref().expect("rock mesh");
    assert!(
        mesh.positions.len() > 100,
        "expected a substantial rock mesh, got {} verts",
        mesh.positions.len()
    );
}

#[test]
fn cave_subtree_is_non_editable() {
    let g = lower_src(BASIC);
    // The wrapper stays editable; everything generated under it does not.
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert!(!rock.editable, "generated rock should be non-editable");
}

#[test]
fn cave_rock_gets_a_trimesh_collider() {
    let g = lower_src(BASIC);
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert!(rock.collider.is_some(), "rock should carry a collider");
}

#[test]
fn cave_is_deterministic_under_same_seed() {
    let a = lower_src(BASIC);
    let b = lower_src(BASIC);
    assert_eq!(a.nodes.len(), b.nodes.len());
    let ra = a
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    let rb = b
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert_eq!(
        ra.mesh.as_ref().unwrap().positions.len(),
        rb.mesh.as_ref().unwrap().positions.len()
    );
}

#[test]
fn cave_seed_changes_geometry() {
    let a = lower_src(BASIC);
    let b = lower_src(BASIC_ALT_SEED);
    let ra = a
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    let rb = b
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    // Different seed → different chamber layout → (almost surely) a different
    // vertex count or at least different positions.
    let pa = &ra.mesh.as_ref().unwrap().positions;
    let pb = &rb.mesh.as_ref().unwrap().positions;
    assert!(pa != pb, "different seeds should produce different caves");
}

#[test]
fn cave_decorations_emit_nodes() {
    let src = r#"
cave "grotto" (
  seed=4,
  size=[22, 10, 22],
  chambers=5,
  resolution=40,
  stalagmites=6,
  stalactites=4,
  rock_piles=2,
  pools=1,
  lakes=1,
)
"#;
    let g = lower_src(src);
    assert_eq!(count_role(&g, "stalagmite"), 6);
    assert_eq!(count_role(&g, "stalactite"), 4);
    assert_eq!(count_role(&g, "rock_pile"), 2);
    assert_eq!(count_role(&g, "pool"), 1);
    assert_eq!(count_role(&g, "lake"), 1);
}

#[test]
fn cave_feature_overrides_count_and_material() {
    let src = r#"
material "ice" (color=[0.7, 0.85, 0.95])
cave "ice_cave" (
  seed=2,
  size=[20, 9, 20],
  chambers=4,
  resolution=40,
  stalagmites=2,
) {
  feature "spikes" (kind=stalagmite, count=5, mat="ice", min_size=0.4, max_size=0.9)
}
"#;
    let g = lower_src(src);
    // The feature's count=5 overrides the top-level stalagmites=2.
    assert_eq!(count_role(&g, "stalagmite"), 5);
    let ice = g.find_material("ice").expect("ice material");
    let any_ice = g
        .nodes
        .iter()
        .any(|n| n.role.as_deref() == Some("stalagmite") && n.material == Some(ice));
    assert!(any_ice, "stalagmites should bind the overridden material");
}

#[test]
fn cave_debug_hide_shell_strips_the_outer_hull() {
    let full = lower_src(BASIC);
    let cut = lower_src(&BASIC.replace("entrances=1,", "entrances=1, debug_hide_shell=1,"));
    let extents = |g: &SceneGraph| -> ([f32; 3], [f32; 3]) {
        let rock = g
            .nodes
            .iter()
            .find(|n| n.role.as_deref() == Some("cave_rock"))
            .unwrap();
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in &rock.mesh.as_ref().unwrap().positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        (lo, hi)
    };
    let (flo, fhi) = extents(&full);
    let (clo, chi) = extents(&cut);
    // With the outer hull removed, the remaining inner cavity walls sit well
    // inside the original block on every axis — the bounding box shrinks inward
    // from all six faces, not just the +Z one.
    for k in 0..3 {
        assert!(
            chi[k] < fhi[k] - 0.5 && clo[k] > flo[k] + 0.5,
            "axis {k}: hidden-shell extent should shrink inward on both ends \
             (full [{}, {}], cut [{}, {}])",
            flo[k],
            fhi[k],
            clo[k],
            chi[k],
        );
    }
}

#[test]
fn cave_columns_span_floor_to_ceiling_and_get_colliders() {
    let src = r#"
cave "pillared" (
  seed=5,
  size=[24, 12, 24],
  chambers=6,
  resolution=48,
  columns=4,
)
"#;
    let g = lower_src(src);
    let columns: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("column"))
        .collect();
    assert!(!columns.is_empty(), "expected at least one column placed");
    for col in &columns {
        let mesh = col.mesh.as_ref().expect("column mesh");
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in &mesh.positions {
            lo = lo.min(p[1]);
            hi = hi.max(p[1]);
        }
        // A column spans a real vertical extent (floor to ceiling), not a stub.
        assert!(hi - lo > 1.0, "column too short: span {}", hi - lo);
        // Like every other solid cave mesh, a column gets a collider.
        assert!(col.collider.is_some(), "column should carry a collider");
    }
}

#[test]
fn cave_emits_points_of_interest() {
    let src = r#"
cave "poi_cave" (
  seed=7,
  size=[28, 14, 28],
  chambers=8,
  levels=2,
  resolution=48,
  columns=3,
  mushrooms=5,
)
"#;
    let g = lower_src(src);
    // Mushroom spots are an exact count and are pure markers (no mesh/collider).
    let shrooms: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("mushroom_spot"))
        .collect();
    assert_eq!(shrooms.len(), 5);
    for s in &shrooms {
        assert!(s.mesh.is_none(), "POI markers carry no geometry");
        assert!(s.collider.is_none(), "POI markers carry no collider");
        assert!(s.tags.iter().any(|t| t == "poi"), "POI tagged for the importer");
    }
    // A column placed → its base is marked.
    assert!(
        count_role(&g, "column_base") > 0,
        "each column should emit a column_base POI"
    );
    // Two layers joined by passages → at least one inter-floor climb gets a
    // ladder / rope anchor.
    assert!(
        count_role(&g, "ladder_anchor") > 0,
        "inter-layer climbs should emit ladder_anchor POIs"
    );
    // Ladder anchors are oriented to face their tunnel: at least one carries a
    // non-identity yaw, while placement-only markers stay unrotated.
    let ladders: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("ladder_anchor"))
        .collect();
    assert!(
        ladders
            .iter()
            .any(|n| n.transform.rotation.angle_between(glam::Quat::IDENTITY) > 1e-3),
        "at least one ladder_anchor should be rotated toward its climb"
    );
    for s in &shrooms {
        assert!(
            s.transform.rotation.angle_between(glam::Quat::IDENTITY) < 1e-6,
            "placement-only POIs stay unrotated"
        );
    }
}

#[test]
fn cave_emits_entrance_pois() {
    // Each punched mouth (default entrances=1) gets a floor `entrance` POI,
    // oriented to face out of the wall, with no geometry/collider of its own.
    let src = r#"
cave "mouthy" (
  seed=3,
  size=[28, 14, 28],
  chambers=8,
  levels=2,
  resolution=48,
  entrances=2,
)
"#;
    let g = lower_src(src);
    let entrances: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("entrance"))
        .collect();
    assert_eq!(entrances.len(), 2, "one entrance POI per punched mouth");
    for e in &entrances {
        assert!(e.mesh.is_none(), "entrance POI carries no geometry");
        assert!(e.collider.is_none(), "entrance POI carries no collider");
        assert!(e.tags.iter().any(|t| t == "poi"), "entrance tagged for the importer");
    }
    // Mouths cut to a side face are oriented (non-identity yaw) so a placed
    // door faces outward.
    assert!(
        entrances
            .iter()
            .any(|n| n.transform.rotation.angle_between(glam::Quat::IDENTITY) > 1e-3),
        "at least one entrance should be rotated to face its wall"
    );
}

#[test]
fn cave_per_band_entrances_route_to_correct_bands() {
    // With levels=2 and entrances=[2, 0], both mouths land on band 0 (bottom).
    // With levels=2 and entrances=[0, 2], both land on band 1 (top).
    // The two sets must have distinct Y heights, confirming band routing.
    let make = |entrances: &str| {
        lower_src(&format!(
            r#"cave "banded" (seed=5,size=[28,14,28],chambers=10,levels=2,resolution=48,entrances={entrances})"#
        ))
    };
    let lo = make("[2, 0]");
    let hi = make("[0, 2]");

    let poi_ys = |g: &SceneGraph| -> Vec<f32> {
        g.nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some("entrance"))
            .map(|n| n.transform.translation.y)
            .collect()
    };
    let lo_ys = poi_ys(&lo);
    let hi_ys = poi_ys(&hi);

    assert_eq!(lo_ys.len(), 2, "entrances=[2,0] should place 2 mouths (both on band 0): {lo_ys:?}");
    assert_eq!(hi_ys.len(), 2, "entrances=[0,2] should place 2 mouths (both on band 1): {hi_ys:?}");

    // Band 1 (top) mouths must sit higher than band 0 (bottom) mouths.
    let lo_max_y: f32 = lo_ys.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let hi_min_y: f32 = hi_ys.iter().copied().fold(f32::INFINITY, f32::min);
    assert!(
        hi_min_y > lo_max_y,
        "band-1 entrances (min Y {hi_min_y:.2}) should be above band-0 (max Y {lo_max_y:.2})"
    );

    // Zero count on all bands → no mouths at all.
    let none = make("[0, 0]");
    let none_ys = poi_ys(&none);
    assert!(none_ys.is_empty(), "entrances=[0,0] should emit no entrance POIs");
}

#[test]
fn cave_debug_show_poi_visualizes_markers() {
    let base = r#"
cave "poi_cave" (
  seed=7,
  size=[28, 14, 28],
  chambers=8,
  levels=2,
  resolution=48,
  columns=3,
  mushrooms=5,
)
"#;
    // Without the flag, markers are geometry-free.
    let plain = lower_src(base);
    let shrooms_plain: Vec<_> = plain
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("mushroom_spot"))
        .collect();
    assert!(shrooms_plain.iter().all(|n| n.mesh.is_none()));

    // With debug_show_poi=1, every marker gets a small mesh + a debug material
    // colour-coded by kind, but still no collider (it's a viewing aid only).
    let dbg = lower_src(&base.replace("mushrooms=5,", "mushrooms=5, debug_show_poi=1,"));
    let markers: Vec<_> = dbg
        .nodes
        .iter()
        .filter(|n| n.tags.iter().any(|t| t == "poi"))
        .collect();
    assert!(!markers.is_empty(), "expected POI markers");
    for m in &markers {
        assert!(m.mesh.is_some(), "debug markers carry a sphere mesh");
        assert!(m.collider.is_none(), "debug markers never get a collider");
        let kind = m.role.as_deref().expect("marker has a role");
        let want = dbg
            .find_material(&format!("cave_poi_{kind}"))
            .unwrap_or_else(|| panic!("debug material for {kind}"));
        assert_eq!(m.material, Some(want), "markers bind their per-kind debug material");
    }
    // Different kinds use different materials (colour-coded groups).
    let shroom = markers.iter().find(|n| n.role.as_deref() == Some("mushroom_spot"));
    let column = markers.iter().find(|n| n.role.as_deref() == Some("column_base"));
    if let (Some(s), Some(c)) = (shroom, column) {
        assert_ne!(s.material, c.material, "POI groups should be colour-coded distinctly");
    }
}

#[test]
fn cave_lod_scale_reduces_triangle_count() {
    let base = r#"
cave "lod" (
  seed=8,
  size=[24, 12, 24],
  chambers=6,
  resolution=96,
  stalagmites=6,
)
"#;
    let full = lower_src(base);
    let low = lower_src(&base.replace("resolution=96,", "resolution=96, lod_scale=0.4,"));
    let tris = |g: &SceneGraph| -> usize {
        g.nodes
            .iter()
            .filter_map(|n| n.mesh.as_ref())
            .map(|m| m.indices.len())
            .sum()
    };
    assert!(
        tris(&low) < tris(&full),
        "lod_scale=0.4 should cut triangles ({} !< {})",
        tris(&low),
        tris(&full)
    );
    // Layout is unchanged: the same number of stalagmites land regardless of LOD.
    assert_eq!(count_role(&full, "stalagmite"), count_role(&low, "stalagmite"));
}

#[test]
fn cave_honours_file_global_lod_scale_directive() {
    // The studio LOD slider writes a top-level `lod_scale (value=…)` directive
    // rather than the cave node's own attr; the cave must still respond to it.
    let base = r#"
cave "lod" (
  seed=8,
  size=[24, 12, 24],
  chambers=6,
  resolution=96,
  stalagmites=6,
)
"#;
    let full = lower_src(base);
    let low = lower_src(&format!("lod_scale (value=0.4)\n{base}"));
    let tris = |g: &SceneGraph| -> usize {
        g.nodes
            .iter()
            .filter_map(|n| n.mesh.as_ref())
            .map(|m| m.indices.len())
            .sum()
    };
    assert!(
        tris(&low) < tris(&full),
        "top-level lod_scale=0.4 should cut cave triangles ({} !< {})",
        tris(&low),
        tris(&full)
    );
}

#[test]
fn cave_colliders_none_leaves_everything_collider_free() {
    let src = r#"
cave "smooth" (
  seed=5,
  size=[22, 10, 22],
  chambers=5,
  resolution=40,
  stalagmites=4,
  columns=2,
  colliders="none",
)
"#;
    let g = lower_src(src);
    assert!(
        g.nodes.iter().all(|n| n.collider.is_none()),
        "colliders=\"none\" should leave every cave node collider-free"
    );
}

#[test]
fn cave_colliders_shell_only_collides_the_rock() {
    let src = r#"
cave "shellonly" (
  seed=5,
  size=[22, 10, 22],
  chambers=5,
  resolution=40,
  stalagmites=4,
  columns=2,
  colliders="shell",
)
"#;
    let g = lower_src(src);
    let rock = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert!(rock.collider.is_some(), "shell must keep its collider");
    // Decorations are walk-through under `shell`.
    for n in &g.nodes {
        if matches!(n.role.as_deref(), Some("stalagmite") | Some("column")) {
            assert!(
                n.collider.is_none(),
                "decorations should be collider-free under colliders=\"shell\""
            );
        }
    }
}

#[test]
fn cave_water_collider_opts_water_in() {
    let base = r#"
cave "spring" (seed=1, size=[18, 8, 18], chambers=4, resolution=40, pools=2)
"#;
    // Default: water is wadeable (no collider).
    let plain = lower_src(base);
    assert!(plain
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("pool"))
        .all(|n| n.collider.is_none()));

    // Opt in: pools get a trimesh collider, shell still collides.
    let solid = lower_src(&base.replace("pools=2)", "pools=2, water_collider=1)"));
    assert!(solid
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("pool"))
        .all(|n| n.collider.is_some()));
    let rock = solid
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .unwrap();
    assert!(rock.collider.is_some(), "rock still collides with water_collider=1");
}

#[test]
fn cave_water_uses_default_water_material() {
    let src = r#"
cave "spring" (seed=1, size=[18, 8, 18], chambers=4, resolution=40, pools=2)
"#;
    let g = lower_src(src);
    let water = g.find_material("cave_water").expect("default water material");
    assert_eq!(
        g.materials[water.0 as usize].shader,
        mogen_core::MaterialShader::Water,
        "cave water should use the animated water shader"
    );
    let pools = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("pool"))
        .count();
    assert_eq!(pools, 2);
    assert!(g
        .nodes
        .iter()
        .any(|n| n.role.as_deref() == Some("pool") && n.material == Some(water)));
}

#[test]
fn cave_rock_has_valid_triplanar_uvs() {
    let g = lower_src(BASIC);
    let mesh = g
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("cave_rock"))
        .and_then(|n| n.mesh.as_ref())
        .expect("cave_rock mesh");

    // After per-triangle unwelding, UVs and positions must be parallel.
    assert!(
        mesh.has_uvs(),
        "cave rock must have UV data (triplanar_uvs not applied?)"
    );

    // Full unweld: every triangle gets its own 3 vertices, so vertex count is
    // always a multiple of 3 and equals 3 × triangle-count.
    assert_eq!(
        mesh.positions.len() % 3,
        0,
        "unwelded rock should have positions.len() divisible by 3"
    );
    assert_eq!(
        mesh.positions.len(),
        mesh.indices.len(),
        "unwelded rock: vertex count must equal index count"
    );

    // Per-triangle UV-plane consistency: the two UVs produced by projecting
    // a shared vertex from the same face normal must be equal to within
    // floating-point rounding.  Verify by re-deriving each face's dominant
    // axis and confirming all three vertex UVs are compatible.
    let inv = 1.0_f32 / 5.0; // ROCK_UV_TILE = 5.0
    for tri in mesh.indices.chunks_exact(3) {
        let [i0, i1, i2] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let p = |i: usize| glam::Vec3::from(mesh.positions[i]);
        let (p0, p1, p2) = (p(i0), p(i1), p(i2));
        let face_n = (p1 - p0).cross(p2 - p0).normalize_or_zero();
        let (ax, ay, az) = (face_n.x.abs(), face_n.y.abs(), face_n.z.abs());
        // What the project closure would have produced for each vertex:
        let expected = |pt: glam::Vec3| -> [f32; 2] {
            let (u, v) = if ay >= ax && ay >= az {
                (pt.x, pt.z)
            } else if ax >= az {
                (pt.z, pt.y)
            } else {
                (pt.x, pt.y)
            };
            [u * inv, v * inv]
        };
        for (i, pt) in [(i0, p0), (i1, p1), (i2, p2)] {
            let got = mesh.uvs[i];
            let want = expected(pt);
            assert!(
                (got[0] - want[0]).abs() < 1e-5 && (got[1] - want[1]).abs() < 1e-5,
                "vertex {i} UV mismatch: got {:?}, want {:?}",
                got,
                want
            );
        }
    }
}
