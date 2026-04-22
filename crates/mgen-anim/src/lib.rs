//! Procedural animation templates.
//!
//! Each template produces a [`Clip`] whose tracks drive node transforms (no
//! skinning in v1). Templates take a target node id plus parameters and emit
//! keyframes that the glTF exporter writes as standard TRS channels.

use std::f32::consts::{PI, TAU};

use glam::{Quat, Vec3};
use mgen_core::{Clip, Interpolation, NodeId, Track, TrackProperty};

/// Continuous rotation at `rpm` around `axis`. Duration is one revolution so
/// the clip loops seamlessly. Five keyframes avoid shortest-arc ambiguity
/// between q(0) and q(2π).
pub fn spin(name: &str, target: NodeId, axis: Vec3, rpm: f32) -> Clip {
    let rpm = rpm.max(1e-3);
    let duration = 60.0 / rpm;
    let axis = axis.normalize_or(Vec3::Y);
    let steps = 4; // 5 samples including the endpoint.
    let mut times = Vec::with_capacity(steps + 1);
    let mut values = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = (i as f32) / (steps as f32);
        times.push(t * duration);
        let q = Quat::from_axis_angle(axis, t * TAU);
        values.push([q.x, q.y, q.z, q.w]);
    }
    Clip {
        name: name.to_string(),
        duration,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            times,
            values,
        }],
    }
}

/// Hinge-style open/close: angle 0 → `angle_deg` → 0 over `seconds`.
pub fn open_close(name: &str, target: NodeId, axis: Vec3, angle_deg: f32, seconds: f32) -> Clip {
    let seconds = seconds.max(1e-3);
    let axis = axis.normalize_or(Vec3::Y);
    let half = angle_deg.to_radians();
    let q0 = Quat::IDENTITY;
    let qh = Quat::from_axis_angle(axis, half);
    Clip {
        name: name.to_string(),
        duration: seconds,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Rotation,
            interpolation: Interpolation::Linear,
            times: vec![0.0, seconds * 0.5, seconds],
            values: vec![
                [q0.x, q0.y, q0.z, q0.w],
                [qh.x, qh.y, qh.z, qh.w],
                [q0.x, q0.y, q0.z, q0.w],
            ],
        }],
    }
}

/// Sinusoidal rotation around `axis`, oscillating ±`amplitude_deg` at `hz`.
/// Samples 16 keyframes per cycle; duration is one cycle.
pub fn wave(name: &str, target: NodeId, axis: Vec3, amplitude_deg: f32, hz: f32) -> Clip {
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
        let angle = amp * (t * TAU).sin();
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
            times,
            values,
        }],
    }
}

/// Bird/wing flap: wave-like oscillation, but biased to a single-direction
/// up-beat + down-beat profile. Currently identical to `wave` around `axis`;
/// reserved for asymmetric profiles.
pub fn flap(name: &str, target: NodeId, axis: Vec3, amplitude_deg: f32, hz: f32) -> Clip {
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
        // Skewed sinusoid: faster up (0..0.6), slower down (0.6..1.0).
        let phase = if t < 0.6 { (t / 0.6) * PI } else { PI + ((t - 0.6) / 0.4) * PI };
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
            times,
            values,
        }],
    }
}

/// Subtle breathing idle: scales the target uniformly by ±`amplitude`
/// at `hz`, returning to 1.0 at the loop boundary.
pub fn idle(name: &str, target: NodeId, amplitude: f32, hz: f32) -> Clip {
    let hz = hz.max(1e-3);
    let duration = 1.0 / hz;
    let samples = 16;
    let mut times = Vec::with_capacity(samples + 1);
    let mut values = Vec::with_capacity(samples + 1);
    for i in 0..=samples {
        let t = (i as f32) / (samples as f32);
        times.push(t * duration);
        let s = 1.0 + amplitude * (t * TAU).sin();
        values.push([s, s, s, 0.0]);
    }
    Clip {
        name: name.to_string(),
        duration,
        tracks: vec![Track {
            node: target,
            property: TrackProperty::Scale,
            interpolation: Interpolation::Linear,
            times,
            values,
        }],
    }
}
