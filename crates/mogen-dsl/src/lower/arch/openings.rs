//! Splitting a wall elevation around its openings.
//!
//! Given a wall's extents and a set of rectangular holes, work out the solid
//! pieces that remain: piers either side, a sill below each opening, a lintel
//! above, and the intermediate strips between vertically stacked openings.
//!
//! This is deliberately **mesh-free**. It answers "what solid rectangles are
//! left?" and nothing more, so the same planning serves a mesh builder, a
//! `.mog` text writer, and a mitred wall whose end faces are trapezoids rather
//! than square. Extracted from `building/emit/wall_build.rs`, whose algorithm
//! this is; that module is now a thin wrapper that turns these panels into
//! boxes.
//!
//! Wall-local frame: length on X, vertical on Y, thickness on Z. A hole is
//! `[along, cy, w, h]` — centre on X, centre on Y, then width and height.
//! Holes fully outside the wall are dropped; holes overlapping on X are
//! grouped so their shared span is cut once.

use super::consts::MIN_PANEL;

/// A solid rectangle of wall, in the wall's local elevation. Spans the full
/// thickness — walls are only ever cut through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Panel {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
}

impl Panel {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn centre_x(&self) -> f32 {
        0.5 * (self.x0 + self.x1)
    }

    pub fn centre_y(&self) -> f32 {
        0.5 * (self.y0 + self.y1)
    }

    /// Whether this panel is the whole wall — i.e. nothing was cut out.
    pub fn covers(&self, length: f32, height: f32) -> bool {
        let (hx, hy) = (0.5 * length, 0.5 * height);
        self.x0 == -hx && self.x1 == hx && self.y0 == -hy && self.y1 == hy
    }
}

/// Plan the solid panels of a wall elevation.
///
/// Returns a single full-extent panel when nothing is cut, and an empty vector
/// when the wall itself is degenerate. Panels come out in a fixed order — left
/// to right, then bottom to top within each column — so the geometry built from
/// them is reproducible.
pub(crate) fn solid_panels(length: f32, height: f32, holes: &[[f32; 4]]) -> Vec<Panel> {
    if length <= 0.0 || height <= 0.0 {
        return Vec::new();
    }

    let half_x = 0.5 * length;
    let half_y = 0.5 * height;
    let whole = Panel { x0: -half_x, x1: half_x, y0: -half_y, y1: half_y };

    if holes.is_empty() {
        return vec![whole];
    }

    // Clip each hole to the wall, discarding any that end up too small to be
    // worth cutting.
    let mut spans: Vec<(f32, f32, f32, f32)> = Vec::new();
    for &[along, cy, w, h] in holes {
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let x0 = (along - 0.5 * w).max(-half_x);
        let x1 = (along + 0.5 * w).min(half_x);
        if x1 - x0 < MIN_PANEL {
            continue;
        }
        let y0 = (cy - 0.5 * h).max(-half_y);
        let y1 = (cy + 0.5 * h).min(half_y);
        if y1 - y0 < MIN_PANEL {
            continue;
        }
        spans.push((x0, x1, y0, y1));
    }
    if spans.is_empty() {
        return vec![whole];
    }
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // Group X-overlapping holes into columns. Within a column the X range is
    // shared, but every Y range is kept: stacked holes — an elevator shaft's
    // per-storey doorways, say — must keep the wall between them.
    let mut columns: Vec<(f32, f32, Vec<(f32, f32)>)> = Vec::new();
    for (x0, x1, y0, y1) in spans {
        if let Some(last) = columns.last_mut() {
            if x0 <= last.1 + 1e-3 {
                last.1 = last.1.max(x1);
                last.2.push((y0, y1));
                continue;
            }
        }
        columns.push((x0, x1, vec![(y0, y1)]));
    }

    let mut out = Vec::new();
    let mut cursor = -half_x;
    for (x0, x1, mut ys) in columns {
        // Merge Y-overlapping holes inside the column — the same insurance
        // against rounding noise the X pass already applied.
        ys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut y_merged: Vec<(f32, f32)> = Vec::new();
        for (y0, y1) in ys {
            if let Some(last) = y_merged.last_mut() {
                if y0 <= last.1 + 1e-3 {
                    last.1 = last.1.max(y1);
                    continue;
                }
            }
            y_merged.push((y0, y1));
        }

        // Full-height pier between the previous column and this one.
        if x0 - cursor > 1e-3 {
            out.push(Panel { x0: cursor, x1: x0, y0: -half_y, y1: half_y });
        }

        // Walk bottom to top through the column: floor to the first opening
        // (sill), between each stacked pair, then the last opening to the top
        // (lintel).
        let mut prev_y = -half_y;
        for &(y0, y1) in &y_merged {
            if y0 - prev_y > 1e-3 {
                out.push(Panel { x0, x1, y0: prev_y, y1: y0 });
            }
            prev_y = y1;
        }
        if half_y - prev_y > 1e-3 {
            out.push(Panel { x0, x1, y0: prev_y, y1: half_y });
        }

        cursor = x1;
    }

    // Full-height pier after the last column.
    if half_x - cursor > 1e-3 {
        out.push(Panel { x0: cursor, x1: half_x, y0: -half_y, y1: half_y });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbroken_wall_is_one_panel() {
        let p = solid_panels(4.0, 2.5, &[]);
        assert_eq!(p.len(), 1);
        assert!(p[0].covers(4.0, 2.5));
    }

    #[test]
    fn degenerate_wall_yields_nothing() {
        assert!(solid_panels(0.0, 2.5, &[]).is_empty());
        assert!(solid_panels(4.0, -1.0, &[]).is_empty());
    }

    #[test]
    fn centred_hole_yields_four_panels() {
        // Left pier, sill, lintel, right pier.
        let p = solid_panels(4.0, 2.5, &[[0.0, 0.0, 1.0, 1.4]]);
        assert_eq!(p.len(), 4);
        // Total solid area is the wall minus the hole.
        let area: f32 = p.iter().map(|p| p.width() * p.height()).sum();
        assert!((area - (4.0 * 2.5 - 1.0 * 1.4)).abs() < 1e-4, "got {area}");
    }

    #[test]
    fn door_meeting_the_floor_has_no_sill() {
        let (h, door_h) = (2.6_f32, 2.1_f32);
        let cy = 0.5 * door_h - 0.5 * h;
        let p = solid_panels(4.0, h, &[[0.0, cy, 0.9, door_h]]);
        assert_eq!(p.len(), 3, "left pier, lintel, right pier");
        assert!(
            p.iter().all(|p| p.height() > 1e-3),
            "no zero-height panels: {p:?}"
        );
    }

    #[test]
    fn stacked_holes_keep_the_strip_between_them() {
        let (h, door_h) = (6.0_f32, 1.5_f32);
        let p = solid_panels(
            4.0,
            h,
            &[[0.0, -2.9 + 0.5 * door_h, 1.0, door_h], [0.0, 0.1 + 0.5 * door_h, 1.0, door_h]],
        );
        // Left pier, sill, middle strip, lintel, right pier.
        assert_eq!(p.len(), 5, "{p:?}");
    }

    #[test]
    fn hole_outside_the_wall_is_ignored() {
        let p = solid_panels(4.0, 2.5, &[[10.0, 0.0, 1.0, 1.0]]);
        assert_eq!(p.len(), 1);
        assert!(p[0].covers(4.0, 2.5));
    }

    #[test]
    fn panels_never_overlap() {
        let p = solid_panels(
            6.0,
            3.0,
            &[[-1.5, 0.0, 1.0, 1.2], [1.5, -0.4, 0.9, 2.1]],
        );
        for (i, a) in p.iter().enumerate() {
            for b in p.iter().skip(i + 1) {
                let x_apart = a.x1 <= b.x0 + 1e-4 || b.x1 <= a.x0 + 1e-4;
                let y_apart = a.y1 <= b.y0 + 1e-4 || b.y1 <= a.y0 + 1e-4;
                assert!(x_apart || y_apart, "panels overlap: {a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn panels_stay_within_the_wall() {
        let (l, h) = (5.0_f32, 2.8_f32);
        for p in solid_panels(l, h, &[[0.5, 0.2, 1.2, 1.0]]) {
            assert!(p.x0 >= -0.5 * l - 1e-4 && p.x1 <= 0.5 * l + 1e-4, "{p:?}");
            assert!(p.y0 >= -0.5 * h - 1e-4 && p.y1 <= 0.5 * h + 1e-4, "{p:?}");
        }
    }

    #[test]
    fn planning_is_reproducible() {
        let holes = [[-1.0, 0.0, 1.0, 1.2], [1.2, -0.3, 0.8, 2.0]];
        assert_eq!(solid_panels(6.0, 3.0, &holes), solid_panels(6.0, 3.0, &holes));
    }

    #[test]
    fn hole_order_does_not_change_the_result() {
        // The planner sorts, so a producer emitting holes in a different order
        // must still get identical geometry.
        let a = solid_panels(6.0, 3.0, &[[-1.0, 0.0, 1.0, 1.2], [1.2, -0.3, 0.8, 2.0]]);
        let b = solid_panels(6.0, 3.0, &[[1.2, -0.3, 0.8, 2.0], [-1.0, 0.0, 1.0, 1.2]]);
        assert_eq!(a, b);
    }
}
