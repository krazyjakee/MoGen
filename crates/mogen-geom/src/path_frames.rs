//! Explicit parallel transport using the same frame representation as conform.
use crate::PathFrame;
use anyhow::{bail, Result};
use glam::{Quat, Vec3};

/// Project an authored height/surface-normal hint perpendicular to a tangent.
/// `normal` is height; `-binormal` is right-handed profile width (+X).
pub fn frame_from_up(center: Vec3, tangent: Vec3, up: Vec3) -> Result<PathFrame> {
    if !center.is_finite() || !tangent.is_finite() || !up.is_finite() {
        bail!("E0130: path frame contains non-finite values; supply finite points and frame_up");
    }
    let tangent = tangent.try_normalize().ok_or_else(|| {
        anyhow::anyhow!("E0130: zero path tangent; remove repeated points or a tangent reversal")
    })?;
    let up = up.try_normalize().ok_or_else(|| {
        anyhow::anyhow!("E0130: frame_up is zero; choose a nonzero height direction")
    })?;
    let projected = up - tangent * up.dot(tangent);
    if projected.length_squared() < 1e-8 {
        bail!("E0130: frame_up is parallel to the path tangent; choose a perpendicular height direction (for a vertical path, use [0,0,1])");
    }
    let normal = projected.normalize();
    Ok(PathFrame {
        center,
        tangent,
        normal,
        binormal: tangent.cross(normal).normalize(),
    })
}

/// Transport a height axis along already-sampled points. Closed paths include
/// the first point again at the end; residual loop twist is distributed by arc
/// length so the final frame exactly matches the first frame.
pub fn transport_path_frames(samples: &[Vec3], up: Vec3, closed: bool) -> Result<Vec<PathFrame>> {
    if samples.len() < if closed { 4 } else { 2 } {
        bail!("E0130: path needs at least two points, or three unique points for a closed loop");
    }
    if samples.iter().any(|p| !p.is_finite())
        || samples
            .windows(2)
            .any(|p| (p[1] - p[0]).length_squared() == 0.0)
    {
        bail!("E0130: path contains repeated adjacent or non-finite points; remove duplicates and correct coordinates");
    }
    let last = samples.len() - 1;
    if closed && samples[0] != samples[last] {
        bail!("E0130: closed sampled path must repeat its first point at the end");
    }
    let tangent = |i: usize| {
        if closed && (i == 0 || i == last) {
            samples[1] - samples[last - 1]
        } else {
            samples[(i + 1).min(last)] - samples[i.saturating_sub(1)]
        }
    };
    let mut first = frame_from_up(samples[0], tangent(0), up)?;
    let mut frames = vec![first];
    let mut arc = vec![0.0_f32];
    for i in 1..samples.len() {
        let t = tangent(i).try_normalize().ok_or_else(|| {
            anyhow::anyhow!(
                "E0130: undefined tangent at sample {i}; remove reversal or repeated points"
            )
        })?;
        if first.tangent.dot(t) < -0.9999 {
            bail!("E0130: tangent reverses at sample {i}; add a rounded transition");
        }
        let q = Quat::from_rotation_arc(first.tangent, t);
        first = frame_from_up(samples[i], t, q * first.normal)?;
        frames.push(first);
        arc.push(arc[i - 1] + samples[i].distance(samples[i - 1]));
    }
    if closed {
        let start = frames[0];
        let end = frames[last];
        let residual = start
            .tangent
            .dot(end.normal.cross(start.normal))
            .atan2(end.normal.dot(start.normal));
        for (i, frame) in frames.iter_mut().enumerate() {
            let q = Quat::from_axis_angle(frame.tangent, residual * arc[i] / arc[last]);
            frame.normal = q * frame.normal;
            frame.binormal = frame.tangent.cross(frame.normal).normalize();
        }
        frames[last] = start;
    }
    Ok(frames)
}
