//! Lowering for `joint`, `clip`, and procedural-template DSL nodes.
//!
//! These nodes are declarative metadata on the scene — they do not produce
//! geometry, they produce [`mogen_core::Joint`] entries and [`mogen_core::Clip`]
//! tracks that the exporter emits as glTF animation channels.

use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mogen_core::{
    Clip, Interpolation, Joint, JointKind, NodeId, SceneGraph, Track, TrackProperty,
};
use mogen_anim as anim;

use crate::ast::{Node, Value};

/// Lower a top-level or scene-level `joint` node.
pub fn lower_joint(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("joint declaration requires a name"))?;
    let kind = joint_kind_attr(node)?;
    let axis = node.attr_vec3("axis").unwrap_or(Vec3::Y);
    let limits = node.attr_pair("limits");
    let pivot_ref = string_or_ident(node.attr("pivot"))
        .ok_or_else(|| anyhow!("joint \"{name}\" requires pivot=\"<node_name>\""))?;
    let pivot = graph
        .find_node(&pivot_ref)
        .ok_or_else(|| anyhow!("joint \"{name}\" pivot \"{pivot_ref}\" is not a scene node"))?;
    graph.joints.push(Joint {
        name,
        kind,
        axis,
        limits,
        pivot,
    });
    Ok(())
}

/// Lower a top-level or scene-level `clip` node.
pub fn lower_clip(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("clip declaration requires a name"))?;
    let seconds = node.attr_number("seconds").unwrap_or(1.0).max(1e-3);

    let mut tracks = Vec::new();
    for c in &node.children {
        if c.kind != "track" {
            bail!(
                "clip \"{name}\" children must be `track` nodes (got `{}`)",
                c.kind
            );
        }
        tracks.push(lower_track(c, graph, seconds)?);
    }

    graph.clips.push(Clip {
        name,
        duration: seconds,
        tracks,
    });
    Ok(())
}

fn lower_track(node: &Node, graph: &SceneGraph, duration: f32) -> Result<Track> {
    let target_name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("track requires a target name (joint or node)"))?;

    // Resolve target: prefer joint match, fall back to bare node match.
    if let Some(joint) = graph.find_joint(&target_name).cloned() {
        return lower_joint_track(node, &joint, duration);
    }

    let Some(node_id) = graph.find_node(&target_name) else {
        bail!(
            "track target \"{}\" is neither a joint nor a scene node",
            target_name
        );
    };

    // Direct node track: caller must supply an explicit property.
    let prop = string_or_ident(node.attr("prop"))
        .ok_or_else(|| anyhow!("direct-node track requires prop=\"translation|rotation|scale\""))?;
    let property = match prop.as_str() {
        "translation" | "pos" => TrackProperty::Translation,
        "rotation" | "rot" => TrackProperty::Rotation,
        "scale" => TrackProperty::Scale,
        other => bail!("unknown track prop `{other}`"),
    };
    let axis = node.attr_vec3("axis").unwrap_or(Vec3::Y);
    let (times, values) = sample_track(node, duration, property, axis)?;
    Ok(Track {
        node: node_id,
        property,
        interpolation: Interpolation::Linear,
        times,
        values,
    })
}

fn lower_joint_track(node: &Node, joint: &Joint, duration: f32) -> Result<Track> {
    let (property, axis) = match joint.kind {
        JointKind::Hinge | JointKind::Rotor | JointKind::Ball => {
            (TrackProperty::Rotation, joint.axis)
        }
        JointKind::Slider => (TrackProperty::Translation, joint.axis),
    };
    let (times, values) = sample_track(node, duration, property, axis)?;
    Ok(Track {
        node: joint.pivot,
        property,
        interpolation: Interpolation::Linear,
        times,
        values,
    })
}

/// Pick the right sampler: multi-keyframe `keys=[[t,v], ...]` if present,
/// otherwise the two-keyframe `from=`/`to=` shorthand.
fn sample_track(
    node: &Node,
    duration: f32,
    property: TrackProperty,
    axis: Vec3,
) -> Result<(Vec<f32>, Vec<[f32; 4]>)> {
    if node.attr("keys").is_some() {
        sample_keys(node, property, axis)
    } else {
        sample_2kf(node, duration, property, axis)
    }
}

/// Multi-keyframe sampler. Reads `keys=[[t,v], ...]` where `t` is seconds and
/// `v` is a scalar interpreted the same way as `from`/`to` (degrees for
/// rotation, meters for translation, uniform factor for scale). Emits one
/// keyframe per entry. Times must be non-negative and strictly ascending.
fn sample_keys(
    node: &Node,
    property: TrackProperty,
    axis: Vec3,
) -> Result<(Vec<f32>, Vec<[f32; 4]>)> {
    let pairs = node
        .attr_list_pair("keys")
        .ok_or_else(|| anyhow!("track `keys=` must be a list of [time, value] pairs"))?;
    if pairs.len() < 2 {
        bail!("track `keys=` must have at least 2 keyframes (got {})", pairs.len());
    }
    if pairs[0][0] < 0.0 {
        bail!("track `keys=` times must be non-negative (first time = {})", pairs[0][0]);
    }
    for w in pairs.windows(2) {
        if w[1][0] <= w[0][0] {
            bail!(
                "track `keys=` times must be strictly ascending (got {} then {})",
                w[0][0],
                w[1][0]
            );
        }
    }
    let times: Vec<f32> = pairs.iter().map(|p| p[0]).collect();
    let values: Vec<[f32; 4]> = pairs.iter().map(|p| encode_value(property, axis, p[1])).collect();
    Ok((times, values))
}

/// Two-keyframe sampler. Reads `from`/`to` scalar (angle in degrees for
/// rotation; distance for translation; uniform factor for scale) and emits
/// keyframes at `[0, duration]`.
fn sample_2kf(
    node: &Node,
    duration: f32,
    property: TrackProperty,
    axis: Vec3,
) -> Result<(Vec<f32>, Vec<[f32; 4]>)> {
    let from = node.attr_number("from").unwrap_or(0.0);
    let to = node
        .attr_number("to")
        .ok_or_else(|| anyhow!("track requires `to=` scalar (or `keys=[[t,v], ...]`)"))?;
    let times = vec![0.0, duration];
    let values = vec![encode_value(property, axis, from), encode_value(property, axis, to)];
    Ok((times, values))
}

/// Pack a scalar keyframe value into the `[f32; 4]` storage that `Track::values`
/// uses, applying the right encoding for each property.
fn encode_value(property: TrackProperty, axis: Vec3, v: f32) -> [f32; 4] {
    match property {
        TrackProperty::Rotation => {
            let a = axis.normalize_or(Vec3::Y);
            let q = Quat::from_axis_angle(a, v.to_radians());
            [q.x, q.y, q.z, q.w]
        }
        TrackProperty::Translation => {
            let a = axis.normalize_or(Vec3::Y);
            let p = a * v;
            [p.x, p.y, p.z, 0.0]
        }
        TrackProperty::Scale => [v, v, v, 0.0],
    }
}

/// Lower a procedural-template node (spin / open_close / wave / flap / idle).
pub fn lower_template(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let clip_name = node
        .name
        .clone()
        .unwrap_or_else(|| format!("{}_clip", node.kind));
    let target_ref = string_or_ident(node.attr("target"))
        .ok_or_else(|| anyhow!("`{}` requires target=\"<name>\"", node.kind))?;
    let targets = resolve_anim_targets(&target_ref, graph);
    if targets.is_empty() {
        bail!(
            "`{}` target \"{}\" is neither a joint nor a scene node",
            node.kind,
            target_ref
        );
    }
    // A joint target re-uses the joint's own axis when the caller didn't override.
    let axis_default = graph
        .find_joint(&target_ref)
        .map(|j| j.axis)
        .unwrap_or(Vec3::Y);
    let axis = node.attr_vec3("axis").unwrap_or(axis_default);

    let multi = targets.len() > 1;
    for (i, target) in targets.into_iter().enumerate() {
        let name = if multi {
            format!("{clip_name}_{i}")
        } else {
            clip_name.clone()
        };
        let clip = match node.kind.as_str() {
            "spin" => {
                let rpm = node.attr_number("rpm").unwrap_or(60.0);
                anim::spin(&name, target, axis, rpm)
            }
            "open_close" => {
                let angle = node.attr_number("angle").unwrap_or(90.0);
                let seconds = node.attr_number("seconds").unwrap_or(1.0);
                anim::open_close(&name, target, axis, angle, seconds)
            }
            "wave" => {
                let amp = node.attr_number("amplitude").unwrap_or(15.0);
                let hz = node.attr_number("hz").unwrap_or(1.0);
                anim::wave(&name, target, axis, amp, hz)
            }
            "flap" => {
                let amp = node.attr_number("amplitude").unwrap_or(30.0);
                let hz = node.attr_number("hz").unwrap_or(2.0);
                anim::flap(&name, target, axis, amp, hz)
            }
            "idle" => {
                let amp = node.attr_number("amplitude").unwrap_or(0.02);
                let hz = node.attr_number("hz").unwrap_or(0.5);
                anim::idle(&name, target, amp, hz)
            }
            other => bail!("unknown animation template `{other}`"),
        };
        graph.clips.push(clip);
    }
    Ok(())
}

/// Resolve an anim `target=` reference to one or more scene nodes.
///
/// Precedence: joint name → every node sharing `name` → every node sharing
/// `role`. The multi-match name pass lets `array`/`mirror` replicants (which
/// keep their source names) all receive the same procedural clip.
fn resolve_anim_targets(target_ref: &str, graph: &SceneGraph) -> Vec<NodeId> {
    if let Some(j) = graph.find_joint(target_ref) {
        return vec![j.pivot];
    }
    let by_name = graph.find_nodes_by_name(target_ref);
    if !by_name.is_empty() {
        return by_name;
    }
    graph.find_nodes_by_role(target_ref)
}

fn joint_kind_attr(node: &Node) -> Result<JointKind> {
    let s = string_or_ident(node.attr("type"))
        .ok_or_else(|| anyhow!("joint requires type=hinge|slider|ball|rotor"))?;
    match s.as_str() {
        "hinge" => Ok(JointKind::Hinge),
        "slider" => Ok(JointKind::Slider),
        "ball" => Ok(JointKind::Ball),
        "rotor" => Ok(JointKind::Rotor),
        other => bail!("unknown joint type `{other}`"),
    }
}

fn string_or_ident(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) | Value::Ident(s) => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;
    use crate::lower::lower;

    #[test]
    fn track_with_keys_emits_one_keyframe_per_entry() {
        let g = lower(&parse(
            r#"
            scene {
              box "hip" (size=[0.1, 0.1, 0.1])
            }
            clip "walk" (seconds=1.0) {
              track "hip" (prop=rotation, keys=[[0, -25], [0.25, 0], [0.5, 25], [0.75, 0], [1.0, -25]])
            }
            "#,
        ).expect("parse")).expect("lower");
        assert_eq!(g.clips.len(), 1);
        let clip = &g.clips[0];
        assert_eq!(clip.name, "walk");
        assert_eq!(clip.tracks.len(), 1);
        let t = &clip.tracks[0];
        assert_eq!(t.times, vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(t.values.len(), 5);
        // +25° and -25° rotations around Y should produce quaternions with
        // opposite-signed Y components.
        assert!(t.values[0][1] < 0.0);
        assert!(t.values[2][1] > 0.0);
    }

    #[test]
    fn track_without_to_or_keys_errors() {
        let err = lower(&parse(
            r#"
            scene { box "hip" (size=[0.1, 0.1, 0.1]) }
            clip "c" (seconds=1.0) {
              track "hip" (prop=rotation)
            }
            "#,
        ).expect("parse")).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("`to=`") && msg.contains("keys="), "got: {msg}");
    }

    #[test]
    fn track_keys_rejects_non_ascending_times() {
        let err = lower(&parse(
            r#"
            scene { box "hip" (size=[0.1, 0.1, 0.1]) }
            clip "c" (seconds=1.0) {
              track "hip" (prop=rotation, keys=[[0, 0], [0.5, 10], [0.25, 20]])
            }
            "#,
        ).expect("parse")).unwrap_err();
        assert!(format!("{err}").contains("ascending"));
    }

    #[test]
    fn track_keys_rejects_single_keyframe() {
        let err = lower(&parse(
            r#"
            scene { box "hip" (size=[0.1, 0.1, 0.1]) }
            clip "c" (seconds=1.0) {
              track "hip" (prop=rotation, keys=[[0, 0]])
            }
            "#,
        ).expect("parse")).unwrap_err();
        assert!(format!("{err}").contains("at least 2"));
    }

    #[test]
    fn track_axis_routes_rotation_to_the_requested_axis() {
        // Rotating around +X should produce a quat with non-zero X and zero Y.
        let g = lower(&parse(
            r#"
            scene { box "hip_l" (size=[0.1, 0.1, 0.1]) }
            clip "walk" (seconds=1.0) {
              track "hip_l" (prop=rotation, axis=[1, 0, 0],
                             keys=[[0, -25], [0.5, 25], [1.0, -25]])
            }
            "#,
        ).expect("parse")).expect("lower");
        let t = &g.clips[0].tracks[0];
        // First key: -25° around X → quat.x < 0, quat.y = 0.
        assert!(t.values[0][0] < 0.0, "expected non-zero X component, got {:?}", t.values[0]);
        assert!(t.values[0][1].abs() < 1e-5, "Y should be ~0 for pure X rotation");
    }

    #[test]
    fn track_two_keyframe_from_to_still_works() {
        // Regression: the classic two-keyframe path must still work when
        // `keys=` is absent.
        let g = lower(&parse(
            r#"
            scene { box "door" (size=[0.9, 2.0, 0.05]) }
            clip "swing" (seconds=0.8) {
              track "door" (prop=rotation, from=0, to=85)
            }
            "#,
        ).expect("parse")).expect("lower");
        let t = &g.clips[0].tracks[0];
        assert_eq!(t.times, vec![0.0, 0.8]);
        assert_eq!(t.values.len(), 2);
    }
}
