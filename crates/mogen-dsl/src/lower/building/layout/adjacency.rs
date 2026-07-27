//! Where two rooms share a wall.
//!
//! Moved out of `emit/rooms.rs`, and reframed while it moved. The old version
//! asked "do these two rectangles touch, and on which of my four sides?" — a
//! question only a rectangle can answer. This asks "which stretches of these
//! two cells' boundaries are collinear and overlapping?", which is the same
//! question for a rectangle and still a question for an L-shaped room.
//!
//! That reframing is the whole point. Rooms stay rectangular for now — making
//! `RoomCell` polygonal has ~1,500 lines of consumers outside `layout/` and is
//! a rewrite disguised as a refactor — but this is the seam where that change
//! would land, and it is cheaper to cut the seam while the answers are still
//! checkable against the old code.
//!
//! Behaviour is identical to what it replaces, deliberately: same pair order,
//! same tolerance, same elevator exclusion, same overlap arithmetic.

use super::{CellKind, Floorplate, Rect2, RoomCell};

/// Which way a shared wall runs in plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WallAxis {
    /// The wall's plane is a constant X; the wall runs along Z.
    Vertical,
    /// The wall's plane is a constant Z; the wall runs along X.
    Horizontal,
}

/// One stretch of wall shared by two rooms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WallRun {
    /// Indices into [`Floorplate::rooms`], always `a < b`.
    pub a: usize,
    pub b: usize,
    pub axis: WallAxis,
    /// The shared face's position on the axis it is constant along.
    pub at: f32,
    /// Extent of the overlap along the wall's own direction.
    pub span: (f32, f32),
}

impl WallRun {
    pub fn length(&self) -> f32 {
        self.span.1 - self.span.0
    }

    pub fn midpoint(&self) -> f32 {
        0.5 * (self.span.0 + self.span.1)
    }
}

/// Faces closer than this are the same plane. Rooms are laid out by splitting
/// a plate, so shared faces agree exactly in principle — the tolerance covers
/// accumulated float error across a deep BSP, not genuine gaps.
const COINCIDENT: f32 = 1e-3;

/// Every wall run between two cells.
///
/// A `Vec` rather than an `Option` because two polygonal rooms can share more
/// than one stretch of boundary — think a C shape wrapped around a smaller
/// room. Rectangles never produce more than one, so this returns at most one
/// element today, and the callers do not need to know that.
pub(crate) fn runs_between(a: &Rect2, b: &Rect2, i: usize, j: usize) -> Vec<WallRun> {
    let mut out = Vec::new();
    let mut push = |axis, at, span: (f32, f32)| {
        if span.1 > span.0 {
            out.push(WallRun { a: i, b: j, axis, at, span });
        }
    };

    // Preserved verbatim from `collect_interior_walls`, including the
    // if/else-if chain: a pair of rectangles can only touch on one face, so
    // the first match is the answer and testing the rest would be wasted.
    if (a.x_max - b.x_min).abs() < COINCIDENT {
        push(WallAxis::Vertical, a.x_max, (a.z_min.max(b.z_min), a.z_max.min(b.z_max)));
    } else if (b.x_max - a.x_min).abs() < COINCIDENT {
        push(WallAxis::Vertical, b.x_max, (a.z_min.max(b.z_min), a.z_max.min(b.z_max)));
    } else if (a.z_max - b.z_min).abs() < COINCIDENT {
        push(WallAxis::Horizontal, a.z_max, (a.x_min.max(b.x_min), a.x_max.min(b.x_max)));
    } else if (b.z_max - a.z_min).abs() < COINCIDENT {
        push(WallAxis::Horizontal, b.z_max, (a.x_min.max(b.x_min), a.x_max.min(b.x_max)));
    }
    out
}

/// Every interior wall on a floorplate.
///
/// Pairs are visited in `(i, j)` order with `i < j`, which is load-bearing:
/// the emitted wall nodes are named from their position in this list, so
/// reordering it renames geometry.
pub(crate) fn interior_walls(plate: &Floorplate) -> Vec<WallRun> {
    let mut out = Vec::new();
    let n = plate.rooms.len();
    for i in 0..n {
        for j in (i + 1)..n {
            // Elevators emit their own four-sided shaft enclosure in
            // `emit/circulation.rs` (N/E/S solid plus W with one cutout per
            // storey at the elevator's centred opening). A cell-shared wall on
            // an elevator face would double the wall, and its door cutout —
            // which lands on the overlap midpoint, not the elevator centre —
            // would block the shaft's correctly-placed one.
            if matches!(plate.rooms[i].kind, CellKind::Elevator)
                || matches!(plate.rooms[j].kind, CellKind::Elevator)
            {
                continue;
            }
            out.extend(runs_between(&plate.rooms[i].rect, &plate.rooms[j].rect, i, j));
        }
    }
    out
}

impl RoomCell {
    /// The cell's own boundary, as the wall runs it would contribute if it
    /// stood alone.
    ///
    /// Unused by the generator today — interior walls come from
    /// [`interior_walls`], which needs both cells to know where the shared
    /// stretch is. It exists because it is the other half of the polygonal
    /// seam: a polygonal room's perimeter is a list of runs, and a rectangle's
    /// is these four. Having both sides of the interface expressed the same
    /// way is what makes the eventual change a substitution rather than a
    /// rewrite.
    #[allow(dead_code)]
    pub(crate) fn wall_runs(&self, index: usize) -> Vec<WallRun> {
        let r = &self.rect;
        vec![
            WallRun {
                a: index,
                b: index,
                axis: WallAxis::Vertical,
                at: r.x_min,
                span: (r.z_min, r.z_max),
            },
            WallRun {
                a: index,
                b: index,
                axis: WallAxis::Vertical,
                at: r.x_max,
                span: (r.z_min, r.z_max),
            },
            WallRun {
                a: index,
                b: index,
                axis: WallAxis::Horizontal,
                at: r.z_min,
                span: (r.x_min, r.x_max),
            },
            WallRun {
                a: index,
                b: index,
                axis: WallAxis::Horizontal,
                at: r.z_max,
                span: (r.x_min, r.x_max),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x_min: f32, z_min: f32, x_max: f32, z_max: f32) -> Rect2 {
        Rect2 { x_min, z_min, x_max, z_max }
    }

    #[test]
    fn two_rooms_side_by_side_share_a_vertical_wall() {
        let runs = runs_between(&rect(0.0, 0.0, 4.0, 3.0), &rect(4.0, 0.0, 7.0, 3.0), 0, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].axis, WallAxis::Vertical);
        assert_eq!(runs[0].at, 4.0);
        assert_eq!(runs[0].span, (0.0, 3.0));
    }

    #[test]
    fn two_rooms_stacked_share_a_horizontal_wall() {
        let runs = runs_between(&rect(0.0, 0.0, 4.0, 3.0), &rect(0.0, 3.0, 4.0, 6.0), 0, 1);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].axis, WallAxis::Horizontal);
        assert_eq!(runs[0].at, 3.0);
    }

    #[test]
    fn the_shared_run_is_only_the_overlap() {
        // A tall room beside a short one shares only the short one's extent.
        let runs = runs_between(&rect(0.0, 0.0, 4.0, 8.0), &rect(4.0, 2.0, 7.0, 5.0), 0, 1);
        assert_eq!(runs[0].span, (2.0, 5.0));
        assert_eq!(runs[0].length(), 3.0);
        assert_eq!(runs[0].midpoint(), 3.5);
    }

    #[test]
    fn rooms_that_only_touch_at_a_corner_share_nothing() {
        // Zero-length overlap is not a wall.
        let runs = runs_between(&rect(0.0, 0.0, 4.0, 3.0), &rect(4.0, 3.0, 7.0, 6.0), 0, 1);
        assert!(runs.is_empty(), "{runs:?}");
    }

    #[test]
    fn rooms_with_a_gap_between_them_share_nothing() {
        let runs = runs_between(&rect(0.0, 0.0, 4.0, 3.0), &rect(4.5, 0.0, 7.0, 3.0), 0, 1);
        assert!(runs.is_empty(), "{runs:?}");
    }

    #[test]
    fn order_does_not_change_which_face_is_found() {
        // Whichever way round the pair arrives, the wall is in the same place.
        let (a, b) = (rect(0.0, 0.0, 4.0, 3.0), rect(4.0, 0.0, 7.0, 3.0));
        let forward = runs_between(&a, &b, 0, 1);
        let backward = runs_between(&b, &a, 1, 0);
        assert_eq!(forward[0].at, backward[0].at);
        assert_eq!(forward[0].axis, backward[0].axis);
        assert_eq!(forward[0].span, backward[0].span);
    }

    #[test]
    fn a_cells_own_perimeter_is_four_runs() {
        let cell = RoomCell {
            rect: rect(0.0, 0.0, 4.0, 3.0),
            room_type_index: 0,
            kind: CellKind::Room,
            door_slots: Vec::new(),
        };
        let runs = cell.wall_runs(0);
        assert_eq!(runs.len(), 4);
        // Two of each orientation, and together they enclose the rectangle.
        assert_eq!(runs.iter().filter(|r| r.axis == WallAxis::Vertical).count(), 2);
        let perimeter: f32 = runs.iter().map(|r| r.length()).sum();
        assert_eq!(perimeter, 2.0 * (4.0 + 3.0));
    }
}
