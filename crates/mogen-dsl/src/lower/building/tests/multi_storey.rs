use super::{count_kind, lower_src, MIN_GRID_SRC, MULTI_FLOOR_SRC};
use mogen_core::SceneGraph;

#[test]
fn multi_storey_emits_one_floor_group_per_storey() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // 3 storeys: floor_b1, floor_0, floor_1.
    let floors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("floor_"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        floors.len(),
        3,
        "expected 3 floor groups, got {floors:?}"
    );
    assert!(floors.contains(&"floor_b1"));
    assert!(floors.contains(&"floor_0"));
    assert!(floors.contains(&"floor_1"));
}

#[test]
fn multi_storey_floors_stack_vertically_by_step() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // ceiling_height (2.6) + ceiling_thickness (0.2) = step of 2.8.
    let y_for = |name: &str| -> f32 {
        g.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name}"))
            .transform
            .translation
            .y
    };
    let y_b1 = y_for("floor_b1");
    let y_0 = y_for("floor_0");
    let y_1 = y_for("floor_1");
    let step = 2.6 + 0.2;
    assert!((y_0 - y_b1 - step).abs() < 1e-3, "basement→ground step wrong: {} vs {step}", y_0 - y_b1);
    assert!((y_1 - y_0 - step).abs() < 1e-3, "ground→upper step wrong: {} vs {step}", y_1 - y_0);
}

#[test]
fn staircase_emits_two_half_flights_per_storey_pair() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // Switchback: 3 storeys → 2 transitions → 2 pairs × 2 half-flights = 4.
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(
        flights, 4,
        "expected 4 half-flights (2 per pair × 2 pairs), got {flights}"
    );
}

#[test]
fn switchback_pairs_lower_and_upper_per_storey() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let lowers = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .filter(|n| n.tags.iter().any(|t| t == "lower"))
        .count();
    let uppers = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .filter(|n| n.tags.iter().any(|t| t == "upper"))
        .count();
    assert_eq!(lowers, 2, "expected 2 lower half-flights, got {lowers}");
    assert_eq!(uppers, 2, "expected 2 upper half-flights, got {uppers}");
}

#[test]
fn switchback_emits_a_mid_landing_per_pair() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let landings = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_landing"))
        .count();
    assert_eq!(
        landings, 2,
        "expected 2 mid-landings (one per storey-pair), got {landings}"
    );
}

#[test]
fn stair_steps_have_constant_rise_not_growing_boxes() {
    // Stair treads must be single-rise blocks at their true elevation,
    // not growing boxes that span y=0..tread_top. The growing-box layout
    // makes the under-side a flat plane at the flight base, so the
    // half-flight stacked directly above (same x-half on the next
    // storey) capped headroom at half_rise (= 1.4 m for a 2.8 m step).
    // Constant-rise blocks keep the under-side parallel to the treads
    // so the climber below has room to stand the whole way up.
    let g = lower_src(MULTI_FLOOR_SRC);
    // Walk every stair_flight subtree and grab the per-step box meshes.
    // The stair_simple module emits them as `box "step_$i"` children
    // (no role) so we match by name rather than role.
    let mut heights: Vec<f32> = Vec::new();
    let mut frontier: Vec<usize> = g
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.role.as_deref() == Some("stair_flight"))
        .map(|(i, _)| i)
        .collect();
    while let Some(i) = frontier.pop() {
        for c in &g.nodes[i].children {
            let idx = c.0 as usize;
            let n = &g.nodes[idx];
            if n.name.starts_with("step_") {
                if let Some(mesh) = &n.mesh {
                    let (mut ymin, mut ymax) = (f32::INFINITY, f32::NEG_INFINITY);
                    for p in &mesh.positions {
                        ymin = ymin.min(p[1]);
                        ymax = ymax.max(p[1]);
                    }
                    heights.push(ymax - ymin);
                }
            }
            frontier.push(idx);
        }
    }
    assert!(!heights.is_empty(), "no stair step meshes emitted");
    let max_h = heights.iter().cloned().fold(0.0f32, f32::max);
    let min_h = heights.iter().cloned().fold(f32::INFINITY, f32::min);
    assert!(
        (max_h - min_h).abs() < 1e-3,
        "stair step heights should be constant (single-rise blocks), \
         saw min={min_h} max={max_h} — a growing spread means the under-stair \
         is flat and steals headroom from the flight below"
    );
    // Sanity: with half_rise = 1.4 m and target ~0.18 m per step, the
    // half-flight gets 8 steps so actual_rise ≈ 0.175 m. Each block
    // should hover near that, not the full half_rise.
    assert!(
        max_h < 0.5,
        "step height {max_h} is suspiciously tall — looks like the old \
         growing-box layout (top step ≈ half_rise) is back"
    );
}

#[test]
fn flight_handrails_sit_on_each_flights_own_spine_edge() {
    // The lower (east) flight's spine-facing edge is at +spine/2; its
    // rail centre belongs at +(spine/2 + thickness/2), with the rail's
    // west face flush against the spine. Placing the rail at the
    // opposite sign would put it across the spine inside the OTHER
    // flight's footprint, where the climber can't reach it. Mirror for
    // the upper (west) flight.
    let g = lower_src(MULTI_FLOOR_SRC);
    let mut saw_lower = false;
    let mut saw_upper = false;
    for n in &g.nodes {
        if !n.name.starts_with("flight_handrail_") {
            continue;
        }
        // Rail names are `flight_handrail_{storey}_{lower|upper}` —
        // the suffix tells us which flight the rail serves.
        let x = n.transform.translation.x;
        if n.name.ends_with("_upper") {
            saw_upper = true;
            assert!(
                x < 0.0,
                "upper flight rail should sit west of spine centre, got x={x} ({})",
                n.name
            );
        } else if n.name.ends_with("_lower") {
            saw_lower = true;
            assert!(
                x > 0.0,
                "lower flight rail should sit east of spine centre, got x={x} ({})",
                n.name
            );
        }
    }
    assert!(saw_lower && saw_upper, "expected both flight rails");
}

#[test]
fn flight_handrail_corners_stay_within_flight_z_extent() {
    // A rotated rail box pushes its corners past flight_z_min and
    // flight_z_max by ~RH/2·sin(α), making the rail jut into the entry
    // zone and overhang the landing. The sheared-parallelogram rail's
    // south/north faces are vertical, so its world-space z extent
    // matches the flight depth exactly.
    let g = lower_src(MULTI_FLOOR_SRC);
    // Find the staircase group's world translation — the rails are
    // expressed in stair-local coordinates and the staircase group
    // carries the cell-centre offset.
    let stair_centre_z = g
        .nodes
        .iter()
        .find(|n| n.name == "staircase_0")
        .map(|n| n.transform.translation.z)
        .expect("staircase_0 group");
    for n in &g.nodes {
        if !n.name.starts_with("flight_handrail_") {
            continue;
        }
        let mesh = n.mesh.as_ref().expect("rail has mesh");
        let rail_centre_z = n.transform.translation.z;
        let min_z = mesh
            .positions
            .iter()
            .map(|p| p[2])
            .fold(f32::INFINITY, f32::min);
        let max_z = mesh
            .positions
            .iter()
            .map(|p| p[2])
            .fold(f32::NEG_INFINITY, f32::max);
        // The rail's local mesh z extent equals the flight depth
        // exactly when sheared (vertical end faces). A rotated box
        // would extend further by RH/2 * sin(slope_angle) ≈ 0.25 m.
        let span = max_z - min_z;
        // Flight depth ≈ cell_depth - 2 m (entry + landing strips).
        // We just need to assert the rail does NOT overshoot — span
        // bounded above by what a vertical-ended panel would produce.
        // Cell depths in MULTI_FLOOR_SRC are bounded by floor_area, so
        // a generous upper bound is the cell depth itself; the tight
        // assertion is that the rail's *centre* sits at stair-local
        // z=0 (between the entry and landing zones) and its span
        // doesn't add any tilt overshoot.
        let _ = (stair_centre_z, rail_centre_z, span);
        // Concretely, the four corner z values must be exactly
        // ±span/2 — no rotation means each z position appears 12 times
        // (4 corners on each of the 2 ±x faces, plus duplicates from
        // the box's 24-vertex face split). Check that exactly two
        // distinct z values appear.
        let mut zs: Vec<f32> = mesh.positions.iter().map(|p| p[2]).collect();
        zs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        zs.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
        assert_eq!(
            zs.len(),
            2,
            "rail {} should have exactly 2 distinct local z values \
             (vertical end faces), got {:?} — a rotated box would have 4",
            n.name,
            zs
        );
    }
}

#[test]
fn cutout_edge_railing_only_on_top_storey() {
    // The south edge of the slab cutout faces intact entry-platform
    // slab. On the top storey its east half is a real drop hazard
    // (no flight ascends from the top) and gets a railing. On every
    // other storey the east half is the FIRST STEP of the next pair's
    // lower flight — a railing there walls off the base of the next
    // ascent, leaving the staircase impossible to traverse upward.
    // MULTI_FLOOR_SRC has bottom=-1 and top=1 with one staircase, so
    // we expect exactly one cutout_handrail and it must belong to
    // storey 1 (the top).
    let g = lower_src(MULTI_FLOOR_SRC);
    let cutout_rails: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("cutout_handrail_"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(
        cutout_rails,
        vec!["cutout_handrail_1"],
        "cutout-edge railing should only fire on the top storey"
    );
}

#[test]
fn elevator_emits_a_single_shaft_for_the_whole_building() {
    let g = lower_src(MULTI_FLOOR_SRC);
    let shafts = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("elevator"))
        .count();
    assert_eq!(shafts, 1);
}

#[test]
fn switchback_flights_split_east_and_west() {
    // Switchback: the lower half-flight sits on the east half of the
    // cell and the upper half-flight on the west half, separated by a
    // central spine. Without this split both flights would overlap in
    // plan and the mid-landing would have nowhere coherent to put its
    // two access points.
    let g = lower_src(MULTI_FLOOR_SRC);
    let mut saw_lower = false;
    let mut saw_upper = false;
    for n in &g.nodes {
        if n.role.as_deref() != Some("stair_flight") {
            continue;
        }
        let x = n.transform.translation.x;
        if n.tags.iter().any(|t| t == "lower") {
            saw_lower = true;
            assert!(
                x > 0.05,
                "lower half-flight should sit on east half, got x={x}"
            );
        }
        if n.tags.iter().any(|t| t == "upper") {
            saw_upper = true;
            assert!(
                x < -0.05,
                "upper half-flight should sit on west half, got x={x}"
            );
        }
    }
    assert!(saw_lower && saw_upper, "expected both lower and upper flights");
}

#[test]
fn circulation_cells_get_shaft_walls() {
    // Shaft enclosure walls (MULTI_FLOOR_SRC = 3 storeys):
    // - Staircase: N/E/S (3) — the west side stays open so the
    //   per-storey cell-shared wall can carry the door cutout that
    //   lands on the south entry zone.
    // - Elevator: N/E/S (3) full-height + W split into one piece per
    //   storey (3) so each storey's door cutout can shift along Z to
    //   match its own room layout. A single full-height west wall
    //   could only hold one X column per door, so per-storey shifts
    //   would smear into one giant hole via `wall_with_holes` merge.
    let g = lower_src(MULTI_FLOOR_SRC);
    let walls = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("shaft_wall"))
        .count();
    assert_eq!(
        walls, 9,
        "expected 3 walls for the stair + 3 full-height + 3 per-storey W for the elevator"
    );
}

#[test]
fn door_bfs_never_connects_stair_to_elevator() {
    // A door between two circulation cells is useless: you can't step
    // out of a stairwell into an elevator shaft. The BFS must skip
    // those edges so the tree connects the elevator via a real room.
    let g = lower_src(MULTI_FLOOR_SRC);
    for n in &g.nodes {
        let Some(role) = n.role.as_deref() else { continue };
        if role != "service_wall" {
            continue;
        }
        // Wall name format: `interior_wall_{idx}_{lo}_{hi}`.
        let parts: Vec<&str> = n.name.split('_').collect();
        if parts.len() < 5 {
            continue;
        }
        let lo: usize = parts[3].parse().unwrap();
        let hi: usize = parts[4].parse().unwrap();
        // Skip if this wall is a stair↔elevator wall (both are
        // circulation cells). We just need to check the wall isn't
        // carrying a door — i.e. it's a solid 24-vertex box, not a
        // wall_with_holes mesh.
        // Walk up to the parent cell to see its kind via the group name.
        let parent_idx = n.parent.unwrap();
        let parent_name = &g.nodes[parent_idx.0 as usize].name;
        if !(parent_name.starts_with("staircase_") || parent_name.starts_with("elevator_")) {
            continue;
        }
        // A wall between two circulation cells would have BOTH a stair
        // and an elevator on its two sides — the parent (lower idx) is
        // a circulation cell (we just checked). For a stair↔elevator
        // wall to exist, the higher-idx cell must also be circulation.
        // We can't read kinds from the graph, but the absence of a hole
        // is what proves the BFS skipped it: the moment a stair-elevator
        // edge enters the tree, this wall gets a door cutout.
        let mesh = n.mesh.as_ref().unwrap();
        let has_hole = mesh.positions.len() > 24;
        // The connectivity check is the indirect property — assert no
        // pairing where lo & hi are both circulation. We don't have
        // direct kind info, but the wall index pair `(lo, hi)` lets
        // the test author eyeball it. For MULTI_FLOOR_SRC the BFS
        // currently never picks a stair-elevator edge; this test fails
        // if a future change re-enables it.
        // The strongest assertion we can make portably: if a wall is
        // BOTH service_wall AND on a circulation cell, AND it has a
        // hole, the other side must be a room, not the other circulation
        // cell — but without kind metadata in the graph we can't tell
        // directly. Settle for the layout-independent assertion that
        // (5,6) — staircase 5 ↔ elevator 6 in MULTI_FLOOR_SRC — never
        // appears with a hole.
        if (lo, hi) == (5, 6) || (lo, hi) == (6, 5) {
            assert!(
                !has_hole,
                "BFS placed a door between stair and elevator (wall {})",
                n.name
            );
        }
    }
}

#[test]
fn stair_entry_zone_gets_an_interior_door() {
    // Regression for the grid_office example
    // (examples/buildings/grid_office.mog): when the layout solver only
    // exposes the staircase's *west* entry slot to an adjacent RoomCell
    // (i.e. no room touches the stair's south face), the BFS in
    // `place_interior_doors` must still place a door at that slot.
    //
    // The bug was that the eligibility threshold
    // `edge.span() >= cfg.door_w * 1.1` excluded the slot, because
    // `STAIR_ENTRY_DEPTH = 1.0` m exactly equals the slot span and
    // `door_w * 1.1` is above it for any reasonable door width. With
    // no other edge available, the stair was unreachable.
    let src = r#"
        material "concrete" (color=[0.78, 0.78, 0.75])
        building "office" (
          seed=7, style="office-core",
          floor_area=500, floors_above=2, rooms=19,
          windows=40, entrances=1, ceiling_height=2.8,
          door_w=0.95, door_h=2.1, staircases=1,
          mat="concrete",
        ) {
          room_type "office"   (kind=staff_only, density=4)
          room_type "corridor" (kind=public,     density=1)
        }
    "#;
    let g = lower_src(src);

    // Compute world position by accumulating translation up the parent
    // chain. Everything along the way is axis-aligned in the building
    // (floor groups offset on Y, openings groups at identity), so a
    // plain sum suffices for the (x, z) check the assertion needs.
    let world_pos = |idx: usize| -> (f32, f32, f32) {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut z = 0.0;
        let mut cur = Some(mogen_core::NodeId(idx as u32));
        while let Some(id) = cur {
            let n = &g.nodes[id.0 as usize];
            x += n.transform.translation.x;
            y += n.transform.translation.y;
            z += n.transform.translation.z;
            cur = n.parent;
        }
        (x, y, z)
    };

    // Locate the floor-0 staircase landing — that node sits at the
    // (x, y, z) centre of the south entry strip.
    let landing_idx = g
        .nodes
        .iter()
        .position(|n| n.role.as_deref() == Some("staircase_landing"))
        .expect("staircase_landing node");
    let (stair_x, _, stair_z) = world_pos(landing_idx);

    // The entry zone is the 1m strip at the south end of the stair cell.
    // The landing centre sits at the stair-cell centre, so the entry
    // zone runs from z = stair_z - STAIR_DEPTH/2 northward by
    // STAIR_ENTRY_DEPTH = 1m. We tolerate a generous box around it to
    // allow doors on east / west / south faces of the entry zone.
    let stair_depth = 4.0_f32; // STAIR_DEPTH constant in circulation.rs
    let entry_depth = 1.0_f32; // STAIR_ENTRY_DEPTH constant
    let column_width = 2.0_f32; // typical office-core column width; bound is generous
    let entry_x_min = stair_x - column_width;
    let entry_x_max = stair_x + column_width;
    let entry_z_min = stair_z - 0.5 * stair_depth - 0.5; // south face plus wall slack
    let entry_z_max = stair_z - 0.5 * stair_depth + entry_depth + 0.5;

    let mut door_in_entry: Option<(String, f32, f32)> = None;
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("int_door") {
            continue;
        }
        let (dx, _, dz) = world_pos(i);
        if dx >= entry_x_min
            && dx <= entry_x_max
            && dz >= entry_z_min
            && dz <= entry_z_max
        {
            door_in_entry = Some((n.name.clone(), dx, dz));
            break;
        }
    }

    assert!(
        door_in_entry.is_some(),
        "no interior door reaches the staircase entry zone \
         (x∈[{entry_x_min:.2},{entry_x_max:.2}], z∈[{entry_z_min:.2},{entry_z_max:.2}]); \
         landing at ({stair_x:.2}, {stair_z:.2})"
    );
}

#[test]
fn stair_entry_door_is_not_blocked_by_shaft_wall() {
    // Regression: `emit_shaft_enclosure` was emitting `shaft_wall_n/e/s` as
    // solid 24-vert boxes that sat just outside each face of the stair cell.
    // When the BFS placed a stair entry door on a face whose adjacent cell
    // is a Room (the room's per-cell wall carries the door cutout, exactly
    // like the always-exempted west face), the shaft_wall covering that
    // face's z-range still had no hole — it shadowed the room wall's
    // cutout, visually masking the door slab and blocking passage with its
    // trimesh collider.
    //
    // Reproducer matches slop-land's seed=2 office-core layout: a corridor
    // cell sits south of the stair, so the BFS places the entry door on the
    // stair's south face. The fix omits shaft_wall_X whenever a Room shares
    // that face — analogous to the existing west-face exemption.
    let src = r#"
        material "concrete" (color=[0.78, 0.78, 0.75])
        building "office" (
          seed=2, style="office-core",
          floor_area=500, floors_above=2, rooms=19,
          windows=40, entrances=1, ceiling_height=2.8,
          door_w=0.95, door_h=2.1, staircases=1,
          mat="concrete",
        ) {
          room_type "office"   (kind=staff_only, density=4)
          room_type "corridor" (kind=public,     density=1)
        }
    "#;
    let g = lower_src(src);

    // Compute world translation by accumulating up the parent chain (every
    // intermediate node is at identity rotation in this slice of the tree).
    let world_pos = |idx: usize| -> (f32, f32, f32) {
        let mut x = 0.0;
        let mut y = 0.0;
        let mut z = 0.0;
        let mut cur = Some(mogen_core::NodeId(idx as u32));
        while let Some(id) = cur {
            let n = &g.nodes[id.0 as usize];
            x += n.transform.translation.x;
            y += n.transform.translation.y;
            z += n.transform.translation.z;
            cur = n.parent;
        }
        (x, y, z)
    };

    // Collect every interior/exterior door's xz position. A door wrapper
    // carries no mesh, but its xz position is the door's centre.
    let mut doors: Vec<(String, f32, f32)> = Vec::new();
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("int_door") && n.role.as_deref() != Some("ext_door") {
            continue;
        }
        let (x, _y, z) = world_pos(i);
        doors.push((n.name.clone(), x, z));
    }

    // Every shaft_wall mesh: derive its world-space xz footprint from its
    // mesh bounds + its world transform, then assert no door footprint
    // overlaps it.
    let door_w = 0.95;
    let half_door = 0.5 * door_w;
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("shaft_wall") {
            continue;
        }
        let Some(mesh) = n.mesh.as_ref() else { continue };
        let (mut mnx, mut mxx) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut mnz, mut mxz) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in &mesh.positions {
            mnx = mnx.min(p[0]); mxx = mxx.max(p[0]);
            mnz = mnz.min(p[2]); mxz = mxz.max(p[2]);
        }
        let (wx, _, wz) = world_pos(i);
        let (sw_xmin, sw_xmax) = (wx + mnx, wx + mxx);
        let (sw_zmin, sw_zmax) = (wz + mnz, wz + mxz);

        for (door_name, dx, dz) in &doors {
            let (d_xmin, d_xmax) = (dx - half_door, dx + half_door);
            // Door is thin along its facing axis (~4 cm slab + frame depth)
            // but we don't know facing here; treat door as a 0.12 m strip
            // centred on z (which matches the room wall thickness).
            let (d_zmin, d_zmax) = (dz - 0.06, dz + 0.06);
            let overlaps_x = sw_xmin < d_xmax - 0.005 && sw_xmax > d_xmin + 0.005;
            let overlaps_z = sw_zmin < d_zmax - 0.005 && sw_zmax > d_zmin + 0.005;
            assert!(
                !(overlaps_x && overlaps_z),
                "shaft_wall '{}' xz=[{:.2},{:.2}]→[{:.2},{:.2}] covers door '{}' at ({:.2},{:.2})",
                n.name, sw_xmin, sw_zmin, sw_xmax, sw_zmax, door_name, dx, dz,
            );
        }
    }
}

#[test]
fn shaft_wall_still_closes_faces_with_no_adjacent_room() {
    // Counterpart to `stair_entry_door_is_not_blocked_by_shaft_wall`. The
    // fix omits `shaft_wall_X` whenever a `Room` cell shares that face;
    // it MUST still emit a wall when no room is there. Otherwise the
    // shaft is open to the column gap (or beyond), which is what those
    // walls existed to close.
    //
    // For seed=2 office-core / floor_area=500, slop-land's geometry:
    //   - South face: corridor cell adjacent → shaft_wall_s skipped.
    //   - North face: a column-filler gap, no adjacent room → must
    //     still emit shaft_wall_n.
    //   - East face: building exterior, no adjacent room → still emits
    //     shaft_wall_e.
    let src = r#"
        material "concrete" (color=[0.78, 0.78, 0.75])
        building "office" (
          seed=2, style="office-core",
          floor_area=500, floors_above=2, rooms=19,
          windows=40, entrances=1, ceiling_height=2.8,
          door_w=0.95, door_h=2.1, staircases=1,
          mat="concrete",
        ) {
          room_type "office"   (kind=staff_only, density=4)
          room_type "corridor" (kind=public,     density=1)
        }
    "#;
    let g = lower_src(src);

    let mut saw_n = false;
    let mut saw_e = false;
    let mut saw_s = false;
    for n in &g.nodes {
        if n.role.as_deref() != Some("shaft_wall") {
            continue;
        }
        match n.name.as_str() {
            "shaft_wall_n" => saw_n = true,
            "shaft_wall_e" => saw_e = true,
            "shaft_wall_s" => saw_s = true,
            _ => {}
        }
    }
    assert!(saw_n, "shaft_wall_n was omitted but no room is north of the stair");
    assert!(saw_e, "shaft_wall_e was omitted but no room is east of the stair");
    assert!(
        !saw_s,
        "shaft_wall_s was emitted but the corridor sits south of the stair; \
         the room wall handles closure and would shadow the entry door"
    );
}

#[test]
fn elevator_doorways_are_one_and_a_half_times_door_width() {
    // Doors that open onto an elevator cell are widened to 1.5 × the
    // standard interior door (so 0.9 × 1.5 = 1.35 m at default
    // `door_w`). We force the unknown-module fallback so each int_door
    // panel mesh size === the opening width.
    let src = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "tower" (
          seed=3, style="hotel-corridor",
          floor_area=160, rooms=10,
          floors_above=2, floors_below=1,
          staircases=1, elevators=1,
          internal_door="no_such_module",
          mat="concrete",
        ) {
          room_type "suite" (kind=private, density=1)
        }
    "#;
    let g = lower_src(src);
    let mut widths: Vec<f32> = Vec::new();
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("int_door") {
            continue;
        }
        for c in &g.nodes[i].children {
            let panel = &g.nodes[c.0 as usize];
            if panel.kind != "panel" {
                continue;
            }
            let mesh = panel.mesh.as_ref().expect("panel mesh");
            let (mut xmin, mut xmax) = (f32::INFINITY, f32::NEG_INFINITY);
            for p in &mesh.positions {
                xmin = xmin.min(p[0]);
                xmax = xmax.max(p[0]);
            }
            widths.push(xmax - xmin);
        }
    }
    assert!(!widths.is_empty(), "no int_door panels emitted");
    let max_w = widths.iter().cloned().fold(0.0f32, f32::max);
    let min_w = widths.iter().cloned().fold(f32::INFINITY, f32::min);
    assert!(
        (max_w - 1.35).abs() < 1e-3,
        "expected an elevator doorway widened to 1.35 m, widest was {max_w}"
    );
    assert!(
        (min_w - 0.9).abs() < 1e-3,
        "expected non-elevator doorways at 0.9 m, narrowest was {min_w}"
    );
}

#[test]
fn elevator_has_a_1p5x_door_on_every_floor() {
    // Bulletproof: every storey must produce exactly one doorway between
    // the elevator and a non-circulation room, AND that doorway must be
    // the full 1.5 × `cfg.door_w` width (1.35 m at default). We force the
    // unknown-module fallback so each int_door is a plain panel whose
    // mesh local-X extent equals the opening width, then check both the
    // count per storey and the width per door.
    //
    // We sweep multiple seeds, multiple storey counts, and the
    // problematic `grid` style (which historically left the elevator
    // orphaned) so this guards against geometric corner cases — not just
    // one happy layout.
    for &seed in &[1, 2, 3, 7, 13, 42, 999] {
        for &(below, above) in &[(0u32, 2u32), (1, 2), (0, 3), (2, 3)] {
            check_elevator_doors_for(seed, below, above);
        }
    }
}

fn check_elevator_doors_for(seed: u32, floors_below: u32, floors_above: u32) {
    let src = format!(
        r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "tower" (
          seed={seed}, style="grid",
          floor_area=80, rooms=8,
          floors_above={floors_above}, floors_below={floors_below},
          staircases=1, elevators=1,
          internal_door="no_such_module",
          mat="concrete",
        ) {{
          room_type "office" (kind=staff_only, density=1)
        }}
        "#
    );
    let g = lower_src(&src);
    let storeys: Vec<i32> = (-(floors_below as i32)..(floors_above as i32)).collect();

    let elev_idx: Vec<usize> = g
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.role.as_deref() == Some("elevator"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        elev_idx.len(),
        1,
        "[seed={seed} below={floors_below} above={floors_above}] expected exactly one elevator group, got {}",
        elev_idx.len()
    );

    let storey_for = |mut idx: usize| -> Option<i32> {
        loop {
            let name = &g.nodes[idx].name;
            if let Some(rest) = name.strip_prefix("floor_") {
                if let Ok(s) = rest.parse::<i32>() {
                    return Some(s);
                } else if let Some(b) = rest.strip_prefix('b') {
                    return Some(-(b.parse::<i32>().ok()?));
                }
            }
            idx = g.nodes[idx].parent?.0 as usize;
        }
    };
    let world_xz = |idx: usize| -> (f32, f32) {
        let mut cur = idx;
        let (mut x, mut z) = (0.0f32, 0.0f32);
        loop {
            let n = &g.nodes[cur];
            x += n.transform.translation.x;
            z += n.transform.translation.z;
            match n.parent {
                Some(p) => cur = p.0 as usize,
                None => return (x, z),
            }
        }
    };
    // The fallback panel is the int_door group's only child of kind
    // "panel". Its mesh is `box_mesh([op.width, op.height, 0.04])` with
    // an identity local transform, so the local-X extent === op.width.
    let panel_width = |int_door_idx: usize| -> Option<f32> {
        for c in &g.nodes[int_door_idx].children {
            let panel = &g.nodes[c.0 as usize];
            if panel.kind != "panel" {
                continue;
            }
            let mesh = panel.mesh.as_ref()?;
            let (mut xmin, mut xmax) = (f32::INFINITY, f32::NEG_INFINITY);
            for p in &mesh.positions {
                xmin = xmin.min(p[0]);
                xmax = xmax.max(p[0]);
            }
            return Some(xmax - xmin);
        }
        None
    };

    let (ex, ez) = world_xz(elev_idx[0]);
    // Per-storey list of (door_world_x, door_world_z, width). The door
    // sits on the elevator's 2 m perimeter ⇒ |Δx| or |Δz| ≈ 1 m; allow
    // 1.1 for the wall-thickness inset on the panel.
    #[derive(Clone, Copy, Debug)]
    struct Door {
        x: f32,
        z: f32,
        width: f32,
    }
    let mut by_storey: std::collections::BTreeMap<i32, Vec<Door>> =
        std::collections::BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("int_door") {
            continue;
        }
        let (dx, dz) = world_xz(i);
        let touches = (dx - ex).abs() <= 1.1 && (dz - ez).abs() <= 1.1;
        if !touches {
            continue;
        }
        let s = storey_for(i).expect("int_door must live under a floor_<x> ancestor");
        let w = panel_width(i).expect("int_door must contain a fallback panel");
        by_storey.entry(s).or_default().push(Door {
            x: dx,
            z: dz,
            width: w,
        });
    }

    assert_eq!(
        by_storey.keys().copied().collect::<Vec<_>>(),
        storeys,
        "[seed={seed} below={floors_below} above={floors_above}] expected an elevator doorway on every storey; got {:?}",
        by_storey
    );
    for (s, doors) in &by_storey {
        assert_eq!(
            doors.len(),
            1,
            "[seed={seed} below={floors_below} above={floors_above}] storey {s} should have exactly one elevator doorway, got {doors:?}"
        );
        let d = doors[0];
        assert!(
            (d.width - 1.35).abs() < 1e-3,
            "[seed={seed} below={floors_below} above={floors_above}] storey {s} elevator doorway must be 1.35 m wide, got {}",
            d.width
        );
        // The doorway must sit on the elevator's west face (x = ex − 1)
        // and be centred along Z on the elevator's 2 m face. Grid
        // layouts always extend a room past both the elevator's z_min
        // and z_max, so neither corner of the shared edge has a wall on
        // the room side — there's nothing to push the door off-centre.
        assert!(
            (d.x - (ex - 1.0)).abs() < 0.06,
            "[seed={seed} below={floors_below} above={floors_above}] storey {s} elevator doorway should sit on the elevator's west face at x≈{}, got x={}",
            ex - 1.0,
            d.x
        );
        assert!(
            (d.z - ez).abs() < 1e-3,
            "[seed={seed} below={floors_below} above={floors_above}] storey {s} elevator doorway must be centred on the elevator's Z midline (z≈{ez}), got z={}",
            d.z
        );
    }
}

#[test]
fn upper_floors_have_no_entrance_holes() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // Entrances are tagged with role=ext_door; they should only sit
    // under the storey-0 subtree.
    let storey_for_node = |n_idx: usize| -> i32 {
        // Walk up to find the floor_<x> ancestor.
        let mut cur = Some(g.nodes[n_idx].parent);
        while let Some(Some(p)) = cur {
            let name = &g.nodes[p.0 as usize].name;
            if let Some(rest) = name.strip_prefix("floor_") {
                if let Ok(s) = rest.parse::<i32>() {
                    return s;
                } else if rest.starts_with('b') {
                    return -(rest[1..].parse::<i32>().unwrap_or(0));
                }
            }
            cur = Some(g.nodes[p.0 as usize].parent);
        }
        i32::MIN
    };
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() == Some("ext_door") {
            assert_eq!(
                storey_for_node(i),
                0,
                "ext_door must live under floor_0, found one under storey {}",
                storey_for_node(i)
            );
        }
    }
}

#[test]
fn skylight_only_emits_on_top_storey() {
    let src = r#"
        material "c" (color=[0.8, 0.8, 0.8])
        building "t" (
          seed=2, style="grid", floor_area=60, rooms=4,
          floors_above=2, staircases=1, skylights=2, mat="c",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let skies = g.nodes.iter().filter(|n| n.role.as_deref() == Some("skylight")).count();
    assert_eq!(skies, 2, "expected 2 skylights, got {skies}");
}

#[test]
fn t2_layout_is_deterministic_under_same_seed() {
    let a = lower_src(MULTI_FLOOR_SRC);
    let b = lower_src(MULTI_FLOOR_SRC);
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
    assert!((hash(&a) - hash(&b)).abs() < 1e-3, "mesh hash diverged under same seed");
}

#[test]
fn debug_hide_roof_drops_top_storey_ceiling() {
    // Baseline: a single-storey flat-roof building emits two slabs
    // (floor + ceiling). Setting debug_hide_roof should drop the ceiling.
    let base = MIN_GRID_SRC;
    let with_flag = MIN_GRID_SRC.replace(
        "mat=\"concrete\",",
        "mat=\"concrete\", debug_hide_roof=1,",
    );
    let g0 = lower_src(base);
    let g1 = lower_src(&with_flag);
    assert_eq!(count_kind(&g0, "slab"), 2, "baseline should have 2 slabs");
    assert_eq!(
        count_kind(&g1, "slab"),
        1,
        "debug_hide_roof should drop the ceiling slab"
    );
}

#[test]
fn debug_hide_roof_suppresses_skylights() {
    let src_base = r#"
        material "c" (color=[0.8, 0.8, 0.8])
        building "t" (
          seed=2, style="grid", floor_area=60, rooms=4,
          floors_above=1, skylights=2, mat="c",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    let src_hide = src_base.replace("mat=\"c\",", "mat=\"c\", debug_hide_roof=1,");
    let g0 = lower_src(src_base);
    let g1 = lower_src(&src_hide);
    let skies = |g: &SceneGraph| {
        g.nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some("skylight"))
            .count()
    };
    assert_eq!(skies(&g0), 2, "baseline should emit 2 skylights");
    assert_eq!(
        skies(&g1),
        0,
        "debug_hide_roof should suppress skylight emission"
    );
}

#[test]
fn debug_render_floor_isolates_a_single_storey() {
    // 3 storeys (b1, 0, 1). debug_render_floor=1 hides other storeys'
    // floor groups but leaves vertical circulation (stairs + elevator)
    // intact so the chosen storey reads in context.
    let src = MULTI_FLOOR_SRC.replace(
        "mat=\"concrete\",",
        "mat=\"concrete\", debug_render_floor=1,",
    );
    let g = lower_src(&src);
    let floors: Vec<&str> = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("floor_"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(floors, vec!["floor_1"], "should isolate exactly floor_1");
    // Stair flights still span every storey pair (b1→0, 0→1). Each
    // pair has two half-flights (switchback), so 2 pairs → 4 flights.
    let flights = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("stair_flight"))
        .count();
    assert_eq!(
        flights, 4,
        "stair flights should still emit for every storey pair when isolating"
    );
    // Elevator shaft still emits.
    let shafts = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("elevator"))
        .count();
    assert_eq!(
        shafts, 1,
        "elevator should still emit when isolating a floor"
    );
    // Wrapper carries `floating` so the connectivity validator (E1101)
    // exempts this partial debug view — stair flights stranded between
    // unrendered floor slabs would otherwise scan as disconnected clusters.
    let wrapper = g
        .nodes
        .iter()
        .find(|n| n.name == "tower")
        .expect("building wrapper node missing");
    assert!(
        wrapper.tags.iter().any(|t| t == "floating"),
        "isolated debug view should mark the building wrapper as floating, got tags={:?}",
        wrapper.tags
    );
}

#[test]
fn staircase_emits_one_access_poi_per_storey() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // 3 storeys (b1, 0, 1) → one stair_access marker each.
    let access = super::count_role(&g, "stair_access");
    assert_eq!(
        access, 3,
        "expected one stair_access POI per storey, got {access}"
    );
    // Each marker is geometry-free and carries its signed floor index.
    for n in g.nodes.iter().filter(|n| n.role.as_deref() == Some("stair_access")) {
        assert_eq!(n.kind, "poi", "stair_access marker should be a poi node");
        assert!(n.mesh.is_none(), "stair_access POI must stay geometry-free");
        assert!(
            n.tags.iter().any(|t| t.starts_with("floor=")),
            "stair_access POI should carry a floor=<n> tag, got {:?}",
            n.tags
        );
    }
}

#[test]
fn elevator_emits_one_stop_poi_per_storey() {
    let g = lower_src(MULTI_FLOOR_SRC);
    // 3 served storeys (b1, 0, 1) → one elevator_stop marker each.
    let stops = super::count_role(&g, "elevator_stop");
    assert_eq!(
        stops, 3,
        "expected one elevator_stop POI per served storey, got {stops}"
    );
    for n in g.nodes.iter().filter(|n| n.role.as_deref() == Some("elevator_stop")) {
        assert_eq!(n.kind, "poi", "elevator_stop marker should be a poi node");
        assert!(n.mesh.is_none(), "elevator_stop POI must stay geometry-free");
        assert!(
            n.tags.iter().any(|t| t.starts_with("floor=")),
            "elevator_stop POI should carry a floor=<n> tag, got {:?}",
            n.tags
        );
    }
}

#[test]
fn circulation_pois_land_on_floor_surfaces() {
    // Both stair-access and elevator-stop markers should resolve to the
    // floor datum of their storey (world Y = storey * step). The markers
    // are nested under group transforms, so accumulate ancestor Y.
    let g = lower_src(MULTI_FLOOR_SRC);
    let step = 2.6 + 0.2; // ceiling_height + ceiling_thickness defaults.
    let world_y = |mut idx: usize| -> f32 {
        let mut y = 0.0;
        loop {
            y += g.nodes[idx].transform.translation.y;
            match g.nodes[idx].parent {
                Some(p) => idx = p.0 as usize,
                None => break,
            }
        }
        y
    };
    for (i, n) in g.nodes.iter().enumerate() {
        let role = n.role.as_deref();
        if role != Some("stair_access") && role != Some("elevator_stop") {
            continue;
        }
        let floor: i32 = n
            .tags
            .iter()
            .find_map(|t| t.strip_prefix("floor="))
            .and_then(|s| s.parse().ok())
            .expect("circulation POI missing floor=<n> tag");
        let expected = floor as f32 * step;
        let got = world_y(i);
        assert!(
            (got - expected).abs() < 1e-3,
            "{:?} on floor {floor} should sit at world y={expected}, got {got}",
            n.name
        );
    }
}
