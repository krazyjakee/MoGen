//! Surface-derived guides with stable authored references, not triangle IDs.
use crate::ast::Node;
use anyhow::{anyhow, Result};
use glam::Vec3;
use mogen_core::{GuideCurve, NodeId, SceneGraph, Transform};

fn error(node: &Node, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("E0160 at {}..{}: {message}", node.span.start, node.span.end)
}
fn unique(graph: &SceneGraph, node: &Node, name: &str) -> Result<NodeId> {
    let ids: Vec<_> = graph
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.name == name && graph.use_id_visible(node.use_id, n.use_id))
        .map(|(i, _)| NodeId(i as u32))
        .collect();
    if ids.len() != 1 {
        return Err(error(
            node,
            format!(
                "target {name:?} resolves to {} nodes; use a unique name",
                ids.len()
            ),
        ));
    }
    Ok(ids[0])
}
fn authored<'a>(all: &[&'a Node], graph: &SceneGraph, id: NodeId) -> Result<&'a Node> {
    let n = graph.get(id);
    all.iter()
        .find(|a| {
            a.kind == n.kind
                && a.use_id == n.use_id
                && a.origin == n.origin
                && Some(a.span) == n.source_span
        })
        .copied()
        .ok_or_else(|| anyhow!("E0160: cannot identify authored surface {:?}", n.name))
}

pub(super) fn resolve(ast: &[Node], graph: &mut SceneGraph) -> Result<()> {
    fn walk<'a>(nodes: &'a [Node], replicated: bool, out: &mut Vec<&'a Node>) -> Result<()> {
        for n in nodes {
            if replicated && (n.kind == "guide" || n.attr("guide").is_some()) {
                return Err(error(n,"guide details inside replicators are unsupported; instantiate explicit modules"));
            }
            out.push(n);
            walk(
                &n.children,
                replicated || matches!(n.kind.as_str(), "array" | "mirror" | "grid" | "stack"),
                out,
            )?;
        }
        Ok(())
    }
    let mut all = vec![];
    walk(ast, false, &mut all)?;
    if !all
        .iter()
        .any(|n| n.kind == "guide" || n.attr("guide").is_some())
    {
        return Ok(());
    }
    let mut next = graph.clone();
    for spec in all.iter().copied().filter(|n| n.kind == "guide") {
        let name = spec
            .name
            .clone()
            .ok_or_else(|| error(spec, "guide requires a reusable name"))?;
        if next
            .guides
            .iter()
            .any(|g| g.name == name && g.use_id == spec.use_id)
        {
            return Err(error(spec, "duplicate guide name in this module"));
        }
        let target = unique(
            &next,
            spec,
            spec.attr_string("target")
                .ok_or_else(|| error(spec, "guide requires target"))?,
        )?;
        let source = authored(&all, &next, target)?;
        if next.get(target).conform_binding.is_some()
            || source.attrs.iter().any(|(k, _)| {
                k == "anchor"
                    || k == "subdivide"
                    || k.starts_with("bend_")
                    || k.starts_with("twist_")
                    || matches!(k.as_str(), "taper" | "droop" | "noise" | "jitter" | "wave")
            })
        {
            return Err(error(spec,"guide target must retain its authored analytic surface; deformation/anchor/subdivision/conform is unsupported"));
        }
        let tolerance = spec.attr_number("tolerance").unwrap_or(0.001);
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(error(spec, "tolerance must be finite and positive"));
        }
        let section = spec.attr_string("section").unwrap_or("latitude");
        let (points,normals,closed)=match (source.kind.as_str(),section) {
            ("superellipsoid","latitude")=>latitude(source,spec,tolerance)?,
            ("sweep","profile_edge")=>profile_edge(source,spec,target,&next,tolerance)?,
            _=>return Err(error(spec,"supported guides are superellipsoid latitude and explicit-frame sweep profile_edge")),
        };
        validate_path(&points, closed, spec, tolerance)?;
        next.guides.push(GuideCurve {
            name,
            target,
            section: section.into(),
            use_id: spec.use_id,
            closed,
            tolerance,
            points: points.iter().map(|p| p.to_array()).collect(),
            normals: normals.iter().map(|n| n.to_array()).collect(),
            source_span: spec.span,
        });
    }
    for node in all
        .iter()
        .copied()
        .filter(|n| n.kind == "sweep" && n.attr("guide").is_some())
    {
        let child = unique(
            &next,
            node,
            node.name
                .as_deref()
                .ok_or_else(|| error(node, "guided sweep requires an editable name"))?,
        )?;
        let name = node
            .attr_string("guide")
            .ok_or_else(|| error(node, "guide must name a declared curve"))?;
        let guides: Vec<_> = next
            .guides
            .iter()
            .filter(|g| g.name == name && next.use_id_visible(node.use_id, g.use_id))
            .collect();
        if guides.len() != 1 {
            return Err(error(
                node,
                format!("guide {name:?} is unresolved or ambiguous"),
            ));
        }
        let guide = guides[0].clone();
        if next.is_ancestor(child, guide.target) || !next.get(child).children.is_empty() {
            return Err(error(node, "guide dependency cycle or non-leaf detail"));
        }
        for key in [
            "path",
            "roll",
            "scale_along",
            "twist",
            "frame_up",
            "closed",
            "anchor",
            "subdivide",
        ] {
            if node.attr(key).is_some() {
                return Err(error(
                    node,
                    format!("guided sweep owns its path/frame; remove {key}"),
                ));
            }
        }
        if node.attrs.iter().any(|(k, _)| {
            k.starts_with("bend_")
                || k.starts_with("twist_")
                || matches!(k.as_str(), "taper" | "droop" | "noise" | "jitter" | "wave")
        }) {
            return Err(error(node, "deformation would detach a guided detail"));
        }
        let profile = node
            .attr_list_pair("profile")
            .ok_or_else(|| error(node, "guided sweep requires a 2D profile"))?;
        let lift = node.attr_number("lift").unwrap_or(0.0);
        let mut frames = surface_frames(&guide, node)?;
        for (frame, normal) in frames.iter_mut().zip(&guide.normals) {
            frame.center += Vec3::from_array(*normal).normalize_or_zero() * lift;
        }
        let uv = next
            .get(child)
            .material
            .and_then(|m| next.materials.get(m.0 as usize))
            .map(|m| m.uv_mode)
            .unwrap_or_default();
        validate_path(
            &frames.iter().map(|f| f.center).collect::<Vec<_>>(),
            guide.closed,
            node,
            guide.tolerance,
        )?;
        let mut profile_path: Vec<_> = profile.iter().map(|p| Vec3::new(p[0], p[1], 0.0)).collect();
        if let Some(first) = profile_path.first().copied() {
            profile_path.push(first);
        }
        validate_path(&profile_path, true, node, guide.tolerance.min(0.0001))?;
        let mesh = mogen_geom::sweep_surface_curve(&profile, &frames, 0.0, guide.closed, uv)?;
        // Same placement/offset model as conform, default reparent=1.
        if node.attr_number("reparent").unwrap_or(1.0) != 0.0 {
            crate::attach::reparent_pub(&mut next, child, guide.target);
            next.nodes[child.0 as usize].transform = Transform::IDENTITY;
            next.set_mesh(child, mesh);
        } else {
            let worlds = next.world_transforms();
            let to_child = worlds[child.0 as usize].inverse() * worlds[guide.target.0 as usize];
            if !to_child.is_finite() {
                return Err(error(node, "detail parent transform is singular"));
            }
            next.set_mesh(child, mogen_geom::transform_mesh(&mesh, to_child));
        }
        next.nodes[child.0 as usize].geometry_identity = None;
        next.nodes[child.0 as usize].path_frame = Some(
            serde_json::json!({"version":1,"guide":name,"target":next.get(guide.target).name,
            "space":"target local","lift":lift,"closed":guide.closed,"tolerance":guide.tolerance}),
        );
        next.nodes[child.0 as usize]
            .connectors
            .retain(|c| c.source_span.is_some());
        super::connector::add_aabb_connectors_if_missing(child, &mut next);
    }
    *graph = next;
    Ok(())
}

fn latitude(source: &Node, spec: &Node, tolerance: f32) -> Result<(Vec<Vec3>, Vec<Vec3>, bool)> {
    let size = super::helpers::resolve_size3(source, Vec3::ONE);
    let ew = source.attr_number("ew").unwrap_or(1.0);
    let ns = source.attr_number("ns").unwrap_or(1.0);
    let level = spec.attr_number("level").unwrap_or(0.0);
    if !size.is_finite()
        || size.min_element() <= 0.0
        || !ew.is_finite()
        || !ns.is_finite()
        || ew < 0.5
        || ns < 0.5
        || !level.is_finite()
        || level.abs() >= 1.0
    {
        return Err(error(
            spec,
            "latitude needs positive size, ew/ns >= 0.5 and -1 < level < 1",
        ));
    }
    let eps_ns = 1.0 / ns;
    let eps_ew = 1.0 / ew;
    let spow = |x: f32, p: f32| {
        if x.abs() < 1e-7 {
            0.0
        } else {
            x.signum() * x.abs().powf(p)
        }
    };
    // level is normalized Y, so invert the source's latitude parameter.
    let eta = (level.signum() * level.abs().powf(ns)).asin();
    let evaluate = |u: f32| {
        // Evaluate cardinal angles accurately: tiny trig residuals become
        // visible offsets when raised to fractional superellipse powers.
        let angle = f64::from(u) * std::f64::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let (sin, cos) = (sin as f32, cos as f32);
        let p = Vec3::new(
            size.x * 0.5 * spow(eta.cos(), eps_ns) * spow(cos, eps_ew),
            size.y * 0.5 * level,
            size.z * 0.5 * spow(eta.cos(), eps_ns) * spow(sin, eps_ew),
        );
        let normal = Vec3::new(
            spow(eta.cos(), 2.0 - eps_ns) * spow(cos, 2.0 - eps_ew) / size.x,
            spow(eta.sin(), 2.0 - eps_ns) / size.y,
            spow(eta.cos(), 2.0 - eps_ns) * spow(sin, 2.0 - eps_ew) / size.z,
        )
        .normalize_or_zero();
        (p, normal)
    };
    let mut count = 32;
    loop {
        let mut points = vec![];
        let mut normals = vec![];
        let mut error_bound = 0.0_f32;
        for i in 0..count {
            let a = evaluate(i as f32 / count as f32);
            let b = evaluate((i + 1) as f32 / count as f32);
            for fraction in [0.25, 0.5, 0.75] {
                let actual = evaluate((i as f32 + fraction) / count as f32).0;
                // Bound geometric chord error. Fractional-power curves have
                // highly nonuniform speed near their axes; comparing equal
                // parameter fractions falsely treats tangential travel as error.
                let nearest = mogen_geom::measure::closest_segment_points(
                    actual, actual, a.0, b.0,
                ).1;
                error_bound = error_bound.max(actual.distance(nearest));
            }
            points.push(a.0);
            normals.push(a.1);
        }
        if error_bound <= tolerance {
            points.push(points[0]);
            normals.push(normals[0]);
            return Ok((points, normals, true));
        }
        count *= 2;
        if count > 4096 {
            return Err(error(
                spec,
                "guide needs more than 4096 samples; relax tolerance or soften the profile",
            ));
        }
    }
}

fn profile_edge(
    source: &Node,
    spec: &Node,
    target: NodeId,
    graph: &SceneGraph,
    tolerance: f32,
) -> Result<(Vec<Vec3>, Vec<Vec3>, bool)> {
    let up = source.attr_vec3("frame_up").ok_or_else(|| {
        error(
            spec,
            "profile-edge guides require an explicit frame_up on the target",
        )
    })?;
    let profile = source
        .attr_list_pair("profile")
        .ok_or_else(|| error(spec, "target requires an authored profile"))?;
    let edge = spec.attr_number("edge").unwrap_or(0.0);
    if !edge.is_finite() || edge < 0.0 || edge.fract() != 0.0 || edge as usize >= profile.len() {
        return Err(error(spec, "edge must index an authored profile edge"));
    }
    let edge = edge as usize;
    let a = glam::Vec2::from_array(profile[edge]);
    let b = glam::Vec2::from_array(profile[(edge + 1) % profile.len()]);
    if a == b {
        return Err(error(spec, "profile edge is degenerate"));
    }
    let midpoint = (a + b) * 0.5;
    let mut path = source
        .attr_list_vec3("path")
        .ok_or_else(|| error(spec, "target requires an authored path"))?;
    for r in graph
        .relationships
        .iter()
        .filter(|r| r.child == target && r.mode == "endpoint")
    {
        let i = if r.endpoint.as_deref() == Some("start") {
            0
        } else {
            path.len() - 1
        };
        path[i] = r.child_anchor;
    }
    if source.attr("roll").is_some()
        || source.attr("scale_along").is_some()
        || source.attr_number("twist").unwrap_or(0.0) != 0.0
    {
        return Err(error(spec,"profile-edge v1 requires constant, unrolled profile; remove modulation or derive a separate guide"));
    }
    let closed = source.attr_number("closed").unwrap_or(0.0) != 0.0;
    let mut samples = 8;
    loop {
        if samples as usize * 2 * path.len() > 4096 {
            return Err(error(spec, "guide exceeds 4096 samples; relax tolerance"));
        }
        let (_, frames) = mogen_geom::sweep_path_frames(&path, samples, up.to_array(), closed)?;
        let (_, fine) = mogen_geom::sweep_path_frames(&path, samples * 2, up.to_array(), closed)?;
        let point =
            |f: &mogen_geom::PathFrame| f.center - f.binormal * midpoint.x + f.normal * midpoint.y;
        let error_bound = frames
            .windows(2)
            .enumerate()
            .map(|(i, w)| point(&fine[i * 2 + 1]).distance(point(&w[0]).lerp(point(&w[1]), 0.5)))
            .fold(0.0_f32, f32::max);
        if error_bound <= tolerance {
            let points: Vec<_> = frames.iter().map(point).collect();
            let last = points.len() - 1;
            let normals = frames
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let tangent = if closed && (i == 0 || i == last) {
                        points[1] - points[last - 1]
                    } else {
                        points[(i + 1).min(last)] - points[i.saturating_sub(1)]
                    };
                    let edge_vector = -f.binormal * (b.x - a.x) + f.normal * (b.y - a.y);
                    edge_vector.cross(tangent).normalize_or_zero()
                })
                .collect();
            return Ok((points, normals, closed));
        }
        samples *= 2;
        if samples as usize * path.len() > 4096 {
            return Err(error(spec, "guide exceeds 4096 samples; relax tolerance"));
        }
    }
}

fn surface_frames(guide: &GuideCurve, node: &Node) -> Result<Vec<mogen_geom::PathFrame>> {
    let points: Vec<_> = guide.points.iter().map(|p| Vec3::from_array(*p)).collect();
    let last = points.len() - 1;
    let mut frames = vec![];
    for i in 0..points.len() {
        let t = if guide.closed && (i == 0 || i == last) {
            points[1] - points[last - 1]
        } else {
            points[(i + 1).min(last)] - points[i.saturating_sub(1)]
        };
        let frame = mogen_geom::frame_from_up(points[i], t, Vec3::from_array(guide.normals[i]))
            .map_err(|e| error(node, e))?;
        frames.push(frame);
    }
    if guide.closed {
        frames[last] = frames[0];
    }
    Ok(frames)
}

fn validate_path(points: &[Vec3], closed: bool, spec: &Node, tolerance: f32) -> Result<()> {
    if points.len() < if closed { 4 } else { 2 }
        || points.len() > 4097
        || points.iter().any(|p| !p.is_finite())
        || points.windows(2).any(|p| p[0] == p[1])
    {
        return Err(error(
            spec,
            "guide/profile is empty, degenerate, non-finite or exceeds 4096 segments",
        ));
    }
    let epsilon = tolerance * 0.01;
    for i in 0..points.len() - 1 {
        let lo = points[i].min(points[i + 1]) - Vec3::splat(epsilon);
        let hi = points[i].max(points[i + 1]) + Vec3::splat(epsilon);
        for j in i + 2..points.len() - 1 {
            if closed && i == 0 && j == points.len() - 2 {
                continue;
            }
            if points[j].min(points[j + 1]).cmpgt(hi).any()
                || points[j].max(points[j + 1]).cmplt(lo).any()
            {
                continue;
            }
            let (a, b) = mogen_geom::measure::closest_segment_points(
                points[i],
                points[i + 1],
                points[j],
                points[j + 1],
            );
            if a.distance(b) <= epsilon {
                return Err(error(spec,"guide/profile self-intersects or self-contacts within tolerance/100; reduce lift or correct the authored path"));
            }
        }
    }
    Ok(())
}
