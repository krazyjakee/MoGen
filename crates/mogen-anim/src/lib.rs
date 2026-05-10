//! Procedural animation templates.
//!
//! Each template produces a [`Clip`] whose tracks drive node transforms (no
//! skinning in v1). Templates take a target node id plus parameters and emit
//! keyframes that the glTF exporter writes as standard TRS channels.
//!
//! Easing is applied to the procedural **phase** parameter (the normalised
//! time `t ∈ [0, 1]` that drives the underlying curve), not as a post-bake
//! per-segment warp. That gives the visually expected behaviour for one-shot
//! animations like `open_close` (start slow, end slow) and for loop boundary
//! ramps. The resulting `Track` carries `easing = Easing::Linear` because the
//! curve has already been pre-warped — downstream consumers see a dense
//! LINEAR sampling.

use std::f32::consts::{PI, TAU};

use glam::{Quat, Vec3};
use mogen_core::{Clip, Easing, Interpolation, NodeId, Track, TrackProperty};

/// Continuous rotation at `rpm` around `axis`. Duration is one revolution so
/// the clip loops seamlessly. With `Easing::Linear` we use the historical
/// 5-sample track (avoids shortest-arc ambiguity between q(0) and q(2π));
/// non-linear easing densifies to 32 samples so the phase warp is visible.
pub fn spin(name: &str, target: NodeId, axis: Vec3, rpm: f32, easing: Easing) -> Clip {
    let rpm = rpm.max(1e-3);
    let duration = 60.0 / rpm;
    let axis = axis.normalize_or(Vec3::Y);
    let steps = if easing.is_linear() { 4 } else { 32 };
    let mut times = Vec::with_capacity(steps + 1);
    let mut values = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = (i as f32) / (steps as f32);
        times.push(t * duration);
        let phase = easing.apply(t);
        let q = Quat::from_axis_angle(axis, phase * TAU);
        values.push([q.x, q.y, q.z, q.w]);
    }
    Clip {
        name: name.to_string(),
        duration,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times,
            values,
            source_span: None,
        }],
        origin: None,
    }
}

/// Hinge-style open/close: angle 0 → `angle_deg` → 0 over `seconds`. With
/// non-linear easing, samples the eased phase densely so the curve is
/// visible (e.g. `ease_in_out_back` overshoots near the apex).
pub fn open_close(
    name: &str,
    target: NodeId,
    axis: Vec3,
    angle_deg: f32,
    seconds: f32,
    easing: Easing,
) -> Clip {
    let seconds = seconds.max(1e-3);
    let axis = axis.normalize_or(Vec3::Y);
    let half = angle_deg.to_radians();
    let (times, values) = if easing.is_linear() {
        let q0 = Quat::IDENTITY;
        let qh = Quat::from_axis_angle(axis, half);
        (
            vec![0.0, seconds * 0.5, seconds],
            vec![
                [q0.x, q0.y, q0.z, q0.w],
                [qh.x, qh.y, qh.z, qh.w],
                [q0.x, q0.y, q0.z, q0.w],
            ],
        )
    } else {
        let steps = 32;
        let mut times = Vec::with_capacity(steps + 1);
        let mut values = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let t = (i as f32) / (steps as f32);
            times.push(t * seconds);
            // Triangle phase 0→1→0 that easing warps in each half.
            let tri = if t < 0.5 {
                easing.apply(t * 2.0)
            } else {
                easing.apply((1.0 - t) * 2.0)
            };
            let q = Quat::from_axis_angle(axis, tri * half);
            values.push([q.x, q.y, q.z, q.w]);
        }
        (times, values)
    };
    Clip {
        name: name.to_string(),
        duration: seconds,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times,
            values,
            source_span: None,
        }],
        origin: None,
    }
}

/// Sinusoidal rotation around `axis`, oscillating ±`amplitude_deg` at `hz`.
/// Samples 16 keyframes per cycle; duration is one cycle. Easing warps the
/// in-cycle phase, so e.g. `ease_in_out` produces an asymmetric wobble that
/// dwells at the extremes.
pub fn wave(
    name: &str,
    target: NodeId,
    axis: Vec3,
    amplitude_deg: f32,
    hz: f32,
    easing: Easing,
) -> Clip {
    let hz = hz.max(1e-3);
    let duration = 1.0 / hz;
    let axis = axis.normalize_or(Vec3::Z);
    let amp = amplitude_deg.to_radians();
    let samples = 16;
    let mut times = Vec::with_capacity(samples + 1);
    let mut values = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let t = (i as f32) / (samples as f32);
        times.push(t * duration);
        let phase = easing.apply(t);
        let angle = amp * (phase * TAU).sin();
        let q = Quat::from_axis_angle(axis, angle);
        values.push([q.x, q.y, q.z, q.w]);
    }
    Clip {
        name: name.to_string(),
        duration,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times,
            values,
            source_span: None,
        }],
        origin: None,
    }
}

/// Bird/wing flap: wave-like oscillation, but biased to a single-direction
/// up-beat + down-beat profile. Currently identical to `wave` around `axis`;
/// reserved for asymmetric profiles.
pub fn flap(
    name: &str,
    target: NodeId,
    axis: Vec3,
    amplitude_deg: f32,
    hz: f32,
    easing: Easing,
) -> Clip {
    // Asymmetric profile: 60% up-stroke, 40% down-stroke. Sample analytically.
    let hz = hz.max(1e-3);
    let duration = 1.0 / hz;
    let axis = axis.normalize_or(Vec3::Z);
    let amp = amplitude_deg.to_radians();
    let samples = 16;
    let mut times = Vec::with_capacity(samples + 1);
    let mut values = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let t = (i as f32) / (samples as f32);
        times.push(t * duration);
        let warped = easing.apply(t);
        // Skewed sinusoid: faster up (0..0.6), slower down (0.6..1.0).
        let phase = if warped < 0.6 {
            (warped / 0.6) * PI
        } else {
            PI + ((warped - 0.6) / 0.4) * PI
        };
        let angle = amp * phase.sin();
        let q = Quat::from_axis_angle(axis, angle);
        values.push([q.x, q.y, q.z, q.w]);
    }
    Clip {
        name: name.to_string(),
        duration,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times,
            values,
            source_span: None,
        }],
        origin: None,
    }
}

/// Subtle breathing idle: scales the target uniformly by ±`amplitude`
/// at `hz`, returning to 1.0 at the loop boundary.
pub fn idle(name: &str, target: NodeId, amplitude: f32, hz: f32, easing: Easing) -> Clip {
    let hz = hz.max(1e-3);
    let duration = 1.0 / hz;
    let samples = 16;
    let mut times = Vec::with_capacity(samples + 1);
    let mut values = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let t = (i as f32) / (samples as f32);
        times.push(t * duration);
        let phase = easing.apply(t);
        let s = 1.0 + amplitude * (phase * TAU).sin();
        values.push([s, s, s, 0.0]);
    }
    Clip {
        name: name.to_string(),
        duration,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Scale,
            interpolation: Interpolation::Linear,
            easing: Easing::Linear,
            times,
            values,
            source_span: None,
        }],
        origin: None,
    }
}
