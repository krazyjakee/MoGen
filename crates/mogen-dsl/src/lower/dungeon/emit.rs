//! Emit pass — turns a [`DungeonLayout`] into SceneGraph geometry.
//!
//! Every piece is a closed box, so the dungeon is watertight by construction:
//! there is no CSG and no open shell. Three kinds of solid are emitted:
//!
//! - **Decks**: a thin slab per occupied cell at each storey boundary. Deck `k`
//!   is the floor of level `k` and simultaneously the ceiling of level `k-1`, so
//!   a cell gets a slab where either level has floor there. Staircase openings
//!   punch the slab out so a flight can rise through it.
//! - **Walls**: a box on every exposed edge of a walkable cell (an edge facing a
//!   non-walkable neighbour or the grid border), spanning one storey's height.
//!   Walls overlap by a wall-thickness at corners so no corner can open a gap.
//! - **Steps**: a stacked run of boxes of increasing height that climbs one
//!   storey along a staircase's reserved cell run.
//!
//! Cells map to world space centred on the origin in XZ; each box is positioned
//! by its node transform, so the meshes themselves stay at the local origin.

use glam::Vec3;

use mogen_core::{ColliderShape, NodeId, SceneGraph, Transform, UvMode};
use mogen_geom::box_mesh;

use crate::ast::Node;
use crate::lower::material::bind_inherited_or_default;

use super::config::{ColliderMode, DungeonCfg};
use super::generate::DungeonLayout;
use super::materials::{FLOOR_MAT, STONE_MAT};

/// Four cardinal neighbours as (di, dj) for wall edge tests.
const DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

pub(super) fn emit(
    node: &Node,
    cfg: &DungeonCfg,
    layout: &DungeonLayout,
    parent: NodeId,
    graph: &mut SceneGraph,
) {
    let cell = cfg.cell;
    let room_h = cfg.size[1];
    let ft = cfg.floor_thickness;
    let pitch = room_h + ft;
    let gw = layout.gw;
    let gd = layout.gd;

    let half_w = gw as f32 * cell * 0.5;
    let half_d = gd as f32 * cell * 0.5;
    let cx = |i: i32| -half_w + (i as f32 + 0.5) * cell;
    let cz = |j: i32| -half_d + (j as f32 + 0.5) * cell;
    // Bottom of deck k (the slab top is the walking surface of level k).
    let deck_bottom = |k: usize| k as f32 * pitch;
    let deck_top = |k: usize| deck_bottom(k) + ft;

    let collide = cfg.colliders == ColliderMode::All;

    // Debug: render only one level (its floor slab + walls + touching stairs,
    // no ceiling), so a caller can peek inside one floor. Tag the wrapper
    // `floating` so the connectivity check skips the deliberately cut-away
    // subtree (stairs left dangling to unrendered levels would otherwise read
    // as disconnected clusters).
    let isolate = isolated_level(cfg, layout);
    if isolate.is_some() {
        graph.nodes[parent.0 as usize].tags.push("floating".to_string());
    }

    // --- decks (floors + ceilings) ----------------------------------------
    // levels + 1 deck planes; the topmost (k == levels) is the roof.
    let top_deck = layout.levels;
    for k in 0..=top_deck {
        // Isolation renders only the floor slab of the chosen level (no roof or
        // ceiling); otherwise drop the roof when debug_hide_roof is set.
        match isolate {
            Some(l) if k != l => continue,
            None if k == top_deck && cfg.debug_hide_roof => continue,
            _ => {}
        }
        for j in 0..gd {
            for i in 0..gw {
                // Ceiling of level k-1: cover every walkable cell, not just
                // room/corridor floor, so a stair landing (a walkable
                // `stair_cell`) gets a ceiling — and the top level gets a roof.
                let below = k >= 1 && layout.is_walkable(k - 1, i, j);
                let above = k < layout.levels && layout.is_floor(k, i, j); // floor of level k
                // When isolating, keep only the floor of the chosen level — not
                // the ceiling-of-(k-1) contribution the shared deck carries.
                if isolate.is_some() && !above {
                    continue;
                }
                if !(below || above) {
                    continue;
                }
                // ceilings=false: open-topped levels — skip any cell that would
                // be a pure ceiling (below=true, above=false). This covers both
                // intermediate decks and the top-level roof.
                if !cfg.ceilings && !above {
                    continue;
                }
                // A staircase punches the floor deck of the level it rises into.
                if k < layout.levels && layout.grids[k].opening[layout.idx(i, j)] {
                    continue;
                }
                let center = Vec3::new(cx(i), deck_bottom(k) + ft * 0.5, cz(j));
                // A walkable floor reads as flagstone; a pure ceiling/roof as stone.
                let (mat, role) = if above {
                    (FLOOR_MAT, "floor")
                } else {
                    (STONE_MAT, "ceiling")
                };
                add_box(
                    graph, parent, node, "deck", center, [cell, ft, cell], mat, role, collide,
                );
            }
        }
    }

    // --- walls -------------------------------------------------------------
    for level in 0..layout.levels {
        if matches!(isolate, Some(l) if level != l) {
            continue;
        }
        // Walls span the full storey pitch (floor-deck underside up to the next
        // floor-deck underside), not just the room clearance. This seals the
        // floor-thickness band at the storey boundary even where a staircase
        // opening removes the floor slab, and lets walls tile vertically across
        // levels with no slit.
        let wall_h = room_h + ft;
        let y_center = deck_bottom(level) + wall_h * 0.5;
        for j in 0..gd {
            for i in 0..gw {
                if !layout.is_walkable(level, i, j) {
                    continue;
                }
                for (di, dj) in DIRS {
                    if layout.is_walkable(level, i + di, j + dj) {
                        continue;
                    }
                    // Leave a gap in the perimeter wall where the exterior
                    // doorway is carved, so there is a clear way in.
                    if level == 0 {
                        if let Some(e) = layout.entrance {
                            if e.i == i && e.j == j && e.di == di && e.dj == dj {
                                continue;
                            }
                        }
                    }
                    // Wall sits on the shared edge, extended by a wall-thickness
                    // along its length so perpendicular walls overlap at corners.
                    let (center, size) = if di != 0 {
                        // East / West face: spans Z.
                        let x = cx(i) + di as f32 * cell * 0.5;
                        (
                            Vec3::new(x, y_center, cz(j)),
                            [cfg.wall_thickness, wall_h, cell + cfg.wall_thickness],
                        )
                    } else {
                        // North / South face: spans X.
                        let z = cz(j) + dj as f32 * cell * 0.5;
                        (
                            Vec3::new(cx(i), y_center, z),
                            [cell + cfg.wall_thickness, wall_h, cfg.wall_thickness],
                        )
                    };
                    add_box(
                        graph, parent, node, "wall", center, size, STONE_MAT, "wall", collide,
                    );
                }
            }
        }
    }

    // --- staircase steps ---------------------------------------------------
    // Fill each reserved flight with a run of small, game-realistic steps: a
    // ~0.18 m riser climbs across the run as a stack of solid boxes (watertight
    // by construction) rather than one cell-sized mega-block per step.
    const TARGET_RISER: f32 = 0.18;
    for stair in &layout.stairs {
        // When isolating, keep only the flights that touch the rendered level.
        if matches!(isolate, Some(l) if stair.lower_level != l && stair.lower_level + 1 != l) {
            continue;
        }
        let base_y = deck_top(stair.lower_level);
        let rise = pitch; // one storey, floor surface to floor surface
        let first = stair.cells.first().copied().unwrap_or((0, 0));
        let last = stair.cells.last().copied().unwrap_or(first);
        // Unit run direction in cells (one axis is ±1, the other 0).
        let di = (last.0 - first.0).signum();
        let dj = (last.1 - first.1).signum();
        let length = stair.cells.len() as f32 * cell; // horizontal run, metres
        // Foot edge: the low end is the outer face of the first cell.
        let x_start = cx(first.0) - di as f32 * cell * 0.5;
        let z_start = cz(first.1) - dj as f32 * cell * 0.5;
        let n_steps = (rise / TARGET_RISER).round().max(2.0) as usize;
        let tread = length / n_steps as f32;
        for s in 0..n_steps {
            // Each step is a solid riser from the lower floor up to its top.
            let top = base_y + rise * (s + 1) as f32 / n_steps as f32;
            let h = (top - base_y).max(ft);
            let along = (s as f32 + 0.5) * tread;
            let center = Vec3::new(
                x_start + di as f32 * along,
                base_y + h * 0.5,
                z_start + dj as f32 * along,
            );
            // Step spans `tread` along the run and a full cell across it.
            let size = if di != 0 {
                [tread, h, cell]
            } else {
                [cell, h, tread]
            };
            add_box(
                graph, parent, node, "step", center, size, FLOOR_MAT, "stair", collide,
            );
        }
    }
}

/// The single level to render when `debug_render_floor` names a valid level
/// (`0..levels`), else `None` (render every level). Mirrors building's
/// `debug_render_floor`, but dungeon levels are unsigned and 0-indexed.
fn isolated_level(cfg: &DungeonCfg, layout: &DungeonLayout) -> Option<usize> {
    cfg.debug_render_floor
        .and_then(|t| (t >= 0 && (t as usize) < layout.levels).then_some(t as usize))
}

#[allow(clippy::too_many_arguments)]
fn add_box(
    graph: &mut SceneGraph,
    parent: NodeId,
    node: &Node,
    name: &str,
    center: Vec3,
    size: [f32; 3],
    default_mat: &str,
    role: &str,
    collide: bool,
) {
    let id = graph.add_child(parent, name.to_string(), "mesh", Transform::from_translation(center));
    graph.set_mesh(id, box_mesh(size, UvMode::Tile));
    {
        let n = &mut graph.nodes[id.0 as usize];
        n.origin = node.origin.clone();
        n.role = Some(role.to_string());
        n.tags.extend(["dungeon".to_string(), role.to_string()]);
        if collide {
            n.collider = Some(ColliderShape::Trimesh);
        }
    }
    bind_material(id, default_mat, node.origin.as_deref(), graph);
}

fn bind_material(
    id: NodeId,
    default_name: &str,
    origin: Option<&std::path::Path>,
    graph: &mut SceneGraph,
) {
    bind_inherited_or_default(id, default_name, origin, graph);
}
