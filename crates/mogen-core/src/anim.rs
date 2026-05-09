use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::NodeId;

/// Kinematic degree-of-freedom advertised by a node. In v1 every joint is
/// lowered to a node-transform animation track; the type only picks which
/// channel (rotation/translation) a clip's keyframes drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JointKind {
    /// 1 DOF rotation around `axis`; `from`/`to` are degrees.
    Hinge,
    /// 1 DOF translation along `axis`; `from`/`to` are meters.
    Slider,
    /// 3 DOF rotation; `from`/`to` are vec3 Euler degrees.
    Ball,
    /// Continuous 1 DOF rotation around `axis`; typically driven by `spin`.
    Rotor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Joint {
    pub name: String,
    pub kind: JointKind,
    pub axis: Vec3,
    /// Two-element range; meaning depends on `kind` (degrees or meters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f32; 2]>,
    /// Scene node whose transform this joint drives.
    pub pivot: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackProperty {
    Translation,
    Rotation,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interpolation {
    Linear,
    Step,
}

/// Easing curve applied between (or across) keyframes. v1 bakes easing into
/// dense LINEAR keyframes during lowering — glTF samplers themselves only
/// support `LINEAR`/`STEP`/`CUBICSPLINE`, so a non-linear easing is realised by
/// densifying the track and pre-evaluating the curve. Templates apply easing
/// to their procedural phase parameter; authored `track`s ease the value
/// between consecutive user keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    EaseInCubic,
    EaseOutCubic,
    EaseInOutCubic,
    EaseInSine,
    EaseOutSine,
    EaseInOutSine,
    EaseInBack,
    EaseOutBack,
    EaseInOutBack,
    EaseInBounce,
    EaseOutBounce,
    EaseInOutBounce,
}

impl Easing {
    /// Map a normalised time `t ∈ [0, 1]` to the eased fraction.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::EaseIn => t * t,
            Easing::EaseOut => {
                let u = 1.0 - t;
                1.0 - u * u
            }
            Easing::EaseInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = 1.0 - t;
                    1.0 - 2.0 * u * u
                }
            }
            Easing::EaseInCubic => t * t * t,
            Easing::EaseOutCubic => {
                let u = 1.0 - t;
                1.0 - u * u * u
            }
            Easing::EaseInOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let u = 1.0 - t;
                    1.0 - 4.0 * u * u * u
                }
            }
            Easing::EaseInSine => 1.0 - (t * std::f32::consts::FRAC_PI_2).cos(),
            Easing::EaseOutSine => (t * std::f32::consts::FRAC_PI_2).sin(),
            Easing::EaseInOutSine => 0.5 * (1.0 - (std::f32::consts::PI * t).cos()),
            Easing::EaseInBack => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                C3 * t * t * t - C1 * t * t
            }
            Easing::EaseOutBack => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                let u = t - 1.0;
                1.0 + C3 * u * u * u + C1 * u * u
            }
            Easing::EaseInOutBack => {
                const C1: f32 = 1.70158;
                const C2: f32 = C1 * 1.525;
                if t < 0.5 {
                    let f = 2.0 * t;
                    0.5 * (f * f * ((C2 + 1.0) * f - C2))
                } else {
                    let f = 2.0 * t - 2.0;
                    0.5 * (f * f * ((C2 + 1.0) * f + C2) + 2.0)
                }
            }
            Easing::EaseInBounce => 1.0 - bounce_out(1.0 - t),
            Easing::EaseOutBounce => bounce_out(t),
            Easing::EaseInOutBounce => {
                if t < 0.5 {
                    0.5 * (1.0 - bounce_out(1.0 - 2.0 * t))
                } else {
                    0.5 * (1.0 + bounce_out(2.0 * t - 1.0))
                }
            }
        }
    }

    pub fn is_linear(&self) -> bool {
        matches!(self, Easing::Linear)
    }

    /// Parse a DSL identifier or string. Accepts the canonical `snake_case`
    /// form plus a few common aliases so casual prompts ("ease", "in_out")
    /// still resolve.
    pub fn from_str(s: &str) -> Option<Easing> {
        Some(match s {
            "linear" | "none" => Easing::Linear,
            "ease" | "ease_in" | "in" => Easing::EaseIn,
            "ease_out" | "out" => Easing::EaseOut,
            "ease_in_out" | "in_out" => Easing::EaseInOut,
            "ease_in_cubic" | "in_cubic" => Easing::EaseInCubic,
            "ease_out_cubic" | "out_cubic" => Easing::EaseOutCubic,
            "ease_in_out_cubic" | "in_out_cubic" => Easing::EaseInOutCubic,
            "ease_in_sine" | "in_sine" => Easing::EaseInSine,
            "ease_out_sine" | "out_sine" => Easing::EaseOutSine,
            "ease_in_out_sine" | "in_out_sine" => Easing::EaseInOutSine,
            "ease_in_back" | "in_back" => Easing::EaseInBack,
            "ease_out_back" | "out_back" => Easing::EaseOutBack,
            "ease_in_out_back" | "in_out_back" => Easing::EaseInOutBack,
            "ease_in_bounce" | "in_bounce" => Easing::EaseInBounce,
            "ease_out_bounce" | "out_bounce" => Easing::EaseOutBounce,
            "ease_in_out_bounce" | "in_out_bounce" => Easing::EaseInOutBounce,
            _ => return None,
        })
    }
}

fn bounce_out(t: f32) -> f32 {
    const N1: f32 = 7.5625;
    const D1: f32 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let t = t - 1.5 / D1;
        N1 * t * t + 0.75
    } else if t < 2.5 / D1 {
        let t = t - 2.25 / D1;
        N1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / D1;
        N1 * t * t + 0.984375
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub node: NodeId,
    pub property: TrackProperty,
    pub interpolation: Interpolation,
    /// Easing applied between consecutive user-authored keyframes. The
    /// lowering pipeline calls [`Track::bake_easing`] at the end of each
    /// `clip`/template lower step, which densifies the track and resets this
    /// to [`Easing::Linear`] — by the time a `Track` reaches the exporter or
    /// renderer it always carries already-eased values under LINEAR sampling.
    /// Kept on the type (with a serde default for back-compat) so authoring
    /// tools can round-trip the directive.
    #[serde(default, skip_serializing_if = "Easing::is_linear")]
    pub easing: Easing,
    pub times: Vec<f32>,
    /// For Translation/Scale, each entry is `[x, y, z, 0]`.
    /// For Rotation, each entry is the quaternion `[x, y, z, w]`.
    pub values: Vec<[f32; 4]>,
}

impl Track {
    /// Bake `easing` into dense LINEAR keyframes. No-op for `Easing::Linear`
    /// or single-keyframe tracks. After baking, `easing` is reset to `Linear`
    /// so the operation is idempotent.
    pub fn bake_easing(&mut self) {
        if self.easing.is_linear() || self.times.len() < 2 {
            self.easing = Easing::Linear;
            return;
        }
        const SAMPLES_PER_SEGMENT: usize = 16;
        let easing = self.easing;
        let prop = self.property;
        let mut new_times = Vec::with_capacity((self.times.len() - 1) * SAMPLES_PER_SEGMENT + 1);
        let mut new_values = Vec::with_capacity(new_times.capacity());
        new_times.push(self.times[0]);
        new_values.push(self.values[0]);
        for i in 0..self.times.len() - 1 {
            let t0 = self.times[i];
            let t1 = self.times[i + 1];
            let v0 = self.values[i];
            let v1 = self.values[i + 1];
            for k in 1..=SAMPLES_PER_SEGMENT {
                let f = k as f32 / SAMPLES_PER_SEGMENT as f32;
                let eased = easing.apply(f);
                let t = t0 + f * (t1 - t0);
                let v = match prop {
                    TrackProperty::Rotation => {
                        let q0 = Quat::from_xyzw(v0[0], v0[1], v0[2], v0[3]);
                        let q1 = Quat::from_xyzw(v1[0], v1[1], v1[2], v1[3]);
                        let q = q0.slerp(q1, eased);
                        [q.x, q.y, q.z, q.w]
                    }
                    TrackProperty::Translation | TrackProperty::Scale => [
                        v0[0] + (v1[0] - v0[0]) * eased,
                        v0[1] + (v1[1] - v0[1]) * eased,
                        v0[2] + (v1[2] - v0[2]) * eased,
                        v0[3] + (v1[3] - v0[3]) * eased,
                    ],
                };
                new_times.push(t);
                new_values.push(v);
            }
        }
        self.times = new_times;
        self.values = new_values;
        self.easing = Easing::Linear;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub duration: f32,
    pub tracks: Vec<Track>,
    /// Canonical path of the imported `.mog` file this clip was lowered
    /// from. `None` when the clip was authored in the file currently being
    /// lowered. Used by tooling (e.g. MoGen Studio's inspector) to scope
    /// what's shown to the user — runtime export ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<std::path::PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_endpoints_are_pinned() {
        for e in [
            Easing::Linear,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::EaseInCubic,
            Easing::EaseOutCubic,
            Easing::EaseInOutCubic,
            Easing::EaseInSine,
            Easing::EaseOutSine,
            Easing::EaseInOutSine,
            Easing::EaseInBack,
            Easing::EaseOutBack,
            Easing::EaseInOutBack,
            Easing::EaseInBounce,
            Easing::EaseOutBounce,
            Easing::EaseInOutBounce,
        ] {
            assert!(e.apply(0.0).abs() < 1e-4, "{e:?} f(0) != 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-4, "{e:?} f(1) != 1");
        }
    }

    #[test]
    fn ease_in_out_is_symmetric_at_half() {
        for e in [
            Easing::EaseInOut,
            Easing::EaseInOutCubic,
            Easing::EaseInOutSine,
        ] {
            let v = e.apply(0.5);
            assert!((v - 0.5).abs() < 1e-3, "{e:?} f(0.5) ≈ 0.5, got {v}");
        }
    }

    #[test]
    fn ease_in_lags_linear_at_quarter() {
        // Quadratic ease_in must be below the diagonal in the first half.
        assert!(Easing::EaseIn.apply(0.25) < 0.25);
        // Mirror: ease_out leads.
        assert!(Easing::EaseOut.apply(0.25) > 0.25);
    }

    #[test]
    fn bake_easing_densifies_two_keyframes() {
        let mut t = Track {
            node: NodeId(0),
            property: TrackProperty::Translation,
            interpolation: Interpolation::Linear,
            easing: Easing::EaseInOut,
            times: vec![0.0, 1.0],
            values: vec![[0.0; 4], [10.0, 0.0, 0.0, 0.0]],
        };
        t.bake_easing();
        assert_eq!(t.easing, Easing::Linear);
        // 1 + 16 samples per segment.
        assert_eq!(t.times.len(), 17);
        assert!((t.times[0] - 0.0).abs() < 1e-6);
        assert!((t.times[16] - 1.0).abs() < 1e-6);
        // Mid-segment x-value lags 50% under ease_in_out — at t=0.25 the
        // eased fraction is 2*0.25^2 = 0.125, so x ≈ 1.25.
        let i_quarter = 4; // sample at f=0.25
        assert!((t.values[i_quarter][0] - 1.25).abs() < 1e-3);
    }

    #[test]
    fn bake_easing_is_noop_for_linear() {
        let mut t = Track {
            node: NodeId(0),
            property: TrackProperty::Translation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times: vec![0.0, 1.0],
            values: vec![[0.0; 4], [1.0, 0.0, 0.0, 0.0]],
        };
        t.bake_easing();
        assert_eq!(t.times.len(), 2);
    }

    #[test]
    fn bake_easing_is_idempotent() {
        let mut t = Track {
            node: NodeId(0),
            property: TrackProperty::Translation,
            interpolation: Interpolation::Linear,
            easing: Easing::EaseIn,
            times: vec![0.0, 1.0],
            values: vec![[0.0; 4], [1.0, 0.0, 0.0, 0.0]],
        };
        t.bake_easing();
        let after_first = t.times.len();
        t.bake_easing();
        assert_eq!(t.times.len(), after_first, "second bake must be a no-op");
    }

    #[test]
    fn easing_from_str_canonical_and_aliases() {
        assert_eq!(Easing::from_str("linear"), Some(Easing::Linear));
        assert_eq!(Easing::from_str("ease_in_out"), Some(Easing::EaseInOut));
        assert_eq!(Easing::from_str("in_out"), Some(Easing::EaseInOut));
        assert_eq!(Easing::from_str("ease"), Some(Easing::EaseIn));
        assert_eq!(Easing::from_str("bogus"), None);
    }
}
