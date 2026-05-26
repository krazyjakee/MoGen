use super::*;
use crate::lower::*;

#[test]
fn deform_noise_changes_mesh_positions() {
    // Same primitive built once plain and once with noise+seed should differ.
    let plain = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let noisy = lower_src(r#"scene { sphere "s" (radius=0.5, noise=0.4, seed=7) }"#);
    let p = find_mesh_node(&plain, "s").mesh.as_ref().unwrap();
    let n = find_mesh_node(&noisy, "s").mesh.as_ref().unwrap();
    // Auto-bumped tessellation: noisy mesh should have more verts than plain.
    assert!(
        n.positions.len() > p.positions.len(),
        "expected noise to bump tessellation: plain={}, noisy={}",
        p.positions.len(),
        n.positions.len()
    );
    // Per-vertex positions necessarily differ from the plain unit sphere.
    let plain_radii: f32 = p.positions.iter()
        .map(|q| (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt())
        .sum::<f32>() / p.positions.len() as f32;
    let noisy_radii: f32 = n.positions.iter()
        .map(|q| (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt())
        .sum::<f32>() / n.positions.len() as f32;
    // Plain sphere radii average ~0.5; noise perturbs them but the mean stays
    // near 0.5 (zero-mean random shift). What we want is that the per-vertex
    // STD is non-trivial.
    let noisy_std: f32 = (n.positions.iter()
        .map(|q| {
            let r = (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt();
            (r - noisy_radii).powi(2)
        })
        .sum::<f32>() / n.positions.len() as f32).sqrt();
    let plain_std: f32 = (p.positions.iter()
        .map(|q| {
            let r = (q[0].powi(2) + q[1].powi(2) + q[2].powi(2)).sqrt();
            (r - plain_radii).powi(2)
        })
        .sum::<f32>() / p.positions.len() as f32).sqrt();
    assert!(
        noisy_std > plain_std + 0.001,
        "expected noise to widen radius distribution: plain_std={plain_std}, noisy_std={noisy_std}"
    );
}

#[test]
fn deform_seed_determinism() {
    // Same source compiled twice should produce byte-identical positions.
    let a = lower_src(r#"scene { box "b" (size=[1,1,1], noise=0.3, seed=42) }"#);
    let b = lower_src(r#"scene { box "b" (size=[1,1,1], noise=0.3, seed=42) }"#);
    let pa = find_mesh_node(&a, "b").mesh.as_ref().unwrap();
    let pb = find_mesh_node(&b, "b").mesh.as_ref().unwrap();
    assert_eq!(pa.positions, pb.positions);
}

#[test]
fn deform_taper_shrinks_top_of_cylinder() {
    let g = lower_src(
        r#"scene { cylinder "c" (radius=0.5, height=1.0, taper=0.5) }"#,
    );
    let mesh = find_mesh_node(&g, "c").mesh.as_ref().unwrap();
    let mut top_max = 0.0_f32;
    let mut bot_max = 0.0_f32;
    for p in &mesh.positions {
        let r = (p[0] * p[0] + p[2] * p[2]).sqrt();
        if p[1] > 0.4 { top_max = top_max.max(r); }
        if p[1] < -0.4 { bot_max = bot_max.max(r); }
    }
    assert!((top_max - 0.25).abs() < 1e-3, "top radius should be ~0.25, got {top_max}");
    assert!((bot_max - 0.5).abs() < 1e-3, "bottom radius should be ~0.5, got {bot_max}");
}

#[test]
fn mirror_bakes_reflection_into_subtree_so_chain_stays_positive_det() {
    // Regression: a `mirror (axis=x)` used to leave its second instance with
    // a `scale=(-1,1,1)` on the wrapper, which gave it a negative-determinant
    // world transform. Renderers that don't reverse front-face winding for
    // negative-det chains (and most glTF importers in practice) drew the
    // mirrored copy backface-culled — the `gaming_chair.mog` armpad bug.
    let g = lower_src(
        r#"
        scene {
          mirror "pair" (axis=x) {
            box "leaf" (pos=[0.5, 0.0, 0.0], size=[0.2, 0.2, 0.2])
          }
        }
        "#,
    );

    // Every node's local scale must be positive after the bake.
    for n in &g.nodes {
        let s = n.transform.scale;
        assert!(
            s.x * s.y * s.z > 0.0,
            "node `{}` has non-positive-det scale {:?} after mirror bake",
            n.name,
            s
        );
    }

    // Find the original (`pair_0`) and mirrored (`pair_1`) leaves and confirm
    // the second one has its translation flipped on x and its mesh winding
    // reversed relative to the first.
    let pair_0_leaf = g
        .nodes
        .iter()
        .find(|n| n.name == "leaf"
            && n.parent.is_some()
            && g.nodes[n.parent.unwrap().0 as usize].name == "pair_0")
        .expect("pair_0/leaf");
    let pair_1_leaf = g
        .nodes
        .iter()
        .find(|n| n.name == "leaf"
            && n.parent.is_some()
            && g.nodes[n.parent.unwrap().0 as usize].name == "pair_1")
        .expect("pair_1/leaf");

    assert!((pair_0_leaf.transform.translation.x - 0.5).abs() < 1e-5);
    assert!((pair_1_leaf.transform.translation.x + 0.5).abs() < 1e-5);

    let m0 = pair_0_leaf.mesh.as_ref().unwrap();
    let m1 = pair_1_leaf.mesh.as_ref().unwrap();
    assert_eq!(m0.indices.len(), m1.indices.len());
    // Per-triangle winding flipped: m1 swaps indices 1 and 2 of every tri
    // (and x-flips positions/normals).
    for (a, b) in m0.indices.chunks_exact(3).zip(m1.indices.chunks_exact(3)) {
        assert_eq!(a[0], b[0]);
        assert_eq!(a[1], b[2]);
        assert_eq!(a[2], b[1]);
    }
    for (p0, p1) in m0.positions.iter().zip(m1.positions.iter()) {
        assert!((p0[0] + p1[0]).abs() < 1e-5, "x should be negated");
        assert!((p0[1] - p1[1]).abs() < 1e-5);
        assert!((p0[2] - p1[2]).abs() < 1e-5);
    }
    for (n0, n1) in m0.normals.iter().zip(m1.normals.iter()) {
        assert!((n0[0] + n1[0]).abs() < 1e-5, "normal x should be negated");
    }
}

#[test]
fn mirror_flip_bind_swaps_lr_suffix_for_mirrored_copy() {
    // `mirror (axis=x, flip_bind=1)` over a mesh bound to `shoulder_l`
    // should produce two scene nodes, the original still bound to
    // `shoulder_l` and the mirrored copy rebound to `shoulder_r`. The flag
    // exists so symmetric humanoid accessories (sleeves, cuffs, shoes) can
    // be authored once and follow both bones — string-typed module params
    // aren't supported, so without `flip_bind` you'd hand-author each side.
    let g = lower_src(
        r#"
        scene {
          skeleton "rig" {
            bone "shoulder_l" (pos=[ 0.2, 1.4, 0])
            bone "shoulder_r" (pos=[-0.2, 1.4, 0])
          }
          mirror "arms" (axis=x, flip_bind=1) {
            box "sleeve" (pos=[0.2, 1.4, 0], size=[0.1, 0.2, 0.1],
                          skin="rig", bind="shoulder_l")
          }
        }
        "#,
    );

    let l_idx = g.find_node("shoulder_l").unwrap().0 as u16;
    let r_idx = g.find_node("shoulder_r").unwrap().0 as u16;
    let skin = g.find_skin("rig").unwrap();
    let l_joint = g.skins[skin.0 as usize]
        .joints
        .iter()
        .position(|j| j.0 as u16 == l_idx)
        .unwrap() as u16;
    let r_joint = g.skins[skin.0 as usize]
        .joints
        .iter()
        .position(|j| j.0 as u16 == r_idx)
        .unwrap() as u16;

    let sleeves: Vec<&mogen_core::SceneNode> = g
        .nodes
        .iter()
        .filter(|n| n.name == "sleeve")
        .collect();
    assert_eq!(sleeves.len(), 2, "mirror should produce two `sleeve` copies");

    // Both copies are skinned. The unmirrored copy binds to shoulder_l;
    // the mirrored copy binds to shoulder_r via flip_bind.
    let mut bound_to_l = 0;
    let mut bound_to_r = 0;
    for s in &sleeves {
        let m = s.mesh.as_ref().expect("sleeve mesh");
        assert_eq!(s.skin, Some(skin), "sleeve should be skinned");
        for j in &m.joints {
            assert_eq!(j[1..], [0, 0, 0], "rigid bind expected: only slot 0 used");
            if j[0] == l_joint {
                bound_to_l += 1;
            } else if j[0] == r_joint {
                bound_to_r += 1;
            } else {
                panic!("unexpected joint index {} in sleeve", j[0]);
            }
        }
    }
    assert!(bound_to_l > 0, "unmirrored copy must still bind to shoulder_l");
    assert!(bound_to_r > 0, "mirrored copy must rebind to shoulder_r");
}

#[test]
fn bend_z_range_only_bends_tip() {
    // Cylinder from y=-0.5 to y=0.5; bend_z bends along Y. With range
    // [0.75, 1.0] the lower 75 % of the column stays at x≈0; only the upper
    // 25 % gets perturbed. Compare against the unranged form so we can see
    // the lower ring stayed put while the upper ring moved.
    let baseline = lower_src(
        r#"scene { cylinder "c" (radius=0.1, height=1.0, segments=8) }"#,
    );
    let ranged = lower_src(
        r#"scene { cylinder "c" (radius=0.1, height=1.0, segments=8, bend_z=60, bend_z_range=[0.75, 1.0]) }"#,
    );
    let base_mesh = find_mesh_node(&baseline, "c").mesh.as_ref().unwrap();
    let bent_mesh = find_mesh_node(&ranged, "c").mesh.as_ref().unwrap();
    assert_eq!(base_mesh.positions.len(), bent_mesh.positions.len());
    let mut max_base_shift = 0.0_f32;
    let mut max_tip_shift = 0.0_f32;
    for (b, p) in base_mesh.positions.iter().zip(bent_mesh.positions.iter()) {
        let dx = p[0] - b[0];
        let dy = p[1] - b[1];
        let dz = p[2] - b[2];
        let shift = (dx * dx + dy * dy + dz * dz).sqrt();
        // Use the unbent y to bucket: vertices originally below the column
        // midpoint are "base"; above are "tip". (After bending the tip
        // slides downward, so post-bend `p[1]` would mis-bucket.)
        if b[1] < 0.0 {
            max_base_shift = max_base_shift.max(shift);
        } else {
            max_tip_shift = max_tip_shift.max(shift);
        }
    }
    assert!(
        max_base_shift < 1e-3,
        "lower half should stay put with smoothstep ramp at 0.75 (got {max_base_shift})"
    );
    assert!(
        max_tip_shift > 0.3,
        "upper half should bend appreciably (got {max_tip_shift})"
    );
}

#[test]
fn taper_range_leaves_lower_half_unscaled() {
    // Sphere has dense Y rings, so we can probe both the unranged taper=0.5
    // result and the ranged form at the same y. With taper_range=[0.5, 1.0]
    // the lower hemisphere stays pristine (weight=0) and the upper
    // hemisphere ramps in via smoothstep.
    let plain = lower_src(r#"scene { sphere "s" (radius=0.5) }"#);
    let ranged = lower_src(
        r#"scene { sphere "s" (radius=0.5, taper=0.5, taper_range=[0.5, 1.0]) }"#,
    );
    let p = find_mesh_node(&plain, "s").mesh.as_ref().unwrap();
    let r = find_mesh_node(&ranged, "s").mesh.as_ref().unwrap();
    assert_eq!(p.positions.len(), r.positions.len());

    let mut max_lower_xz_diff = 0.0_f32;
    let mut max_upper_xz_diff = 0.0_f32;
    for (pl, ra) in p.positions.iter().zip(r.positions.iter()) {
        let xz_diff = ((ra[0] - pl[0]).powi(2) + (ra[2] - pl[2]).powi(2)).sqrt();
        if pl[1] < -0.05 {
            max_lower_xz_diff = max_lower_xz_diff.max(xz_diff);
        } else if pl[1] > 0.3 {
            max_upper_xz_diff = max_upper_xz_diff.max(xz_diff);
        }
    }
    assert!(
        max_lower_xz_diff < 1e-4,
        "lower hemisphere should match the un-tapered sphere (got {max_lower_xz_diff})"
    );
    assert!(
        max_upper_xz_diff > 0.05,
        "upper hemisphere should be tapered noticeably (got {max_upper_xz_diff})"
    );
}

#[test]
fn noise_range_only_roughens_top() {
    // Tall box; with noise_range=[0.7, 1.0] only the top 30 % gets bumpy.
    // The bottom face vertices lie at y=-0.5; their (x, z) should match the
    // pristine box — within float noise of zero.
    let g = lower_src(
        r#"scene { box "b" (size=[1, 1, 1], noise=0.5, seed=11, noise_range=[0.7, 1.0]) }"#,
    );
    let mesh = find_mesh_node(&g, "b").mesh.as_ref().unwrap();
    let mut max_base_dx = 0.0_f32;
    let mut max_top_dx = 0.0_f32;
    for p in &mesh.positions {
        // Box default has corners at ±0.5 on every axis; subtracting the
        // sign-aware nominal corner gives the displacement magnitude.
        let dx = p[0].abs() - 0.5;
        let dz = p[2].abs() - 0.5;
        let max_xy = dx.abs().max(dz.abs());
        if p[1] < -0.4 {
            max_base_dx = max_base_dx.max(max_xy);
        } else if p[1] > 0.4 {
            max_top_dx = max_top_dx.max(max_xy);
        }
    }
    assert!(
        max_base_dx < 1e-3,
        "bottom face should stay at exactly ±0.5 (got {max_base_dx})"
    );
    assert!(
        max_top_dx > 0.01,
        "top face should be perturbed by noise (got {max_top_dx})"
    );
}

#[test]
fn range_reversed_endpoints_are_normalised() {
    // [1.0, 0.5] is a user typo for [0.5, 1.0]. set_range sorts the pair so
    // the kernel sees a soft ramp instead of a hard step at 1.0 (which would
    // give zero deformation everywhere because t < 1.0 everywhere except
    // the topmost row).
    let a = lower_src(
        r#"scene { cylinder "c" (radius=0.1, height=1.0, segments=8, bend_z=60, bend_z_range=[0.5, 1.0]) }"#,
    );
    let b = lower_src(
        r#"scene { cylinder "c" (radius=0.1, height=1.0, segments=8, bend_z=60, bend_z_range=[1.0, 0.5]) }"#,
    );
    let pa = find_mesh_node(&a, "c").mesh.as_ref().unwrap();
    let pb = find_mesh_node(&b, "c").mesh.as_ref().unwrap();
    assert_eq!(pa.positions.len(), pb.positions.len());
    for (p, q) in pa.positions.iter().zip(pb.positions.iter()) {
        for k in 0..3 {
            assert!(
                (p[k] - q[k]).abs() < 1e-5,
                "swapped endpoints produced different geometry at axis {k}"
            );
        }
    }
}

#[test]
fn wave_deformer_displaces_dense_plane() {
    // A coarse plane has only 4 corner vertices, so to actually see the
    // wave displacement we use a dense `curved_plane` (zero bend, lots of
    // segments). The wave attribute should produce non-trivial Y movement
    // along the X axis on at least some interior vertices.
    let g = lower_src(
        r#"scene { curved_plane "water" (size=[4, 1], segments_u=32, segments_v=8,
            wave=0.15, wave_frequency=0.5, wave_axis="x") }"#,
    );
    let mesh = find_mesh_node(&g, "water").mesh.as_ref().unwrap();
    let max_y = mesh.positions.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    let min_y = mesh.positions.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    // A flat plane has y=0 everywhere; a `wave=0.15` has a peak crest of
    // ~0.15 m. A noticeable spread means the wave actually fired.
    assert!(
        max_y - min_y > 0.05,
        "wave should have produced visible Y spread: [{min_y}, {max_y}]",
    );
}

#[test]
fn wave_axis_string_attribute_lowers() {
    // `wave_axis="x"` is the canonical valid case — confirms the deformer
    // attribute is wired through the lowering path. The invalid-string
    // case (e.g. `wave_axis="diagonal"`) is currently a validator warning
    // rather than a lowering error: the deformer's `parse_axis` returns
    // `None` and lowering falls back to X, so a typo builds and ships.
    // If a future change tightens that to a hard error, add a separate
    // test asserting `lower_src(...invalid...).is_err()` here.
    let g = lower_src(
        r#"scene { curved_plane "ok" (size=[2, 1], segments_u=16, segments_v=4,
            wave=0.05, wave_axis="x") }"#,
    );
    assert!(find_mesh_node(&g, "ok").mesh.is_some());
}
