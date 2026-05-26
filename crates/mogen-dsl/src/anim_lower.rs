//! Lowering for `joint`, `clip`, and procedural-template DSL nodes.
//!
//! These nodes are declarative metadata on the scene — they do not produce
//! geometry, they produce [`mogen_core::Joint`] entries and [`mogen_core::Clip`]
//! tracks that the exporter emits as glTF animation channels.

use anyhow::{anyhow, bail, Result};
use glam::{Quat, Vec3};

use mogen_core::{
    Clip, Easing, Interpolation, Joint, JointKind, NodeId, SceneGraph, Track, TrackProperty,
};
use mogen_anim as anim;

/// Procedural templates author keyframes as deltas from rest (e.g. `spin`
/// emits `q(0)=I`), but glTF rotation/translation/scale channels REPLACE the
/// node's rest TRS at runtime. If the target node already carries a non-rest
/// transform — typically because `attach` rotated it to align connectors —
/// playback would snap that rest pose away. Bake the rest pose into every
/// keyframe so the channel reproduces rest at t=0 and applies the procedural
/// motion on top.
fn compose_with_rest_pose(clip: &mut Clip, graph: &SceneGraph) {
    for track in &mut clip.tracks {
        let rest = graph.get(track.node).transform;
        match track.property {
            TrackProperty::Rotation => {
                for v in &mut track.values {
                    let q = Quat::from_xyzw(v[0], v[1], v[2], v[3]);
                    let composed = (rest.rotation * q).normalize();
                    *v = [composed.x, composed.y, composed.z, composed.w];
                }
            }
            TrackProperty::Translation => {
                for v in &mut track.values {
                    v[0] += rest.translation.x;
                    v[1] += rest.translation.y;
                    v[2] += rest.translation.z;
                }
            }
            TrackProperty::Scale => {
                for v in &mut track.values {
                    v[0] *= rest.scale.x;
                    v[1] *= rest.scale.y;
                    v[2] *= rest.scale.z;
                }
            }
        }
    }
}

use crate::ast::Node;

/// Lower a top-level or scene-level `joint` node.
pub fn lower_joint(node: &Node, graph: &mut SceneGraph) -> Result<()> {
    let name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("joint declaration requires a name"))?;
    let kind = joint_kind_attr(node)?;
    let axis = node.attr_vec3("axis").unwrap_or(Vec3::Y);
    let limits = node.attr_pair("limits");
    let pivot_ref = node
        .attr_string("pivot")
        .ok_or_else(|| anyhow!("joint \"{name}\" requires pivot=\"<node_name>\""))?;
    let pivot = find_node_scoped(graph, &pivot_ref, node.use_id)
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
        tracks.push(lower_track(c, graph, seconds, node.use_id)?);
    }

    graph.clips.push(Clip {
        name,
        duration: seconds,
        tracks,
        origin: node.origin.clone(),
    });
    Ok(())
}

fn lower_track(
    node: &Node,
    graph: &SceneGraph,
    duration: f32,
    use_id: Option<u32>,
) -> Result<Track> {
    let target_name = node
        .name
        .clone()
        .ok_or_else(|| anyhow!("track requires a target name (joint or node)"))?;
    let easing = parse_easing_attr(node)?;

    // Resolve target: prefer joint match, fall back to bare node match.
    if let Some(joint) = graph.find_joint(&target_name).cloned() {
        return lower_joint_track(node, &joint, duration, easing);
    }

    // Try the use-frame-scoped lookup first; if that misses, fall back to a
    // global by-name lookup so animation clips defined in their own module
    // (e.g. `humanoid_walk`) can drive bones declared in a sibling module
    // frame (`humanoid_full`'s skeleton). This matches the clip files'
    // documented contract: "drives bones by name".
    let node_id = find_node_scoped(graph, &target_name, use_id)
        .or_else(|| graph.find_node(&target_name));
    let Some(node_id) = node_id else {
        bail!(
            "track target \"{}\" is neither a joint nor a scene node",
            target_name
        );
    };

    // Direct node track: caller must supply an explicit property.
    let prop = node
        .attr_string("prop")
        .ok_or_else(|| anyhow!("direct-node track requires prop=\"translation|rotation|scale\""))?;
    let property = match prop {
        "translation" | "pos" => TrackProperty::Translation,
        "rotation" | "rot" => TrackProperty::Rotation,
        "scale" => TrackProperty::Scale,
        other => bail!("unknown track prop `{other}`"),
    };
    let axis = node.attr_vec3("axis").unwrap_or(Vec3::Y);
    let (times, values) = sample_track(node, duration, property, axis)?;
    let mut track = Track {
        node: node_id,
        property,
        interpolation: Interpolation::Linear,
        easing,
        times,
        values,
        source_span: Some(node.span),
    };
    track.bake_easing();
    Ok(track)
}

fn lower_joint_track(
    node: &Node,
    joint: &Joint,
    duration: f32,
    easing: Easing,
) -> Result<Track> {
    let (property, axis) = match joint.kind {
        JointKind::Hinge | JointKind::Rotor | JointKind::Ball => {
            (TrackProperty::Rotation, joint.axis)
        }
        JointKind::Slider => (TrackProperty::Translation, joint.axis),
    };
    let (times, values) = sample_track(node, duration, property, axis)?;
    let mut track = Track {
        node: joint.pivot,
        property,
        interpolation: Interpolation::Linear,
        easing,
        times,
        values,
        source_span: Some(node.span),
    };
    track.bake_easing();
    Ok(track)
}

/// Read `easing=<ident|string>` and resolve it to an [`Easing`]. Missing
/// attribute → [`Easing::Linear`]. Unknown spelling → error so typos surface
/// at lower time rather than silently degrading to linear.
fn parse_easing_attr(node: &Node) -> Result<Easing> {
    let Some(s) = node.attr_string("easing") else {
        return Ok(Easing::Linear);
    };
    Easing::from_str(s).ok_or_else(|| {
        anyhow!(
            "unknown easing `{s}` (expected linear|ease_in|ease_out|ease_in_out|\
             ease_in_cubic|ease_out_cubic|ease_in_out_cubic|ease_in_sine|ease_out_sine|\
             ease_in_out_sine|ease_in_back|ease_out_back|ease_in_out_back|\
             ease_in_bounce|ease_out_bounce|ease_in_out_bounce)"
        )
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
    let target_ref = node
        .attr_string("target")
        .ok_or_else(|| anyhow!("`{}` requires target=\"<name>\"", node.kind))?;
    let targets = resolve_anim_targets(target_ref, graph, node.use_id);
    if targets.is_empty() {
        bail!(
            "`{}` target \"{}\" is neither a joint nor a scene node",
            node.kind,
            target_ref
        );
    }
    // A joint target re-uses the joint's own axis when the caller didn't override.
    let axis_default = graph
        .find_joint(target_ref)
        .map(|j| j.axis)
        .unwrap_or(Vec3::Y);
    let axis = node.attr_vec3("axis").unwrap_or(axis_default);
    let easing = parse_easing_attr(node)?;

    let multi = targets.len() > 1;
    for (i, target) in targets.into_iter().enumerate() {
        let name = if multi {
            format!("{clip_name}_{i}")
        } else {
            clip_name.clone()
        };
        let mut clip = match node.kind.as_str() {
            "spin" => {
                let rpm = node.attr_number("rpm").unwrap_or(60.0);
                anim::spin(&name, target, axis, rpm, easing)
            }
            "open_close" => {
                let angle = node.attr_number("angle").unwrap_or(90.0);
                let seconds = node.attr_number("seconds").unwrap_or(1.0);
                anim::open_close(&name, target, axis, angle, seconds, easing)
            }
            "wave" => {
                let amp = node.attr_number("amplitude").unwrap_or(15.0);
                let hz = node.attr_number("hz").unwrap_or(1.0);
                anim::wave(&name, target, axis, amp, hz, easing)
            }
            "flap" => {
                let amp = node.attr_number("amplitude").unwrap_or(30.0);
                let hz = node.attr_number("hz").unwrap_or(2.0);
                anim::flap(&name, target, axis, amp, hz, easing)
            }
            "idle" => {
                let amp = node.attr_number("amplitude").unwrap_or(0.02);
                let hz = node.attr_number("hz").unwrap_or(0.5);
                anim::idle(&name, target, amp, hz, easing)
            }
            other => bail!("unknown animation template `{other}`"),
        };
        compose_with_rest_pose(&mut clip, graph);
        clip.origin = node.origin.clone();
        graph.clips.push(clip);
    }
    Ok(())
}

/// Resolve an anim `target=` reference to one or more scene nodes.
///
/// Precedence: joint name → every node sharing `name` → every node sharing
/// `role`. The multi-match name pass lets `array`/`mirror` replicants (which
/// keep their source names) all receive the same procedural clip.
///
/// `use_id` scopes the name/role passes to the template's frame plus any
/// descendant frame, mirroring `attach`'s scoped lookup. Two imported objects
/// sharing a node name (e.g. both ceiling_fan and office_chair declare `hub`)
/// stay isolated — each instantiation mints a different frame and the spin
/// authored inside one only sees its own subtree. A template authored in an
/// outer module can still reach nodes brought in by a nested `use`. Top-level
/// user-authored templates carry `use_id=None` and see the whole graph.
fn resolve_anim_targets(
    target_ref: &str,
    graph: &SceneGraph,
    use_id: Option<u32>,
) -> Vec<NodeId> {
    if let Some(j) = graph.find_joint(target_ref) {
        return vec![j.pivot];
    }
    let by_name = find_nodes_by_name_scoped(graph, target_ref, use_id);
    if !by_name.is_empty() {
        return by_name;
    }
    find_nodes_by_role_scoped(graph, target_ref, use_id)
}

fn find_node_scoped(graph: &SceneGraph, name: &str, use_id: Option<u32>) -> Option<NodeId> {
    graph
        .nodes
        .iter()
        .position(|n| n.name == name && graph.use_id_visible(use_id, n.use_id))
        .map(|i| NodeId(i as u32))
}

fn find_nodes_by_name_scoped(
    graph: &SceneGraph,
    name: &str,
    use_id: Option<u32>,
) -> Vec<NodeId> {
    graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            (n.name == name && graph.use_id_visible(use_id, n.use_id))
                .then_some(NodeId(i as u32))
        })
        .collect()
}

fn find_nodes_by_role_scoped(
    graph: &SceneGraph,
    role: &str,
    use_id: Option<u32>,
) -> Vec<NodeId> {
    graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            (n.role.as_deref() == Some(role) && graph.use_id_visible(use_id, n.use_id))
                .then_some(NodeId(i as u32))
        })
        .collect()
}

fn joint_kind_attr(node: &Node) -> Result<JointKind> {
    let s = node
        .attr_string("type")
        .ok_or_else(|| anyhow!("joint requires type=hinge|slider|ball|rotor"))?;
    match s {
        "hinge" => Ok(JointKind::Hinge),
        "slider" => Ok(JointKind::Slider),
        "ball" => Ok(JointKind::Ball),
        "rotor" => Ok(JointKind::Rotor),
        other => bail!("unknown joint type `{other}`"),
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

    #[test]
    fn template_spin_preserves_rest_rotation_at_t0() {
        // When `attach` rotates a node, the procedural `spin` template must
        // start from the rest pose (q(0) == rest.rotation), not snap back to
        // identity. Otherwise playback flips the visible axis.
        let g = lower(&parse(
            r#"
            scene {
              box "anchor" (size=[0.1, 0.1, 0.1]) {
                connector "side" (at=[0.05, 0, 0], dir=[1, 0, 0])
              }
              box "hub" (size=[0.4, 0.4, 0.1])
              attach (parent="anchor", child="hub", socket="side", plug="back")
            }
            spin "hub_spin" (target="hub", axis=[0, 0, 1], rpm=60)
            "#,
        ).expect("parse")).expect("lower");

        let hub_id = g.find_node("hub").expect("hub");
        let rest = g.get(hub_id).transform.rotation;
        // Attach should have produced a non-identity rest rotation.
        assert!(
            (rest.dot(glam::Quat::IDENTITY).abs() - 1.0).abs() > 1e-4,
            "expected attach to rotate hub away from identity (got {:?})",
            rest
        );

        let track = &g.clips[0].tracks[0];
        let q0 = glam::Quat::from_xyzw(
            track.values[0][0], track.values[0][1], track.values[0][2], track.values[0][3],
        );
        // First keyframe should match rest pose (within shortest-arc sign).
        assert!(
            q0.dot(rest).abs() > 1.0 - 1e-4,
            "expected baked t=0 to equal rest rotation: q0={:?} rest={:?}",
            q0,
            rest,
        );
    }

    #[test]
    fn template_target_is_scoped_to_its_use_frame() {
        // Two imported objects share a node name (`hub`). One ships a
        // `spin (target="hub")`. Without use_id-scoped lookup, the spin would
        // also produce a track for the other import's hub. With scoping it
        // must hit only its own.
        let tmp = std::env::temp_dir().join(format!(
            "mogen-anim-scope-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("fan.mog"),
            r#"
            scene { cylinder "hub" (radius=0.1, height=0.05) }
            spin "fan_spin" (target="hub", axis=[0, 1, 0], rpm=30)
            "#,
        )
        .unwrap();
        std::fs::write(
            tmp.join("chair.mog"),
            r#"scene { cylinder "hub" (radius=0.04, height=0.08) }"#,
        )
        .unwrap();
        let main_src = r#"
            import "fan.mog"
            import "chair.mog"
            scene {
              group "f" () { use "fan" () }
              group "c" () { use "chair" () }
            }
        "#;
        let ast = crate::parser::parse(main_src).unwrap();
        let scene = crate::lower::lower_with_source(&ast, Some(tmp.as_path())).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);

        // Exactly one clip from the fan, with exactly one track on the fan's hub.
        assert_eq!(scene.clips.len(), 1, "expected only fan_spin to fire");
        let clip = &scene.clips[0];
        assert_eq!(clip.name, "fan_spin");
        assert_eq!(clip.tracks.len(), 1, "spin should target only fan's hub");

        // The track's node must sit under the `f` group, not the `c` group.
        let target = clip.tracks[0].node;
        let f_id = scene.find_node("f").expect("f group");
        assert_eq!(
            scene.find_node_in_subtree(f_id, "hub"),
            Some(target),
            "spin track must target the fan's hub, not the chair's"
        );
    }

    #[test]
    fn track_easing_densifies_keyframes() {
        // ease_in_out on a 2-keyframe track should produce 17 dense keys
        // (1 + 16 per segment) and lag below the linear midpoint at t=0.25.
        let g = lower(&parse(
            r#"
            scene { box "h" (size=[0.1, 0.1, 0.1]) }
            clip "c" (seconds=1.0) {
              track "h" (prop=rotation, axis=[1, 0, 0], easing=ease_in_out, from=0, to=90)
            }
            "#,
        ).expect("parse")).expect("lower");
        let t = &g.clips[0].tracks[0];
        assert_eq!(t.times.len(), 17, "ease_in_out should densify to 17 keys");
        assert_eq!(t.easing, mogen_core::Easing::Linear, "easing must reset to Linear after bake");
        // Quarter-time keyframe sits at t=0.25 with eased fraction 0.125.
        assert!((t.times[4] - 0.25).abs() < 1e-3);
    }

    #[test]
    fn track_easing_back_overshoots_endpoints() {
        // ease_out_back overshoots above 1.0 mid-segment for a translation track.
        let g = lower(&parse(
            r#"
            scene { box "b" (size=[0.1, 0.1, 0.1]) }
            clip "c" (seconds=1.0) {
              track "b" (prop=translation, axis=[1, 0, 0], easing=ease_out_back, from=0, to=1)
            }
            "#,
        ).expect("parse")).expect("lower");
        let t = &g.clips[0].tracks[0];
        let max_x = t.values.iter().map(|v| v[0]).fold(f32::MIN, f32::max);
        assert!(
            max_x > 1.001,
            "ease_out_back should overshoot 1.0, got max x = {}",
            max_x,
        );
    }

    #[test]
    fn track_easing_unknown_value_errors() {
        let err = lower(&parse(
            r#"
            scene { box "h" (size=[0.1, 0.1, 0.1]) }
            clip "c" (seconds=1.0) {
              track "h" (prop=rotation, easing=bouncy_thing, from=0, to=90)
            }
            "#,
        ).expect("parse")).unwrap_err();
        assert!(format!("{err}").contains("unknown easing"));
    }

    #[test]
    fn template_open_close_easing_warps_phase() {
        // open_close with linear easing has 3 keyframes; with ease_in_out it
        // densifies to 33 to capture the warp.
        let linear = lower(&parse(
            r#"
            scene { box "lid" (size=[0.5, 0.05, 0.5]) }
            open_close "swing" (target="lid", axis=[1, 0, 0], angle=90, seconds=1.0)
            "#,
        ).expect("parse")).expect("lower");
        assert_eq!(linear.clips[0].tracks[0].times.len(), 3);

        let eased = lower(&parse(
            r#"
            scene { box "lid" (size=[0.5, 0.05, 0.5]) }
            open_close "swing" (target="lid", axis=[1, 0, 0], angle=90, seconds=1.0, easing=ease_in_out)
            "#,
        ).expect("parse")).expect("lower");
        assert_eq!(eased.clips[0].tracks[0].times.len(), 33);
    }

    #[test]
    fn template_spin_on_identity_node_is_unchanged() {
        // Common case: target has identity rest. Bake must be a no-op so we
        // don't perturb existing examples (drone.mog, windmill examples).
        let g = lower(&parse(
            r#"
            scene { box "rotor" (size=[1, 0.1, 0.1]) }
            spin "r" (target="rotor", axis=[0, 1, 0], rpm=60)
            "#,
        ).expect("parse")).expect("lower");
        let track = &g.clips[0].tracks[0];
        let q0 = glam::Quat::from_xyzw(
            track.values[0][0], track.values[0][1], track.values[0][2], track.values[0][3],
        );
        assert!(q0.dot(glam::Quat::IDENTITY).abs() > 1.0 - 1e-5);
    }
}
