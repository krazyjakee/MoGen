use serde::{Deserialize, Serialize};

/// Local-space axis along which a linear gradient interpolates.
///
/// Sampling happens against the owning mesh's local AABB so a `Y` gradient on
/// a tall, narrow object covers the full height regardless of world placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientAxis {
    X,
    #[default]
    Y,
    Z,
}

/// What shape the gradient sweep takes.
///
/// `Linear` interpolates along an axis-aligned direction in the mesh's local
/// frame. `Radial` interpolates outward from the mesh's local AABB centre to
/// the furthest corner. Both flavours can carry any number of stops — the
/// DSL keywords `linear` / `vertical` / `stops` / `radial` are all surface
/// sugar that lower to one of these two variants.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum GradientKind {
    Linear { axis: GradientAxis },
    Radial,
}

impl Default for GradientKind {
    fn default() -> Self {
        GradientKind::Linear { axis: GradientAxis::Y }
    }
}

/// One stop in a gradient ramp: a position in `[0, 1]` paired with an RGBA
/// colour. Stops are stored sorted by `t` so sampling can walk them in order.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub t: f32,
    pub color: [f32; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub kind: GradientKind,
    /// At least two stops, sorted by `t` in ascending order. Stops outside
    /// `[0, 1]` are clamped at sample time, not rejected, so authors can
    /// extrapolate fades without re-fitting the ramp.
    pub stops: Vec<GradientStop>,
}

impl Gradient {
    /// Sample the ramp at parameter `t`. Clamps to the first/last stop outside
    /// the `[0, 1]` range. Empty ramps return opaque white — the caller is
    /// expected to validate non-empty stops before baking, so this branch only
    /// fires for malformed in-memory graphs.
    pub fn sample(&self, t: f32) -> [f32; 4] {
        if self.stops.is_empty() {
            return [1.0, 1.0, 1.0, 1.0];
        }
        if t <= self.stops[0].t {
            return self.stops[0].color;
        }
        if t >= self.stops[self.stops.len() - 1].t {
            return self.stops[self.stops.len() - 1].color;
        }
        // Find the bracketing pair. `stops` is short (typically 2..4), so
        // linear scan beats a binary search and keeps the code simple.
        for w in self.stops.windows(2) {
            let a = &w[0];
            let b = &w[1];
            if t >= a.t && t <= b.t {
                let span = b.t - a.t;
                let u = if span <= f32::EPSILON { 0.0 } else { (t - a.t) / span };
                return [
                    a.color[0] + (b.color[0] - a.color[0]) * u,
                    a.color[1] + (b.color[1] - a.color[1]) * u,
                    a.color[2] + (b.color[2] - a.color[2]) * u,
                    a.color[3] + (b.color[3] - a.color[3]) * u,
                ];
            }
        }
        self.stops[self.stops.len() - 1].color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(t: f32, c: [f32; 4]) -> GradientStop {
        GradientStop { t, color: c }
    }

    #[test]
    fn sample_clamps_outside_range() {
        let g = Gradient {
            kind: GradientKind::default(),
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0, 1.0]),
                stop(1.0, [0.0, 0.0, 1.0, 1.0]),
            ],
        };
        assert_eq!(g.sample(-0.5), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(g.sample(1.5), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn sample_interpolates_midpoint() {
        let g = Gradient {
            kind: GradientKind::default(),
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0, 1.0]),
                stop(1.0, [0.0, 0.0, 1.0, 1.0]),
            ],
        };
        let mid = g.sample(0.5);
        assert!((mid[0] - 0.5).abs() < 1e-6);
        assert!((mid[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sample_handles_multi_stop() {
        let g = Gradient {
            kind: GradientKind::default(),
            stops: vec![
                stop(0.0, [1.0, 0.0, 0.0, 1.0]),
                stop(0.5, [0.0, 1.0, 0.0, 1.0]),
                stop(1.0, [0.0, 0.0, 1.0, 1.0]),
            ],
        };
        let q = g.sample(0.25);
        assert!((q[1] - 0.5).abs() < 1e-6);
        let q = g.sample(0.75);
        assert!((q[1] - 0.5).abs() < 1e-6);
        assert!((q[2] - 0.5).abs() < 1e-6);
    }
}
