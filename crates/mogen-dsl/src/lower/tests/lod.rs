use super::*;
use crate::lower::*;

#[test]
fn lod_scale_halves_default_segment_count() {
    let baseline = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let scaled = lower_src(r#"lod_scale (value=0.5) scene { sphere "s" (radius=0.5) }"#);
    let base_verts = find_mesh_node(&baseline, "s").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "s").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts < base_verts,
        "lod_scale=0.5 should reduce sphere vert count (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn lod_scale_doubles_default_segment_count() {
    let baseline = lower_src(r#"scene { cylinder "c" (radius=0.5, height=1) }"#);
    let scaled = lower_src(
        r#"lod_scale (value=2) scene { cylinder "c" (radius=0.5, height=1) }"#,
    );
    let base_verts = find_mesh_node(&baseline, "c").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "c").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts > base_verts,
        "lod_scale=2 should increase cylinder vert count (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn lod_scale_also_scales_explicit_segments() {
    // Explicit per-primitive segment counts ride the global multiplier
    // too — otherwise dense surface primitives (heightfield, curved_plane,
    // surf-style waves) become LOD-inert. The clamp keeps the count above
    // each primitive's minimum so circles still close.
    let baseline = lower_src(r#"scene { sphere "s" (radius=0.5, rings=16, segments=24) }"#);
    let scaled = lower_src(
        r#"lod_scale (value=0.25) scene { sphere "s" (radius=0.5, rings=16, segments=24) }"#,
    );
    let base_verts = find_mesh_node(&baseline, "s").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "s").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts < base_verts,
        "lod_scale=0.25 should reduce explicit rings=16/segments=24 (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn lod_scale_scales_explicit_heightfield_grid() {
    // Regression for examples/nature/heightfield_terrain.mog and wave_water.mog —
    // both pin segments_u/segments_v and would otherwise ignore lod_scale.
    let baseline = lower_src(
        r#"scene { heightfield "h" (size=[6, 6], segments_u=64, segments_v=64) }"#,
    );
    let halved = lower_src(
        r#"lod_scale (value=0.5) scene { heightfield "h" (size=[6, 6], segments_u=64, segments_v=64) }"#,
    );
    let doubled = lower_src(
        r#"lod_scale (value=2) scene { heightfield "h" (size=[6, 6], segments_u=64, segments_v=64) }"#,
    );
    let verts = |g: &SceneGraph| find_mesh_node(g, "h").mesh.as_ref().unwrap().positions.len();
    assert!(verts(&halved) < verts(&baseline));
    assert!(verts(&doubled) > verts(&baseline));
}

#[test]
fn lod_scale_clamps_explicit_segments_to_min() {
    // A vanishing lod_scale must not silently destroy a primitive — every
    // segment count is floored at its per-primitive minimum (cylinder=3).
    let g = lower_src(
        r#"lod_scale (value=0.01) scene { cylinder "c" (radius=0.5, height=1, segments=24) }"#,
    );
    let mesh = find_mesh_node(&g, "c").mesh.as_ref().unwrap();
    // Cylinder side ring + caps; with segments=3 it's still a closed solid.
    assert!(
        !mesh.positions.is_empty() && !mesh.indices.is_empty(),
        "cylinder under lod_scale=0.01 must still produce geometry"
    );
}

#[test]
fn lod_scale_steps_icosphere_subdivisions() {
    // Icosphere triangle count is 20 * 4^subdivisions. Default subdivisions=2
    // → 320 tris. lod_scale=2 → subdivisions=3 → 1280 tris. lod_scale=0.5 →
    // subdivisions=1 → 80 tris.
    let base = lower_src(r#"scene { icosphere "i" (radius=0.5) }"#);
    let up = lower_src(r#"lod_scale (value=2) scene { icosphere "i" (radius=0.5) }"#);
    let down = lower_src(r#"lod_scale (value=0.5) scene { icosphere "i" (radius=0.5) }"#);
    let tris = |g: &SceneGraph| find_mesh_node(g, "i").mesh.as_ref().unwrap().indices.len() / 3;
    assert_eq!(tris(&base), 320);
    assert_eq!(tris(&up), 1280);
    assert_eq!(tris(&down), 80);
}

#[test]
fn lod_scale_default_keeps_existing_vertex_counts() {
    // No `lod_scale` directive should leave every default mesh untouched —
    // a regression here would silently change every existing .mog's output.
    let g = lower_src(r#"scene { sphere "s" (radius=0.5) cylinder "c" (radius=0.5, height=1) }"#);
    let s_verts = find_mesh_node(&g, "s").mesh.as_ref().unwrap().positions.len();
    let c_verts = find_mesh_node(&g, "c").mesh.as_ref().unwrap().positions.len();
    // Sphere default rings=16, segments=24 → 17 * 25 = 425 verts (one extra
    // ring + one extra segment for the seam); cylinder default segments=24
    // → 2 * (24 + 1) side verts + 2 * (24 + 1) cap-fan verts + 2 cap centres
    // = 102 verts. These exact counts depend on the mesh builder — the
    // test asserts them so a future LOD-scale change doesn't drift defaults.
    assert_eq!(s_verts, 425);
    assert_eq!(c_verts, 102);
}

#[test]
fn implicit_curved_primitive_density_tracks_authored_local_size() {
    let g = lower_src(
        r#"scene {
            sphere "indent" (radius=0.015)
            sphere "ball" (radius=0.5)
            sphere "dome" (radius=10)
            icosphere "ico_indent" (radius=0.015)
            icosphere "ico_dome" (radius=10)
        }"#,
    );
    let tris = |name: &str| {
        find_mesh_node(&g, name)
            .mesh
            .as_ref()
            .unwrap()
            .indices
            .len()
            / 3
    };
    assert!(
        tris("indent") < tris("ball"),
        "a 15 mm indent still pays the unit sphere's tessellation"
    );
    assert!(
        tris("ball") < tris("dome"),
        "a large dome did not gain detail from authored size"
    );
    assert!(
        tris("ico_indent") < tris("ico_dome"),
        "icosphere subdivisions remained size-blind"
    );
}

#[test]
fn implicit_size_aware_radial_counts_preserve_cardinal_extents() {
    let g = lower_src(
        r#"scene {
            sphere "sphere" (radius=0.25)
            cylinder "cylinder" (radius=0.25, height=1)
        }"#,
    );
    for name in ["sphere", "cylinder"] {
        let mesh = find_mesh_node(&g, name).mesh.as_ref().unwrap();
        let min_x = mesh.positions.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
        let max_x = mesh.positions.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        let min_z = mesh.positions.iter().map(|p| p[2]).fold(f32::INFINITY, f32::min);
        let max_z = mesh.positions.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
        assert_eq!([min_x, max_x, min_z, max_z], [-0.25, 0.25, -0.25, 0.25]);
    }
}

#[test]
fn explicit_segments_override_size_aware_defaults_exactly() {
    let g = lower_src(
        r#"scene {
            sphere "small" (radius=0.015, rings=7, segments=11)
            sphere "large" (radius=10, rings=7, segments=11)
        }"#,
    );
    let tris = |name: &str| {
        find_mesh_node(&g, name)
            .mesh
            .as_ref()
            .unwrap()
            .indices
            .len()
            / 3
    };
    assert_eq!(
        tris("small"),
        tris("large"),
        "authored rings/segments stopped being exact overrides"
    );
}

#[test]
fn instance_scale_does_not_change_tessellation_identity() {
    let g = lower_src(
        r#"scene {
            sphere "plain" (radius=0.2)
            sphere "placed" (radius=0.2, scale=[100, 1, 0.5])
        }"#,
    );
    let verts = |name: &str| {
        find_mesh_node(&g, name)
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len()
    };
    assert_eq!(
        verts("plain"),
        verts("placed"),
        "placement scale leaked into authored mesh density"
    );
}

#[test]
fn per_node_lod_doubles_segment_count_on_marked_subtree() {
    // `lod=2.0` on a single primitive doubles its default segment count
    // (matches the behaviour of `lod_scale (value=2)` but scoped to that
    // subtree only — see lod.rs::LodMultiplierGuard).
    let baseline = lower_src(r#"scene { cylinder "c" (radius=0.5, height=1) }"#);
    let scaled = lower_src(r#"scene { cylinder "c" (radius=0.5, height=1, lod=2) }"#);
    let base_verts = find_mesh_node(&baseline, "c").mesh.as_ref().unwrap().positions.len();
    let scaled_verts = find_mesh_node(&scaled, "c").mesh.as_ref().unwrap().positions.len();
    assert!(
        scaled_verts > base_verts,
        "per-node lod=2 should increase cylinder vert count (base={base_verts}, scaled={scaled_verts})"
    );
}

#[test]
fn per_node_lod_does_not_leak_to_siblings() {
    // The multiplier guard is RAII-scoped to the marked subtree. A `lod=2`
    // group must not boost a sibling that lives outside it.
    let g = lower_src(
        r#"scene {
            group "hi" (lod=2) { sphere "s" (radius=0.5) }
            sphere "lo" (radius=0.5)
        }"#,
    );
    let hi = find_mesh_node(&g, "s").mesh.as_ref().unwrap().positions.len();
    let lo = find_mesh_node(&g, "lo").mesh.as_ref().unwrap().positions.len();
    let baseline = lower_src(r#"scene { sphere "b" (radius=0.5) }"#);
    let base = find_mesh_node(&baseline, "b").mesh.as_ref().unwrap().positions.len();
    assert!(hi > base, "lod=2 group should boost child sphere (hi={hi}, base={base})");
    assert_eq!(lo, base, "sibling outside the lod=2 group must use baseline detail");
}

#[test]
fn per_node_lod_compounds_with_global_lod_scale() {
    // `lod=2.0` on top of `lod_scale (value=0.5)` cancels out — effective
    // multiplier is 1.0, so the marked subtree returns to the default
    // vertex count even though the file's global setting is 0.5.
    let baseline = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let compound = lower_src(
        r#"lod_scale (value=0.5) scene { sphere "s" (radius=0.5, lod=2) }"#,
    );
    let base_verts = find_mesh_node(&baseline, "s").mesh.as_ref().unwrap().positions.len();
    let compound_verts = find_mesh_node(&compound, "s").mesh.as_ref().unwrap().positions.len();
    assert_eq!(
        base_verts, compound_verts,
        "lod=2 should cancel a global lod_scale=0.5 (base={base_verts}, compound={compound_verts})"
    );
}

#[test]
fn non_finite_authored_lod_values_are_ignored() {
    use crate::ast::Value;
    use crate::lower::lod::{collect_origin_lods, extract_lod_scale};

    let baseline = lower_src(r#"sphere "s""#);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut ast = crate::parse(r#"lod_scale (value=1) sphere "s" (lod=1)"#).unwrap();
        ast[0].attrs[0].1 = Value::Number(value);
        ast[1].attrs[0].1 = Value::Number(value);
        assert_eq!(extract_lod_scale(&ast), 1.0);
        let graph = lower(&ast).unwrap();
        assert_eq!(
            find_mesh_node(&graph, "s").mesh.as_ref().unwrap().positions,
            find_mesh_node(&baseline, "s")
                .mesh
                .as_ref()
                .unwrap()
                .positions,
        );

        let _origins = LodByOriginGuard::fresh();
        let _scale = LodScaleGuard::set(2.0);
        let origin = std::path::Path::new("parts.mog");
        ast[0].origin = Some(origin.into());
        collect_origin_lods(&ast);
        let _origin = crate::lower::lod::LodOriginScaleGuard::for_origin(Some(origin));
        assert_eq!(LOD_SCALE.with(|scale| scale.get()), 1.0);
    }
}

#[test]
fn nested_lowering_resets_lod_and_restores_it_even_after_failure() {
    use crate::lower::lod::{current_lod_scale, LodMultiplierGuard};

    let node = crate::parse("group (lod=2)").unwrap().remove(0);
    let _scale = LodScaleGuard::set(0.5);
    let _mult = LodMultiplierGuard::for_node(&node);
    assert_eq!(current_lod_scale(), 1.0);

    let nested = lower_src(r#"sphere "s""#);
    assert_eq!(
        find_mesh_node(&nested, "s")
            .mesh
            .as_ref()
            .unwrap()
            .positions
            .len(),
        425
    );
    assert!(lower(&crate::parse("group (lod=3) { unknown_primitive }").unwrap()).is_err());
    assert_eq!(current_lod_scale(), 1.0);
}
