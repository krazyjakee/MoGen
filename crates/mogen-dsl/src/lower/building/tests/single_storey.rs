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
