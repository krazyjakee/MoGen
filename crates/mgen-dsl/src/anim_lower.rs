//! Lowering for `joint`, `clip`, and procedural-template DSL nodes.
//!
//! These nodes are declarative metadata on the scene — they do not produce
//! geometry, they produce [`mgen_core::Joint`] entries and [`mgen_core::Clip`]
//! tracks that the exporter emits as glTF animation channels.

use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mgen_core::{
    Clip, Interpolation, Joint, JointKind, NodeId, SceneGraph, Track, TrackProperty,
};
use mgen_anim as anim;

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
    let (times, values) = sample_2kf(node, duration, property, Vec3::Y)?;
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
    let (times, values) = sample_2kf(node, duration, property, axis)?;
    Ok(Track {
        node: joint.pivot,
        property,
        interpolation: Interpolation::Linear,
        times,
        values,
    })
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
        .ok_or_else(|| anyhow!("track requires `to=` scalar"))?;
    let times = vec![0.0, duration];
    let values = match property {
        TrackProperty::Rotation => {
            let a = axis.normalize_or(Vec3::Y);
            let q0 = Quat::from_axis_angle(a, from.to_radians());
            let q1 = Quat::from_axis_angle(a, to.to_radians());
            vec![[q0.x, q0.y, q0.z, q0.w], [q1.x, q1.y, q1.z, q1.w]]
        }
        TrackProperty::Translation => {
            let a = axis.normalize_or(Vec3::Y);
            let v0 = a * from;
            let v1 = a * to;
            vec![[v0.x, v0.y, v0.z, 0.0], [v1.x, v1.y, v1.z, 0.0]]
        }
        TrackProperty::Scale => {
            vec![[from, from, from, 0.0], [to, to, to, 0.0]]
        }
    };
    Ok((times, values))
}

/// Lower a procedural-template node (spin / open_close / wave / flap / idle).
pub fn lower_template(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let clip_name = node
        .name
        .clone()
        .unwrap_or_else(|| format!("{}_clip", node.kind));
    let target_ref = string_or_ident(node.attr("target"))
        .ok_or_else(|| anyhow!("`{}` requires target=\"<name>\"", node.kind))?;
    let target = resolve_anim_target(&target_ref, graph).ok_or_else(|| {
        anyhow!(
            "`{}` target \"{}\" is neither a joint nor a scene node",
            node.kind,
            target_ref
        )
    })?;
    // A joint target re-uses the joint's own axis when the caller didn't override.
    let axis_default = graph
        .find_joint(&target_ref)
        .map(|j| j.axis)
        .unwrap_or(Vec3::Y);
    let axis = node.attr_vec3("axis").unwrap_or(axis_default);

    let clip = match node.kind.as_str() {
        "spin" => {
            let rpm = node.attr_number("rpm").unwrap_or(60.0);
            anim::spin(&clip_name, target, axis, rpm)
        }
        "open_close" => {
            let angle = node.attr_number("angle").unwrap_or(90.0);
            let seconds = node.attr_number("seconds").unwrap_or(1.0);
            anim::open_close(&clip_name, target, axis, angle, seconds)
        }
        "wave" => {
            let amp = node.attr_number("amplitude").unwrap_or(15.0);
            let hz = node.attr_number("hz").unwrap_or(1.0);
            anim::wave(&clip_name, target, axis, amp, hz)
        }
        "flap" => {
            let amp = node.attr_number("amplitude").unwrap_or(30.0);
            let hz = node.attr_number("hz").unwrap_or(2.0);
            anim::flap(&clip_name, target, axis, amp, hz)
        }
        "idle" => {
            let amp = node.attr_number("amplitude").unwrap_or(0.02);
            let hz = node.attr_number("hz").unwrap_or(0.5);
            anim::idle(&clip_name, target, amp, hz)
        }
        other => bail!("unknown animation template `{other}`"),
    };
    graph.clips.push(clip);
    Ok(())
}

fn resolve_anim_target(target_ref: &str, graph: &SceneGraph) -> Option<NodeId> {
    if let Some(j) = graph.find_joint(target_ref) {
        return Some(j.pivot);
    }
    graph.find_node(target_ref)
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
