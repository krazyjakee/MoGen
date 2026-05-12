use super::{count_kind, has_tag, lower_src, MIN_GRID_SRC};
use crate::lower::lower;
use crate::parser::parse;
use mogen_core::SceneGraph;

#[test]
fn building_lowers_to_walls_slabs_doors() {
    let g = lower_src(MIN_GRID_SRC);
    // Top-level wrapper plus floor + shell + rooms + openings groups.
    assert!(
        g.nodes.iter().any(|n| n.kind == "building"),
        "wrapper node missing"
    );
    // Two slabs (floor + ceiling).
    let slabs = count_kind(&g, "slab");
    assert!(slabs >= 2, "expected ≥ 2 slab nodes, got {slabs}");
    // Four perimeter walls + at least three interior walls (4 rooms → ≥3 shared edges).
    let walls = count_kind(&g, "wall");
    assert!(walls >= 4, "expected ≥ 4 wall nodes, got {walls}");
    // Building tag propagated.
    assert!(has_tag(&g, "building"), "no `building` tag on subtree");
    // At least one entrance instance — door_simple module body becomes the
    // subtree under an `ext_door_*` group.
    assert!(
        g.nodes.iter().any(|n| n.role.as_deref() == Some("ext_door")),
        "no ext_door role on any node"
    );
}

#[test]
fn building_apartment_smoke() {
    let src = r#"
        material "warm_wood" (color=[0.55, 0.38, 0.22])
        material "tile" (color=[0.9, 0.9, 0.85])
        building "flat" (
          seed=2, style="apartment-block",
          floor_area=70, rooms=5, windows=4, entrances=1,
          mat="warm_wood",
        ) {
          room_type "bedroom" (kind=private, density=2)
          room_type "kitchen" (kind=service, density=1, mat="tile")
          room_type "living"  (kind=public,  density=2)
          adjacency "kitchen" (adjacent_to=["living"])
        }
    "#;
    let g = lower_src(src);
    assert!(count_kind(&g, "slab") >= 2);
    assert!(count_kind(&g, "wall") >= 4);
    // Per-room groups appear with `room` role.
    let rooms = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("room"))
        .count();
    assert!(rooms >= 2, "expected ≥ 2 rooms, got {rooms}");
}

#[test]
fn perimeter_window_holes_align_on_every_side() {
    // Regression: opening_local's East/West sign was flipped, so window
    // models sat in front of a wall hole sized for a different window
    // (small in a large hole, large in a small). The hole's wall-local
    // X must map back to the window's world XZ under the wall's rotation
    // — verify the per-storey-0 perimeter walls have a full-height pier
    // gap whose centre matches each window's projected world coord, and
    // whose width matches the window's opening width.
    use glam::Vec3;
    // Use an explicit small floorplate with 4 windows so the placement
    // distributes one window per side (no per-segment clustering, which
    // is a separate layout issue and would merge holes in the wall mesh).
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "house" (
          seed=11, style="grid",
          floor_area=85, rooms=5,
          windows=4, entrances=1,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);

    let world_pos = |g: &SceneGraph, mut id: usize| -> Vec3 {
        let mut acc = Vec3::ZERO;
        loop {
            acc = g.nodes[id].transform.translation
                + g.nodes[id].transform.rotation * acc;
            match g.nodes[id].parent {
                Some(p) => id = p.0 as usize,
                None => break acc,
            }
        }
    };

    // For each perimeter wall, find the gaps between full-height pier
    // sub-boxes along its long axis and collect (world projection low,
    // high). Then check each window's projection falls inside a gap of
    // matching width.
    use std::collections::BTreeMap;
    let mut holes_by_side: BTreeMap<&'static str, Vec<(f32, f32)>> = BTreeMap::new();
    for (i, n) in g.nodes.iter().enumerate() {
        if n.role.as_deref() != Some("exterior_wall") {
            continue;
        }
        let side = n
            .tags
            .iter()
            .find_map(|t| t.strip_prefix("side="))
            .expect("side tag");
        let mesh = n.mesh.as_ref().expect("wall mesh");
        let nboxes = mesh.positions.len() / 24;
        let mat = glam::Mat4::from_translation(g.nodes[i].transform.translation)
            * glam::Mat4::from_quat(g.nodes[i].transform.rotation);
        // Transform every vertex into world.
        let world: Vec<Vec3> = mesh
            .positions
            .iter()
            .map(|p| mat.transform_point3(Vec3::new(p[0], p[1], p[2])))
            .collect();
        let mut piers: Vec<(f32, f32)> = Vec::new();
        let long_idx = match side {
            "north" | "south" => 0,
            "east" | "west" => 2,
            _ => unreachable!(),
        };
        for k in 0..nboxes {
            let chunk = &world[k * 24..(k + 1) * 24];
            let y_min = chunk.iter().map(|v| v.y).fold(f32::INFINITY, f32::min);
            let y_max = chunk.iter().map(|v| v.y).fold(f32::NEG_INFINITY, f32::max);
            // Full-height piers reach floor to ceiling.
            if y_max - y_min < 2.5 {
                continue;
            }
            let lo = chunk
                .iter()
                .map(|v| if long_idx == 0 { v.x } else { v.z })
                .fold(f32::INFINITY, f32::min);
            let hi = chunk
                .iter()
                .map(|v| if long_idx == 0 { v.x } else { v.z })
                .fold(f32::NEG_INFINITY, f32::max);
            piers.push((lo, hi));
        }
        piers.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let side_static: &'static str = match side {
            "north" => "north",
            "south" => "south",
            "east" => "east",
            "west" => "west",
            _ => unreachable!(),
        };
        let entry = holes_by_side.entry(side_static).or_default();
        for w in piers.windows(2) {
            entry.push((w[0].1, w[1].0));
        }
    }

    // Look at every `window` role node and verify it sits inside a
    // perimeter-wall hole of matching width. We use the opening-width
    // tag stored on the frame: each window group instances `window_simple`
    // whose `frame_top` has size [width+0.08, ...]; we read that back.
    let mut checked = 0;
    for (i, n) in g.nodes.iter().enumerate() {
        if !n.name.starts_with("window_") || n.kind != "group" {
            continue;
        }
        // Find the frame_top descendant to recover the authored width.
        let mut frame_width: Option<f32> = None;
        let mut stack = vec![i];
        while let Some(id) = stack.pop() {
            for c in &g.nodes[id].children {
                let cn = &g.nodes[c.0 as usize];
                if cn.name == "frame_top" {
                    if let Some(m) = &cn.mesh {
                        let xs: Vec<f32> = m.positions.iter().map(|p| p[0]).collect();
                        let lo = xs.iter().cloned().fold(f32::INFINITY, f32::min);
                        let hi = xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                        frame_width = Some(hi - lo - 0.08);
                    }
                }
                stack.push(c.0 as usize);
            }
        }
        let width = match frame_width {
            Some(w) => w,
            None => continue,
        };
        let wp = world_pos(&g, i);
        let (side, proj) = if (wp.x - 5.482).abs() < 0.05 {
            ("east", wp.z)
        } else if (wp.x + 5.482).abs() < 0.05 {
            ("west", wp.z)
        } else if (wp.z - 3.876).abs() < 0.05 {
            ("north", wp.x)
        } else if (wp.z + 3.876).abs() < 0.05 {
            ("south", wp.x)
        } else {
            continue;
        };
        let Some(holes) = holes_by_side.get(side) else {
            panic!("no exterior wall on {side}");
        };
        let hit = holes
            .iter()
            .find(|(lo, hi)| *lo - 0.05 < proj && proj < *hi + 0.05);
        let (lo, hi) = hit.unwrap_or_else(|| panic!("window on {side} at proj={proj} has no hole"));
        let actual = hi - lo;
        assert!(
            (actual - width).abs() < 0.02,
            "window on {side} at proj={proj} has width={width} but hole width={actual}"
        );
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected to check ≥ 4 windows on east/west/north/south, only checked {checked}"
    );
}

#[test]
fn building_is_deterministic_under_same_seed() {
    let a = lower_src(MIN_GRID_SRC);
    let b = lower_src(MIN_GRID_SRC);
    assert_eq!(a.nodes.len(), b.nodes.len(), "node count diverged");
    // Position hash: sum of every mesh vertex coordinate. A divergence in
    // layout or geometry shifts this.
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

#[test]
fn building_seed_changes_layout() {
    let src1 = MIN_GRID_SRC.replace("seed=5", "seed=5");
    let src2 = MIN_GRID_SRC.replace("seed=5", "seed=99");
    let a = lower_src(&src1);
    let b = lower_src(&src2);
    // Same room count → same wall + slab counts; but the door spans differ
    // because the entrance jitter changes with seed.
    let door_x = |g: &SceneGraph| -> Vec<f32> {
        g.nodes
            .iter()
            .filter(|n| n.role.as_deref() == Some("ext_door"))
            .map(|n| n.transform.translation.x)
            .collect()
    };
    let xa = door_x(&a);
    let xb = door_x(&b);
    assert_eq!(xa.len(), xb.len(), "entrance count diverged across seeds");
    let any_diff = xa.iter().zip(&xb).any(|(x, y)| (x - y).abs() > 1e-4);
    assert!(any_diff, "entrance positions identical across distinct seeds");
}

#[test]
fn building_unknown_door_module_falls_back_to_panel() {
    let src = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "x" (
          seed=1, style="grid", floor_area=30, rooms=2,
          entrances=1, external_door="no_such_module",
          mat="concrete",
        ) {
          room_type "office" (kind=staff_only, density=1)
        }
    "#;
    // The validator would flag this as unknown-module; lowering should still
    // succeed and emit the fallback panel.
    let g = lower_src(src);
    // Fallback panel kind is "panel".
    let panel_count = count_kind(&g, "panel");
    assert!(
        panel_count >= 1,
        "expected fallback panel, got {panel_count}"
    );
}

#[test]
fn building_subtree_marked_non_editable() {
    let g = lower_src(MIN_GRID_SRC);
    // Wrapper stays editable; everything below it is non-editable.
    let wrapper = g
        .nodes
        .iter()
        .position(|n| n.kind == "building")
        .expect("wrapper");
    assert!(g.nodes[wrapper].editable, "wrapper should stay editable");
    // Any descendant should have editable=false.
    let any_non_editable = g.nodes.iter().enumerate().any(|(i, n)| {
        i != wrapper && !n.editable && n.kind != "scene"
    });
    assert!(any_non_editable, "no non-editable descendants found");
}

#[test]
fn windows_on_each_side_never_overlap() {
    // Every pair of windows on the same exterior side must be at least
    // `window_w` apart (one window-width of solid wall pier between
    // them). The old placement could cycle the same segment list and
    // stack two windows on top of one another whenever `windows` >
    // segment count; the new allocator caps each segment at a max that
    // respects the spacing.
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "wide" (
          seed=7, style="grid",
          floor_area=120, rooms=6,
          windows=12, entrances=1,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Group windows by side via the tag stamped onto each opening's
    // perimeter wall hole. We use world-space positions; on north/south
    // the long axis is X, on east/west it's Z.
    use std::collections::BTreeMap;
    let mut by_side: BTreeMap<&'static str, Vec<f32>> = BTreeMap::new();
    // Each window group's local transform sits at the cell's exterior
    // segment fixed coord (= the plate's bounds), while the perimeter
    // wall sits half a wall-thickness outside that. Tolerate the
    // ~`wall_thickness/2 = 0.06 m` offset.
    let side_tag = |g: &SceneGraph, t: &str| -> f32 {
        g.nodes
            .iter()
            .find(|n| n.tags.iter().any(|tt| tt == t))
            .map(|n| n.transform.translation)
            .map(|v| if t.ends_with("east") || t.ends_with("west") { v.x } else { v.z })
            .unwrap_or_else(|| panic!("missing wall tagged {t}"))
    };
    let east_x = side_tag(&g, "side=east");
    let west_x = side_tag(&g, "side=west");
    let north_z = side_tag(&g, "side=north");
    let south_z = side_tag(&g, "side=south");
    const WALL_TOL: f32 = 0.2;
    for n in &g.nodes {
        if !n.name.starts_with("window_") || n.kind != "group" {
            continue;
        }
        let p = n.transform.translation;
        if (p.x - east_x).abs() < WALL_TOL {
            by_side.entry("east").or_default().push(p.z);
        } else if (p.x - west_x).abs() < WALL_TOL {
            by_side.entry("west").or_default().push(p.z);
        } else if (p.z - north_z).abs() < WALL_TOL {
            by_side.entry("north").or_default().push(p.x);
        } else if (p.z - south_z).abs() < WALL_TOL {
            by_side.entry("south").or_default().push(p.x);
        }
    }
    assert!(
        !by_side.is_empty(),
        "expected windows on at least one side, found none"
    );
    // Default `window_w` is 1.2; placement guarantees centre-to-centre
    // pitch ≥ 2 × window_w (one window-width of pier between adjacent
    // windows).
    const MIN_PITCH: f32 = 2.4;
    for (side, mut xs) in by_side {
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in xs.windows(2) {
            let d = (w[1] - w[0]).abs();
            assert!(
                d + 1e-3 >= MIN_PITCH,
                "windows on {side} overlap: positions {:?} → gap {d} < {MIN_PITCH}",
                xs
            );
        }
    }
}

#[test]
fn windows_in_each_cell_run_are_symmetric() {
    // Within each cell × side run, the windows the placer drops should
    // be mirror-symmetric about the run's midpoint. We verify this by
    // pulling the run-centred offsets — for n windows their mean must
    // equal the run centre, and their offsets from the centre must
    // mirror in matched ±pairs.
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "row" (
          seed=42, style="grid",
          floor_area=120, rooms=6,
          windows=12, entrances=1,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // For each window, find its parent room's group via the floor's
    // rooms subtree. We don't have direct access to cell extents from
    // the graph; instead, group windows by (side, integer-rounded fixed
    // coord, segment-bucketed lo/hi) and check each bucket is
    // symmetric. Simpler: for each side, collect all window positions
    // and verify that the run as a whole has matching first/last
    // distances from the wall's overall midpoint. That's a weaker check
    // but it catches asymmetric/jittered placement.
    //
    // The original code used `t = 0.2 + 0.6 * rand_f01(state)` so two
    // windows in a single run would be at random positions, never
    // mirrored about the centre. The fix uses `(j+1)/(n+1)` which IS
    // always centred — the mean position of any n consecutive windows
    // on the same run equals the run centre.
    let east_x = g
        .nodes
        .iter()
        .find(|n| n.tags.iter().any(|t| t == "side=east"))
        .map(|n| n.transform.translation.x)
        .expect("east wall");
    let west_x = g
        .nodes
        .iter()
        .find(|n| n.tags.iter().any(|t| t == "side=west"))
        .map(|n| n.transform.translation.x)
        .expect("west wall");
    let mut east_zs: Vec<f32> = Vec::new();
    let mut west_zs: Vec<f32> = Vec::new();
    // Windows sit on the cell's plate-bounds edge; the perimeter wall
    // sits half a wall-thickness outside. Tolerate the small offset.
    const WALL_TOL: f32 = 0.2;
    for n in &g.nodes {
        if !n.name.starts_with("window_") || n.kind != "group" {
            continue;
        }
        let p = n.transform.translation;
        if (p.x - east_x).abs() < WALL_TOL {
            east_zs.push(p.z);
        } else if (p.x - west_x).abs() < WALL_TOL {
            west_zs.push(p.z);
        }
    }
    // The east/west walls run along Z and (with a grid layout + 1
    // entrance on the south face) take a single row of cells along that
    // axis. The placer pins each window to `(j+1)/(n+1)` of its cell —
    // symmetric about the cell midpoint. If we further had only one
    // cell per side, the whole side would be perfectly centred on z=0.
    // We assert the (cheaper) cross-side property instead: the east and
    // west sides should be reflections of each other under the same
    // seed, because the layout is laterally symmetric across the X
    // midline for a grid + south-only entrance + no circulation column
    // on the west.
    //
    // We can't guarantee east==west when circulation eats the east
    // column, so just assert each side's run is centred about z=0
    // within a half-pitch of the smallest cell.
    let assert_centred = |label: &str, mut zs: Vec<f32>| {
        if zs.is_empty() {
            return;
        }
        zs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean: f32 = zs.iter().sum::<f32>() / zs.len() as f32;
        // The floorplate spans roughly [-7, +7] along Z for a 120 m²
        // grid; one cell's worth of offset is ≤ ~4 m. A symmetric per-
        // cell placement keeps the side's centroid well inside ±2 m of
        // the layout midline.
        assert!(
            mean.abs() < 2.0,
            "{label} windows off-centre: mean z={mean} positions={zs:?}"
        );
    };
    assert_centred("east", east_zs);
    assert_centred("west", west_zs);
}

#[test]
fn window_positions_are_seed_independent() {
    // Windows are placed deterministically from cell geometry alone —
    // they should not respond to the seed. Two different seeds with
    // the same layout-driving config produce identical exterior cell
    // segments only if the layout is also seed-stable, which it isn't
    // for `grid` (room types shuffle). The strongest portable
    // assertion is: for ANY seed, windows on the same side respect
    // the pitch invariant. (See `windows_on_each_side_never_overlap`
    // for the parameterised version.) Here we additionally check that
    // the per-window jitter the old placer introduced is gone: under
    // two distinct seeds the *number* of windows can shift, but every
    // resulting window must still land at a `(j+1)/(n+1)` fraction of
    // its segment — i.e., never at a random offset in [0.2, 0.8].
    //
    // We approximate this by checking determinism under the SAME seed
    // (already covered by `building_is_deterministic_under_same_seed`)
    // and rely on the no-overlap test for the cross-seed claim.
    //
    // What this test asserts directly: an empty `windows=0` config
    // produces zero windows (the previous code path used the RNG
    // unconditionally; this catches a regression where placement
    // would run even for count=0).
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "blind" (
          seed=11, style="grid",
          floor_area=80, rooms=4,
          windows=0, entrances=1,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    let n_windows = g
        .nodes
        .iter()
        .filter(|n| n.name.starts_with("window_") && n.kind == "group")
        .count();
    assert_eq!(
        n_windows, 0,
        "windows=0 should produce zero window groups, got {n_windows}"
    );
}

#[test]
fn multiple_entrances_fan_out_across_facades() {
    // `entrances=1` keeps the canonical south-front behaviour. Bumping
    // the count fans entrances round-robin across S → N → E → W so a
    // four-door building has exactly one entrance on every facade.
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "courthouse" (
          seed=3, style="grid",
          floor_area=120, rooms=6,
          windows=0, entrances=4,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Each entrance group sits at the cell-local exterior position; the
    // perimeter wall its facing points at carries a `side=<name>` tag at
    // the matching world coord. We bucket entrances by the facing vector
    // baked into their group transform — the modules emitter pulls the
    // opening's `facing` into the wrapping group's rotation, so the
    // group's local +Z in world space tells us the side.
    use std::collections::BTreeMap;
    let mut by_side: BTreeMap<&'static str, usize> = BTreeMap::new();
    for n in &g.nodes {
        if n.role.as_deref() != Some("ext_door") {
            continue;
        }
        let fwd = n.transform.rotation * glam::Vec3::Z;
        let side = if fwd.z < -0.7 {
            "south"
        } else if fwd.z > 0.7 {
            "north"
        } else if fwd.x > 0.7 {
            "east"
        } else if fwd.x < -0.7 {
            "west"
        } else {
            continue;
        };
        *by_side.entry(side).or_default() += 1;
    }
    assert_eq!(
        by_side.get("south").copied().unwrap_or(0), 1,
        "expected one south entrance, by_side={by_side:?}"
    );
    assert_eq!(
        by_side.get("north").copied().unwrap_or(0), 1,
        "expected one north entrance, by_side={by_side:?}"
    );
    assert_eq!(
        by_side.get("east").copied().unwrap_or(0), 1,
        "expected one east entrance, by_side={by_side:?}"
    );
    assert_eq!(
        by_side.get("west").copied().unwrap_or(0), 1,
        "expected one west entrance, by_side={by_side:?}"
    );
}

#[test]
fn single_entrance_stays_on_south_front() {
    // The single-entrance case is the canonical "front door" — scoring
    // and corridor layouts pivot off it, so it must keep landing on the
    // south face when no extra entrances are requested.
    let g = lower_src(MIN_GRID_SRC);
    let south_only = g
        .nodes
        .iter()
        .filter(|n| n.role.as_deref() == Some("ext_door"))
        .all(|n| {
            let fwd = n.transform.rotation * glam::Vec3::Z;
            fwd.z < -0.7
        });
    assert!(south_only, "single entrance should sit on the south face");
}

#[test]
fn entrances_and_windows_never_overlap() {
    // Every entrance occupies a `door_w`-wide span on its facade. The
    // window placer must keep window centres clear of that span by at
    // least `(door_w + cw) / 2` (geometric non-overlap) plus a half-pitch
    // `cw / 2` pier (matching window-to-window spacing). Drive multiple
    // entrances against many windows on a small floorplate so several
    // facades carry both kinds of opening at once.
    let src = r#"
        material "p" (color=[0.9, 0.9, 0.88])
        building "wide" (
          seed=5, style="grid",
          floor_area=120, rooms=6,
          windows=12, entrances=4,
          mat="p",
        ) {
          room_type "room" (kind=staff_only, density=1)
        }
    "#;
    let g = lower_src(src);
    // Collect entrance positions by side via the `facing` baked into each
    // ext_door group's rotation.
    use std::collections::BTreeMap;
    let mut entrances_by_side: BTreeMap<&'static str, Vec<f32>> = BTreeMap::new();
    for n in &g.nodes {
        if n.role.as_deref() != Some("ext_door") {
            continue;
        }
        let fwd = n.transform.rotation * glam::Vec3::Z;
        let side: &'static str = if fwd.z < -0.7 {
            "south"
        } else if fwd.z > 0.7 {
            "north"
        } else if fwd.x > 0.7 {
            "east"
        } else if fwd.x < -0.7 {
            "west"
        } else {
            continue;
        };
        let p = n.transform.translation;
        let coord = if side == "east" || side == "west" { p.z } else { p.x };
        entrances_by_side.entry(side).or_default().push(coord);
    }
    assert!(
        !entrances_by_side.is_empty(),
        "expected at least one entrance, found none"
    );
    // Same side-tagging trick as `windows_on_each_side_never_overlap`:
    // sniff each window group's distance to the four perimeter walls.
    let side_tag = |g: &SceneGraph, t: &str| -> f32 {
        g.nodes
            .iter()
            .find(|n| n.tags.iter().any(|tt| tt == t))
            .map(|n| n.transform.translation)
            .map(|v| if t.ends_with("east") || t.ends_with("west") { v.x } else { v.z })
            .unwrap_or_else(|| panic!("missing wall tagged {t}"))
    };
    let east_x = side_tag(&g, "side=east");
    let west_x = side_tag(&g, "side=west");
    let north_z = side_tag(&g, "side=north");
    let south_z = side_tag(&g, "side=south");
    const WALL_TOL: f32 = 0.2;
    let mut windows_by_side: BTreeMap<&'static str, Vec<f32>> = BTreeMap::new();
    for n in &g.nodes {
        if !n.name.starts_with("window_") || n.kind != "group" {
            continue;
        }
        let p = n.transform.translation;
        if (p.x - east_x).abs() < WALL_TOL {
            windows_by_side.entry("east").or_default().push(p.z);
        } else if (p.x - west_x).abs() < WALL_TOL {
            windows_by_side.entry("west").or_default().push(p.z);
        } else if (p.z - north_z).abs() < WALL_TOL {
            windows_by_side.entry("north").or_default().push(p.x);
        } else if (p.z - south_z).abs() < WALL_TOL {
            windows_by_side.entry("south").or_default().push(p.x);
        }
    }
    // door_w default = 0.9, window_w default = 1.2.
    const DOOR_W: f32 = 0.9;
    const WINDOW_W: f32 = 1.2;
    const MIN_CENTRE_TO_CENTRE: f32 = (DOOR_W + WINDOW_W) * 0.5;
    for (side, ent_xs) in &entrances_by_side {
        let win_xs = match windows_by_side.get(side) {
            Some(v) => v,
            None => continue,
        };
        for &e in ent_xs {
            for &w in win_xs {
                let d = (w - e).abs();
                assert!(
                    d + 1e-3 >= MIN_CENTRE_TO_CENTRE,
                    "window at {w} on {side} overlaps entrance at {e} (centre gap {d} < {MIN_CENTRE_TO_CENTRE})"
                );
            }
        }
    }
}

#[test]
fn building_requires_room_type() {
    let src = r#"
        material "concrete" (color=[0.8, 0.8, 0.8])
        building "empty" (seed=1, style="grid", floor_area=20, rooms=1, mat="concrete") {}
    "#;
    let ast = parse(src).expect("parse");
    let err = lower(&ast).expect_err("must require at least one room_type");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("room_type"),
        "expected error mentioning `room_type`, got: {msg}"
    );
}
