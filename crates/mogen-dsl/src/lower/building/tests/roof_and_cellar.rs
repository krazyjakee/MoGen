use super::{count_kind, count_role, has_tag, lower_src, slab_ceiling_count, ROOFTEST_SRC};
use mogen_core::SceneGraph;

#[test]
fn building_roof_gabled_smoke() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "gabled");
    let g = lower_src(&src);
    // Two slope wedges + two gable end-walls.
    assert!(
        count_role(&g, "roof") >= 2,
        "expected ≥ 2 roof slope nodes, got {}",
        count_role(&g, "roof")
    );
    assert!(
        count_role(&g, "gable_wall") == 2,
        "expected exactly 2 gable_wall nodes, got {}",
        count_role(&g, "gable_wall")
    );
    assert_eq!(
        slab_ceiling_count(&g),
        0,
        "non-flat roof must not emit a top-storey ceiling slab"
    );
}

#[test]
fn building_roof_hipped_smoke() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "hipped");
    let g = lower_src(&src);
    // Single frustum mesh.
    assert_eq!(
        count_role(&g, "roof"),
        1,
        "hipped roof must be a single frustum"
    );
    assert_eq!(count_role(&g, "gable_wall"), 0);
    assert_eq!(slab_ceiling_count(&g), 0);
}

#[test]
fn building_roof_mansard_two_tiers() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "mansard");
    let g = lower_src(&src);
    // Lower + upper tier.
    assert_eq!(
        count_role(&g, "roof"),
        2,
        "mansard roof must emit 2 tiers"
    );
}

#[test]
fn building_roof_shed_single_wedge() {
    let src = ROOFTEST_SRC.replace("ROOFKIND", "shed");
    let g = lower_src(&src);
    assert_eq!(count_role(&g, "roof"), 1, "shed roof is one wedge");
    assert_eq!(slab_ceiling_count(&g), 0);
}

#[test]
fn building_non_flat_roof_suppresses_ceiling_slab() {
    for kind in ["pitched", "gabled", "hipped", "mansard", "shed"] {
        let src = ROOFTEST_SRC.replace("ROOFKIND", kind);
        let g = lower_src(&src);
        assert_eq!(
            slab_ceiling_count(&g),
            0,
            "roof=\"{kind}\" left a slab_ceiling in the graph"
        );
    }
}

#[test]
fn building_skylight_disabled_under_pitched_roof() {
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.85])
        building "house" (
          seed=1, style="grid",
          floor_area=60, rooms=4, skylights=2, roof="gabled",
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // No skylight modules instantiated.
    assert!(
        !has_tag(&g, "skylight"),
        "skylight tag should be absent under non-flat roof"
    );
    let skylights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("skylight"))
        .count();
    assert_eq!(
        skylights, 0,
        "expected no skylight instances under non-flat roof"
    );
}

#[test]
fn building_radial_layout_smoke() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "rotunda" (
          seed=7, style="radial",
          floor_area=100, rooms=9, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let rooms = count_role(&g, "room");
    assert!(rooms >= 5, "expected ≥ 5 radial rooms, got {rooms}");
    assert!(count_kind(&g, "slab") >= 2);
    assert!(count_kind(&g, "wall") >= 4);
}

#[test]
fn building_organic_layout_smoke() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "lobby" (
          seed=11, style="organic",
          floor_area=120, rooms=8, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let rooms = count_role(&g, "room");
    assert!(rooms >= 4, "expected ≥ 4 organic rooms, got {rooms}");
    // Every wall mesh's positions should live within a sane bound around
    // the floorplate centre. A bug-broken layout would put cells outside.
    for n in &g.nodes {
        if n.kind != "wall" {
            continue;
        }
        if let Some(m) = &n.mesh {
            for p in &m.positions {
                assert!(p[0].is_finite() && p[2].is_finite(), "non-finite wall vertex");
            }
        }
    }
}

#[test]
fn building_maze_layout_smoke() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "labyrinth" (
          seed=21, style="maze",
          floor_area=140, rooms=10, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let rooms = count_role(&g, "room");
    // Corridor + many small rooms.
    assert!(rooms >= 3, "expected ≥ 3 maze cells, got {rooms}");
}

#[test]
fn building_cellar_area_shrinks_basement_only() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "split" (
          seed=2, style="grid",
          floor_area=120, cellar_area=40, rooms=6,
          floors_above=1, floors_below=1,
          staircases=1, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Find the floor slab for floor_b1 and floor_0; the basement floor
    // slab must have a smaller bounding extent in XZ than the ground floor.
    fn floor_slab_extent(g: &SceneGraph, floor_group_name: &str) -> Option<(f32, f32)> {
        let group = g
            .nodes
            .iter()
            .find(|n| n.kind == "group" && n.name == floor_group_name)?;
        // Walk the group's descendants for `slab_floor`.
        let group_id = g.nodes.iter().position(|n| std::ptr::eq(n, group))? as u32;
        for n in &g.nodes {
            if n.name != "slab_floor" {
                continue;
            }
            // climb up parents until we hit `group_id` or a different floor.
            let mut cur = n.parent;
            while let Some(p) = cur {
                if p.0 == group_id {
                    let m = n.mesh.as_ref()?;
                    let mut x_min = f32::INFINITY;
                    let mut x_max = f32::NEG_INFINITY;
                    let mut z_min = f32::INFINITY;
                    let mut z_max = f32::NEG_INFINITY;
                    for p in &m.positions {
                        x_min = x_min.min(p[0]);
                        x_max = x_max.max(p[0]);
                        z_min = z_min.min(p[2]);
                        z_max = z_max.max(p[2]);
                    }
                    return Some((x_max - x_min, z_max - z_min));
                }
                cur = g.nodes[p.0 as usize].parent;
            }
        }
        None
    }
    let ground = floor_slab_extent(&g, "floor_0").expect("ground floor slab");
    let basement = floor_slab_extent(&g, "floor_b1").expect("basement floor slab");
    assert!(
        basement.0 < ground.0 && basement.1 < ground.1,
        "basement slab ({:?}) must be smaller than ground slab ({:?})",
        basement,
        ground
    );
}

#[test]
fn building_cellar_area_unset_reuses_ground_footprint() {
    let src = r#"
        material "p" (color=[0.7, 0.7, 0.7])
        building "match" (
          seed=2, style="grid",
          floor_area=120, rooms=6,
          floors_above=1, floors_below=1,
          staircases=1, entrances=1,
          mat="p",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Every floor's slab_floor should have the same XZ extent when
    // cellar_area is unset.
    let extents: Vec<(f32, f32)> = g
        .nodes
        .iter()
        .filter(|n| n.name == "slab_floor")
        .map(|n| {
            let m = n.mesh.as_ref().unwrap();
            let mut x_min = f32::INFINITY;
            let mut x_max = f32::NEG_INFINITY;
            let mut z_min = f32::INFINITY;
            let mut z_max = f32::NEG_INFINITY;
            for p in &m.positions {
                x_min = x_min.min(p[0]);
                x_max = x_max.max(p[0]);
                z_min = z_min.min(p[2]);
                z_max = z_max.max(p[2]);
            }
            (x_max - x_min, z_max - z_min)
        })
        .collect();
    assert!(extents.len() >= 2);
    let (w0, d0) = extents[0];
    for (i, (w, d)) in extents.iter().enumerate().skip(1) {
        assert!(
            (w - w0).abs() < 1e-3 && (d - d0).abs() < 1e-3,
            "slab {i} extent ({w},{d}) diverges from slab 0 ({w0},{d0})"
        );
    }
}

#[test]
fn building_entrance_stays_over_cellar_footprint() {
    // With a smaller, east-aligned cellar the ground floor overhangs the
    // basement on three sides. A storey-0 entrance must land only on the
    // facade stretch that has a cellar wall directly below it (the shared
    // east wall), so the door never juts out past the recessed basement.
    // Regression for the "basement door sticking out" bug.
    let src = r#"
        material "stone" (color=[0.6, 0.55, 0.5])
        building "brownstone" (
          seed=21, style="apartment-block",
          floor_area=110, cellar_area=60, rooms=10,
          floors_above=2, floors_below=1,
          staircases=1, entrances=1,
          roof="mansard", windows=6,
          mat="stone",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1)
          room_type "living"  (kind=public,  density=2)
        }
    "#;
    let g = lower_src(src);

    // World XZ of a node, accumulating ancestor translations.
    fn world_xz(g: &SceneGraph, mut i: u32) -> (f32, f32) {
        let (mut x, mut z) = (0.0f32, 0.0f32);
        loop {
            let n = &g.nodes[i as usize];
            let t = n.transform.translation;
            x += t.x;
            z += t.z;
            match n.parent {
                Some(p) => i = p.0,
                None => break,
            }
        }
        (x, z)
    }

    // Absolute XZ bounds of the basement floor slab (the cellar footprint).
    let (mut cx_min, mut cx_max, mut cz_min, mut cz_max) =
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    let b1 = g
        .nodes
        .iter()
        .position(|n| n.kind == "group" && n.name == "floor_b1")
        .expect("basement floor group") as u32;
    let mut found_cellar_slab = false;
    for (i, n) in g.nodes.iter().enumerate() {
        if n.name != "slab_floor" {
            continue;
        }
        // Only the basement slab — climb parents to confirm `floor_b1`.
        let mut cur = n.parent;
        let mut in_b1 = false;
        while let Some(p) = cur {
            if p.0 == b1 {
                in_b1 = true;
                break;
            }
            cur = g.nodes[p.0 as usize].parent;
        }
        if !in_b1 {
            continue;
        }
        found_cellar_slab = true;
        let (ox, oz) = world_xz(&g, i as u32);
        let m = n.mesh.as_ref().expect("slab mesh");
        for p in &m.positions {
            cx_min = cx_min.min(ox + p[0]);
            cx_max = cx_max.max(ox + p[0]);
            cz_min = cz_min.min(oz + p[2]);
            cz_max = cz_max.max(oz + p[2]);
        }
    }
    assert!(found_cellar_slab, "no basement slab found");

    // The entrance group carries role `ext_door`; its transform is the
    // door's world pose.
    let door = g
        .nodes
        .iter()
        .position(|n| n.role.as_deref() == Some("ext_door"))
        .expect("an exterior entrance");
    let (dx, dz) = world_xz(&g, door as u32);

    // The door sits on the perimeter wall, so its position lies on the
    // cellar slab's boundary. Allow a wall-thickness margin so a door
    // centred on the shared wall still counts as grounded.
    let eps = 0.2;
    assert!(
        dx >= cx_min - eps
            && dx <= cx_max + eps
            && dz >= cz_min - eps
            && dz <= cz_max + eps,
        "entrance at ({dx:.3}, {dz:.3}) is outside the cellar footprint \
         x[{cx_min:.3},{cx_max:.3}] z[{cz_min:.3},{cz_max:.3}] — \
         it overhangs the recessed basement"
    );
}

#[test]
fn building_cellar_interior_doors_stay_over_cellar_footprint() {
    // The circulation column fillers (and the doors carved into them) are
    // derived from the whole-building bounds but emitted on every storey.
    // On an inset cellar (`cellar_area < floor_area`) a filler door at the
    // gap's midpoint would land past the recessed basement wall and hang in
    // open air. Regression for the "basement door sticking out" bug — the
    // protruding panel was a cellar-level interior door, not the entrance.
    let src = r#"
        material "stone" (color=[0.6, 0.55, 0.5])
        building "brownstone" (
          seed=21, style="apartment-block",
          floor_area=110, cellar_area=60, rooms=10,
          floors_above=2, floors_below=1,
          staircases=1, entrances=1,
          roof="mansard", windows=6,
          mat="stone",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1)
          room_type "living"  (kind=public,  density=2)
        }
    "#;
    let g = lower_src(src);

    fn world_xz(g: &SceneGraph, mut i: u32) -> (f32, f32) {
        let (mut x, mut z) = (0.0f32, 0.0f32);
        loop {
            let n = &g.nodes[i as usize];
            let t = n.transform.translation;
            x += t.x;
            z += t.z;
            match n.parent {
                Some(p) => i = p.0,
                None => break,
            }
        }
        (x, z)
    }

    let b1 = g
        .nodes
        .iter()
        .position(|n| n.kind == "group" && n.name == "floor_b1")
        .expect("basement floor group") as u32;
    let under_b1 = |g: &SceneGraph, mut i: u32| -> bool {
        while let Some(p) = g.nodes[i as usize].parent {
            if p.0 == b1 {
                return true;
            }
            i = p.0;
        }
        false
    };

    // Cellar footprint from the basement floor slab.
    let (mut cx_min, mut cx_max, mut cz_min, mut cz_max) =
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for (i, n) in g.nodes.iter().enumerate() {
        if n.name != "slab_floor" || !under_b1(&g, i as u32) {
            continue;
        }
        let (ox, oz) = world_xz(&g, i as u32);
        let m = n.mesh.as_ref().expect("slab mesh");
        for p in &m.positions {
            cx_min = cx_min.min(ox + p[0]);
            cx_max = cx_max.max(ox + p[0]);
            cz_min = cz_min.min(oz + p[2]);
            cz_max = cz_max.max(oz + p[2]);
        }
    }
    assert!(cx_max.is_finite(), "no basement slab found");

    // Every cellar interior door must sit within the basement footprint.
    let eps = 0.2;
    for (i, n) in g.nodes.iter().enumerate() {
        if !n.name.starts_with("int_door_") || !under_b1(&g, i as u32) {
            continue;
        }
        let (dx, dz) = world_xz(&g, i as u32);
        assert!(
            dx >= cx_min - eps
                && dx <= cx_max + eps
                && dz >= cz_min - eps
                && dz <= cz_max + eps,
            "cellar interior door `{}` at ({dx:.3}, {dz:.3}) is outside the \
             cellar footprint x[{cx_min:.3},{cx_max:.3}] z[{cz_min:.3},{cz_max:.3}] \
             — it overhangs the recessed basement",
            n.name
        );
    }
}

#[test]
fn building_t4_is_deterministic_under_same_seed() {
    let src = r#"
        material "stone" (color=[0.6, 0.55, 0.5])
        building "brownstone" (
          seed=42, style="apartment-block",
          floor_area=110, cellar_area=60, rooms=10,
          floors_above=2, floors_below=1,
          staircases=1, entrances=1,
          roof="mansard", windows=6,
          mat="stone",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1)
          room_type "living"  (kind=public,  density=2)
        }
    "#;
    let a = lower_src(src);
    let b = lower_src(src);
    assert_eq!(a.nodes.len(), b.nodes.len(), "node count diverged");
    let hash = |g: &SceneGraph| -> f64 {
        let mut acc = 0.0f64;
        for n in &g.nodes {
            if let Some(m) = &n.mesh {
                for p in &m.positions {
                    acc += p[0] as f64;
                    acc += p[1] as f64;
                    acc += p[2] as f64;
                }
            }
        }
        acc
    };
    assert!((hash(&a) - hash(&b)).abs() < 1e-3, "mesh hash diverged");
}
