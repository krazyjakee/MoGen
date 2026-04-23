use glam::{Mat4, Quat, Vec3};
use mogen_core::{Clip, Interpolation, NodeId, SceneGraph, Track, TrackProperty, Transform};

/// Fold `clip` sampled at time `t` into `locals`. Later calls override earlier
/// ones per (node, property), so driving the same node from multiple active
/// clips is deterministic: the last one wins.
pub fn apply_animation(clip: &Clip, t: f32, locals: &mut [Transform]) {
    for track in &clip.tracks {
        let idx = track.node.0 as usize;
        if idx >= locals.len() {
            continue;
        }
        let v = sample_track(track, t);
        let base = &mut locals[idx];
        match track.property {
            TrackProperty::Translation => base.translation = Vec3::new(v[0], v[1], v[2]),
            TrackProperty::Rotation => {
                base.rotation = Quat::from_xyzw(v[0], v[1], v[2], v[3]).normalize()
            }
            TrackProperty::Scale => base.scale = Vec3::new(v[0], v[1], v[2]),
        }
    }
}

pub fn world_transforms_from_locals(scene: &SceneGraph, locals: &[Transform]) -> Vec<Mat4> {
    let mut out = vec![Mat4::IDENTITY; scene.nodes.len()];
    for root in &scene.roots {
        walk_world(scene, *root, Mat4::IDENTITY, locals, &mut out);
    }
    out
}

fn walk_world(
    scene: &SceneGraph,
    id: NodeId,
    parent: Mat4,
    locals: &[Transform],
    out: &mut [Mat4],
) {
    let world = parent * locals[id.0 as usize].to_mat4();
    out[id.0 as usize] = world;
    for c in &scene.nodes[id.0 as usize].children {
        walk_world(scene, *c, world, locals, out);
    }
}

fn sample_track(track: &Track, t: f32) -> [f32; 4] {
    if track.times.is_empty() {
        return [0.0, 0.0, 0.0, 0.0];
    }
    let n = track.times.len();
    if n == 1 {
        return track.values[0];
    }
    let first = track.times[0];
    let last = track.times[n - 1];
    let t = t.clamp(first, last);
    let mut i = 0;
    while i + 1 < n && track.times[i + 1] < t {
        i += 1;
    }
    let i0 = i;
    let i1 = (i + 1).min(n - 1);
    let t0 = track.times[i0];
    let t1 = track.times[i1];
    let f = if (t1 - t0).abs() < 1e-6 {
        0.0
    } else {
        ((t - t0) / (t1 - t0)).clamp(0.0, 1.0)
    };
    let v0 = track.values[i0];
    let v1 = track.values[i1];
    match (track.property, track.interpolation) {
        (_, Interpolation::Step) => v0,
        (TrackProperty::Rotation, _) => {
            let q0 = Quat::from_xyzw(v0[0], v0[1], v0[2], v0[3]);
            let q1 = Quat::from_xyzw(v1[0], v1[1], v1[2], v1[3]);
            let q = q0.slerp(q1, f);
            [q.x, q.y, q.z, q.w]
        }
        _ => [
            v0[0] + (v1[0] - v0[0]) * f,
            v0[1] + (v1[1] - v0[1]) * f,
            v0[2] + (v1[2] - v0[2]) * f,
            v0[3] + (v1[3] - v0[3]) * f,
        ],
    }
}
