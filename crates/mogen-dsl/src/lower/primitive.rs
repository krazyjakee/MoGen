use anyhow::{anyhow, Context, Result};
use glam::{Mat4, Vec3};

use mogen_core::{Mesh, UvMode};
use mogen_geom::{
    bezier_patch_mesh, box_mesh, capsule_mesh, chamfered_box_mesh, clean_csg_output, coil_mesh,
    cone_mesh, curved_plane_mesh, cylinder_mesh, difference_many, disc_mesh, ellipsoid_mesh,
    extrude_mesh, frustum_mesh, half_cylinder_mesh, heightfield_mesh, hemisphere_mesh, hull_mesh,
    icosphere_mesh, inset_box_mesh, is_degenerate_solid, lathe_mesh, leaf_card_mesh, loft_mesh,
    mesh_from_glb_bytes,
    metaball_mesh, plane_mesh, poly_mesh, prism_mesh, pyramid_mesh, quad_mesh, read_glb_bytes,
    rounded_box_mesh, sphere_mesh, spline_ribbon_mesh, spline_tube_mesh, superellipsoid_mesh,
    sweep_mesh, torus_arc_mesh, torus_mesh, transform_mesh, tube_mesh, wedge_mesh,
    CoilHandedness, InsetFace, SweepModulation,
};

use crate::ast::Node;
use crate::lower::{mesh_bytes, source_dir};

use super::helpers::{resolve_size3, resolve_size_xy, resolve_size_xz};
use super::lod::{scaled_count, scaled_subdivisions};

/// Density multiplier for primitive default tessellation when the node
/// carries a smooth-deformation modifier — `bend_*`, `twist_y`, `noise`,
/// `droop`. Twice the segments avoids the obvious faceted look on a bent
/// cylinder, at ~4× triangles. An explicit `segments=` from the author
/// always wins.
fn deform_density(node: &Node) -> u32 {
    let needs_dense = node.attr("bend_x").is_some()
        || node.attr("bend_y").is_some()
        || node.attr("bend_z").is_some()
        || node.attr("twist_y").is_some()
        || node.attr("noise").is_some()
        || node.attr("droop").is_some()
        || node.attr("wave").is_some();
    if needs_dense {
        2
    } else {
        1
    }
}

fn authored_size_density(size: f32, reference: f32) -> f32 {
    if size.is_finite() && size > 0.0 && reference.is_finite() && reference > 0.0 {
        (size / reference).cbrt()
    } else {
        1.0
    }
}

/// Dispatch a primitive `Node` to its mesh builder. Returns `None` for non-
/// primitive kinds (group, scene, material, CSG ops, animation decls, …) so
/// callers can handle those separately. The inner `Result` carries failures
/// from primitives whose construction can fail at lowering time (e.g. `mesh`
/// loading a `.glb` from disk).
pub(super) fn primitive_mesh(node: &Node, uv_mode: UvMode) -> Option<Result<Mesh>> {
    // Density multiplier for the *default* tessellation count. 1× when the
    // node has no smooth-deform modifier; 2× when it does, so a bent or
    // melted shape doesn't read as low-poly. Only the default branch
    // multiplies by `dd` — an author-supplied `segments=` is taken at face
    // value (they already chose the density they want).
    let dd = deform_density(node);
    // Resolve a tessellation count: explicit author value if present,
    // otherwise the default × `dd`. Either way, the active LOD scale
    // (`lod_scale (value=N)` and any compounded `lod=N` overrides) is
    // applied so authors don't have to drop their `segments_u=64` on dense
    // heightfields/curved planes just to opt in to LOD scaling.
    let seg = |attr: &str, base: u32, min: u32| -> u32 {
        let raw = node
            .attr_number(attr)
            .map(|n| n as u32)
            .unwrap_or(base * dd);
        scaled_count(raw, min)
    };
    // Size-aware counterpart for curved primitives. An authored count remains
    // exact (apart from the existing LOD multiplier); only the implicit
    // primitive default changes with authored local size. Cube-root scaling is
    // deliberately gentler than a fixed world-space edge target: a tiny rivet
    // no longer pays for a dome, while a large set-piece cannot explode the
    // triangle budget linearly with its radius. `reference` is the primitive's
    // own default size, so the default spelling remains byte-identical.
    let seg_for_size = |attr: &str, base: u32, min: u32, size: f32, reference: f32| -> u32 {
        let raw = node.attr_number(attr).map(|n| n as u32).unwrap_or_else(|| {
            let ratio = authored_size_density(size, reference);
            ((base * dd) as f32 * ratio).round().max(min as f32) as u32
        });
        scaled_count(raw, min)
    };
    let m: Mesh = match node.kind.as_str() {
        "box" | "slab" | "post" | "panel" => {
            let s = resolve_size3(node, Vec3::ONE);
            box_mesh([s.x, s.y, s.z], uv_mode)
        }
        "plane" => {
            let s = resolve_size_xz(node, [1.0, 1.0]);
            plane_mesh(s, uv_mode)
        }
        "heightfield" => {
            let s = resolve_size_xz(node, [1.0, 1.0]);
            let segments_u = seg("segments_u", 32, 1);
            let segments_v = seg("segments_v", 32, 1);
            let amplitude = node.attr_number("amplitude").unwrap_or(0.5);
            let octaves = node
                .attr_number("octaves")
                .map(|n| (n as u32).clamp(1, 8))
                .unwrap_or(3);
            let frequency = node.attr_number("frequency").unwrap_or(1.0);
            let persistence = node.attr_number("persistence").unwrap_or(0.5);
            let seed = node.attr_number("seed").map(|n| n as u32).unwrap_or(1);
            heightfield_mesh(
                s, segments_u, segments_v, amplitude,
                octaves, frequency, persistence, seed, uv_mode,
            )
        }
        "quad" => {
            let s = resolve_size_xy(node, [1.0, 1.0]);
            quad_mesh(s, uv_mode)
        }
        "decal" => {
            // Decals are always image-as-texture, never tile — overriding the
            // inherited `uv_mode` so a wrapping `Tile` material on the parent
            // can't squash the decal artwork into a repeated micro-pattern.
            let s = resolve_size_xy(node, [0.5, 0.5]);
            let mut m = quad_mesh(s, UvMode::Fit);
            // Lift the quad slightly along its local +Z so it doesn't z-fight
            // against the surface it's sitting on. Default is small enough to
            // read flush at typical scales; the author can override via
            // `offset=`.
            let offset = node.attr_number("offset").unwrap_or(0.001);
            if offset != 0.0 {
                for p in &mut m.positions {
                    p[2] += offset;
                }
            }
            m
        }
        "cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            cylinder_mesh(radius, height, segments, uv_mode)
        }
        "cone" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            cone_mesh(radius, height, segments, uv_mode)
        }
        "sphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let rings = seg_for_size("rings", 16, 2, radius, 0.5);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            sphere_mesh(radius, rings, segments, uv_mode)
        }
        "capsule" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let rings = seg_for_size("rings", 8, 2, radius, 0.5);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            capsule_mesh(radius, height, rings, segments, uv_mode)
        }
        "torus" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let major_segments = seg_for_size("major_segments", 24, 3, major, 0.5);
            let minor_segments = seg_for_size("minor_segments", 12, 3, minor, 0.15);
            torus_mesh(major, minor, major_segments, minor_segments, uv_mode)
        }
        "prism" => {
            let s = resolve_size3(node, Vec3::ONE);
            prism_mesh([s.x, s.y, s.z], uv_mode)
        }
        "pyramid" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let sides = node.attr_number("sides").map(|n| n as u32).unwrap_or(4);
            pyramid_mesh(radius, height, sides, uv_mode)
        }
        "disc" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            disc_mesh(radius, segments, uv_mode)
        }
        "icosphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            // Default base subdivisions follow `dd` (2 with a deform modifier,
            // 1 otherwise); both default and explicit values then run through
            // `scaled_subdivisions` so LOD steps the count.
            let raw = node.attr_number("subdivisions").map(|n| n as u32).unwrap_or_else(|| {
                let size_steps = authored_size_density(radius, 0.5).log2().round() as i32;
                (1 + dd as i32 + size_steps).max(0) as u32
            });
            let subdivisions = scaled_subdivisions(raw);
            icosphere_mesh(radius, subdivisions, uv_mode)
        }
        "rounded_box" => {
            let s = resolve_size3(node, Vec3::ONE);
            let radius = node.attr_number("radius").unwrap_or(0.1);
            let segments = seg_for_size("segments", 4, 1, radius, 0.1);
            rounded_box_mesh([s.x, s.y, s.z], radius, segments, uv_mode)
        }
        "chamfered_box" => {
            let s = resolve_size3(node, Vec3::ONE);
            let radius = node.attr_number("radius").unwrap_or(0.1);
            chamfered_box_mesh([s.x, s.y, s.z], radius, uv_mode)
        }
        "inset_box" => {
            let s = resolve_size3(node, Vec3::ONE);
            let face = match node.attr_string("face") {
                Some(name) => match parse_inset_face(name) {
                    Ok(f) => f,
                    Err(e) => return Some(Err(e)),
                },
                None => InsetFace::PosY,
            };
            let amount = node.attr_number("amount").unwrap_or(0.1);
            let depth = node.attr_number("depth").unwrap_or(0.05);
            inset_box_mesh([s.x, s.y, s.z], face, amount, depth, uv_mode)
        }
        "wedge" => {
            let s = resolve_size3(node, Vec3::ONE);
            wedge_mesh([s.x, s.y, s.z], uv_mode)
        }
        "frustum" => {
            let bottom = node.attr_pair("bottom").unwrap_or([1.0, 1.0]);
            let top = node.attr_pair("top").unwrap_or([0.5, 0.5]);
            let height = node.attr_number("height").unwrap_or(1.0);
            frustum_mesh(bottom, top, height, uv_mode)
        }
        "tube" => {
            let outer = node.attr_number("outer").unwrap_or(0.5);
            let inner = node.attr_number("inner").unwrap_or(0.3);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = seg_for_size("segments", 24, 3, outer, 0.5);
            tube_mesh(outer, inner, height, segments, uv_mode)
        }
        "hemisphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let rings = seg_for_size("rings", 8, 2, radius, 0.5);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            hemisphere_mesh(radius, rings, segments, uv_mode)
        }
        "half_cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = seg_for_size("segments", 24, 3, radius, 0.5);
            half_cylinder_mesh(radius, height, segments, uv_mode)
        }
        "torus_arc" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let arc_deg = node.attr_number("arc").unwrap_or(90.0);
            let major_segments = seg_for_size("major_segments", 24, 3, major, 0.5);
            let minor_segments = seg_for_size("minor_segments", 12, 3, minor, 0.15);
            torus_arc_mesh(major, minor, arc_deg.to_radians(), major_segments, minor_segments, uv_mode)
        }
        "ellipsoid" => {
            let s = resolve_size3(node, Vec3::ONE);
            let characteristic = s.x.abs().max(s.y.abs()).max(s.z.abs());
            let rings = seg_for_size("rings", 16, 2, characteristic, 1.0);
            let segments = seg_for_size("segments", 24, 3, characteristic, 1.0);
            ellipsoid_mesh([s.x, s.y, s.z], rings, segments, uv_mode)
        }
        "superellipsoid" => {
            let s = resolve_size3(node, Vec3::ONE);
            let ew = node.attr_number("ew").unwrap_or(1.0);
            let ns = node.attr_number("ns").unwrap_or(1.0);
            let characteristic = s.x.abs().max(s.y.abs()).max(s.z.abs());
            let rings = seg_for_size("rings", 16, 2, characteristic, 1.0);
            let segments = seg_for_size("segments", 24, 3, characteristic, 1.0);
            superellipsoid_mesh([s.x, s.y, s.z], ew, ns, rings, segments, uv_mode)
        }
        "curved_plane" => {
            let s = resolve_size_xz(node, [1.0, 1.0]);
            let bend_u = node.attr_number("bend_u").unwrap_or(0.0).to_radians();
            let bend_v = node.attr_number("bend_v").unwrap_or(0.0).to_radians();
            let segments_u = seg("segments_u", 12, 1);
            let segments_v = seg("segments_v", 12, 1);
            curved_plane_mesh(s, bend_u, bend_v, segments_u, segments_v, uv_mode)
        }
        "bezier_patch" => {
            // Author supplies exactly 16 vec3 control points, row-major.
            let points = node.attr_list_vec3("points").unwrap_or_default();
            if points.len() != 16 {
                return Some(Err(anyhow!(
                    "`bezier_patch` requires exactly 16 vec3 control points in `points=`, got {}",
                    points.len(),
                )));
            }
            let segments_u = seg("segments_u", 12, 1);
            let segments_v = seg("segments_v", 12, 1);
            bezier_patch_mesh(&points, segments_u, segments_v, uv_mode)
        }
        "metaball" => {
            let points = node.attr_list_vec3("points").unwrap_or_default();
            if points.is_empty() {
                return Some(Err(anyhow!(
                    "`metaball` requires at least one point in `points=[[x,y,z], …]`",
                )));
            }
            // `radii` (per-point list) takes precedence; else fall back to
            // scalar `radius`. One of the two is required so the author
            // can't accidentally request a metaball with no radii.
            // The grammar parses 3-element lists as `Vec3`, so accept that
            // shape too — same fallback `loft.heights` uses.
            let radii: Vec<f32> = match node.attr("radii") {
                Some(crate::ast::Value::List(v)) => v.to_vec(),
                Some(crate::ast::Value::Vec3(v)) => v.to_vec(),
                _ => match node.attr_number("radius") {
                    Some(r) => vec![r],
                    None => {
                        return Some(Err(anyhow!(
                            "`metaball` requires either a scalar `radius=` or a per-point `radii=[…]`",
                        )));
                    }
                },
            };
            let blend = node.attr_number("blend").unwrap_or(0.0);
            let rings = seg("rings", 12, 2);
            let segments = seg("segments", 16, 3);
            metaball_mesh(&points, &radii, blend, rings, segments, uv_mode)
        }
        "hull" => {
            // Convex hull of a point cloud — the lossless sink for arbitrary
            // convex solids (sheared/sloped blocks) no parametric primitive
            // captures. Needs ≥4 points; fewer can't bound a volume.
            let points = node.attr_list_vec3("points").unwrap_or_default();
            if points.len() < 4 {
                return Some(Err(anyhow!(
                    "`hull` requires at least 4 points in `points=[[x,y,z], …]`, got {}",
                    points.len(),
                )));
            }
            let mesh = hull_mesh(&points);
            // Degenerate input does not come back as an empty mesh: Manifold
            // returns a zero-volume sheet for a coplanar point set, which would
            // export as an invisible, non-watertight node. Test the volume the
            // hull actually bounds rather than trusting it to come back empty.
            if is_degenerate_solid(&mesh) {
                return Some(Err(anyhow!(
                    "`hull` produced no solid geometry — the points bound no volume (all coplanar or collinear)"
                )));
            }
            mesh
        }
        "poly" => {
            // Raw triangle mesh with author-supplied per-vertex UVs — the
            // escape hatch for geometry whose texture mapping must be carried
            // through verbatim (e.g. a map converter re-emitting engine-native
            // block faces, where each face samples an authored atlas
            // sub-rectangle no procedural projection can reproduce). Ignores
            // `uv_mode`: the `uvs=` list is the UV channel.
            let points = node.attr_list_vec3("points").unwrap_or_default();
            let uvs = node.attr_list_pair("uvs").unwrap_or_default();
            // `indices=` is a flat list of vertex indices, three per triangle.
            // The grammar parses every number list as f32; round to u32 here.
            let indices: Vec<u32> = node
                .attr_list("indices")
                .map(|s| s.iter().map(|&f| f.round() as u32).collect())
                .unwrap_or_default();
            return Some(poly_mesh(&points, &uvs, &indices));
        }
        "wall" => {
            // Box cut through along Z by any number of rectangular holes
            // declared as [x, y, w, h] in the wall's local frame.
            let s = resolve_size3(node, Vec3::new(1.0, 1.0, 0.1));
            let wall_box = box_mesh([s.x, s.y, s.z], uv_mode);
            let holes = node.attr_list_quad("holes").unwrap_or_default();
            if holes.is_empty() {
                wall_box
            } else {
                let cutouts: Vec<Mesh> = holes
                    .iter()
                    .map(|&[hx, hy, hw, hh]| {
                        let c = box_mesh([hw.max(1e-4), hh.max(1e-4), s.z + 0.02], uv_mode);
                        transform_mesh(&c, Mat4::from_translation(Vec3::new(hx, hy, 0.0)))
                    })
                    .collect();
                clean_csg_output(&difference_many(&wall_box, &cutouts))
            }
        }
        "lathe" => {
            let profile = node
                .attr_list_pair("profile")
                .unwrap_or_else(|| vec![[0.0, -0.5], [0.5, 0.0], [0.0, 0.5]]);
            let segments = seg("segments", 24, 3);
            let cap_ends = node.attr_number("cap_ends").map(|n| n != 0.0).unwrap_or(true);
            lathe_mesh(&profile, segments, cap_ends, uv_mode)
        }
        "leaf_card" => {
            let s = resolve_size_xy(node, [0.4, 0.4]);
            let cards = node.attr_number("cards").map(|n| n as u32).unwrap_or(2).max(1);
            leaf_card_mesh(s, cards, uv_mode)
        }
        "spline_tube" => {
            let points = node
                .attr_list_vec3("points")
                .unwrap_or_else(|| vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
            // `radii` (list) takes precedence; else fall back to scalar `radius`.
            let radii = if let Some(r) = node.attr_list("radii") {
                r.to_vec()
            } else {
                vec![node.attr_number("radius").unwrap_or(0.1)]
            };
            let segments = seg("segments", 12, 3);
            let samples = seg("samples", 8, 2);
            let cap_ends = node.attr_number("cap_ends").map(|n| n != 0.0).unwrap_or(true);
            spline_tube_mesh(&points, &radii, segments, samples, cap_ends, uv_mode)
        }
        "coil" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let turns = node.attr_number("turns").unwrap_or(3.0);
            let profile_radius = node.attr_number("profile_radius").unwrap_or(0.05);
            let segments = seg("segments", 12, 3);
            let samples = seg("samples", 16, 4);
            let cap_ends = node.attr_number("cap_ends").map(|n| n != 0.0).unwrap_or(true);
            let handedness = match node.attr_string("handedness") {
                Some(s) => match s.to_ascii_lowercase().as_str() {
                    "right" | "rh" | "ccw" => CoilHandedness::Right,
                    "left"  | "lh" | "cw"  => CoilHandedness::Left,
                    other => {
                        return Some(Err(anyhow!(
                            "coil.handedness: expected \"right\" or \"left\", got \"{other}\""
                        )));
                    }
                },
                None => CoilHandedness::Right,
            };
            coil_mesh(
                radius, height, turns, profile_radius,
                segments, samples, cap_ends, handedness, uv_mode,
            )
        }
        "spline_ribbon" => {
            let points = node
                .attr_list_vec3("points")
                .unwrap_or_else(|| vec![[0.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
            // `widths` (list, one per control point) takes precedence; else
            // fall back to scalar `width`.
            let widths = if let Some(w) = node.attr_list("widths") {
                w.to_vec()
            } else {
                vec![node.attr_number("width").unwrap_or(0.1)]
            };
            let samples = seg("samples", 8, 2);
            // Author writes degrees (per the prompt), the mesh builder takes radians.
            let twist = node.attr_number("twist").unwrap_or(0.0).to_radians();
            spline_ribbon_mesh(&points, &widths, samples, twist, uv_mode)
        }
        "extrude" => {
            let outer = node.attr_list_pair("points").unwrap_or_else(|| {
                vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]
            });
            // Single optional inner contour (CW). Multi-hole support is
            // gated on a future grammar change — three-level nested lists
            // can't be expressed in the parser today, so authoring two or
            // more holes today means stacking two `extrude` calls or
            // following up with a `difference`.
            let hole = node.attr_list_pair("hole").unwrap_or_default();
            let holes: Vec<Vec<[f32; 2]>> = if hole.is_empty() {
                Vec::new()
            } else {
                vec![hole]
            };
            let height = node.attr_number("height").unwrap_or(1.0);
            let taper = node.attr_number("taper").unwrap_or(1.0);
            let twist = node.attr_number("twist").unwrap_or(0.0).to_radians();
            let caps = node.attr_number("caps").map(|n| n != 0.0).unwrap_or(true);
            extrude_mesh(&outer, &holes, height, taper, twist, caps, uv_mode)
        }
        "sweep" => {
            let profile = node.attr_list_pair("profile").unwrap_or_else(|| {
                vec![[-0.05, -0.05], [0.05, -0.05], [0.05, 0.05], [-0.05, 0.05]]
            });
            let path = node
                .attr_list_vec3("path")
                .unwrap_or_else(|| vec![[-0.5, 0.0, 0.0], [0.5, 0.0, 0.0]]);
            let samples = seg("samples", 8, 2);
            let twist = node.attr_number("twist").unwrap_or(0.0).to_radians();
            // Author-supplied per-control-point modulation lists. `roll` is
            // in degrees per the prompt, converted to radians here so the
            // kernel can stay in radians throughout.
            let roll: Vec<f32> = node
                .attr_list("roll")
                .map(|s| s.iter().map(|d| d.to_radians()).collect())
                .unwrap_or_default();
            let scale: Vec<f32> = node.attr_list("scale_along")
                .map(|s| s.to_vec())
                .unwrap_or_default();
            let modulation = SweepModulation { roll, scale };
            let caps = node.attr_number("caps").map(|n| n != 0.0).unwrap_or(true);
            sweep_mesh(&profile, &path, samples, twist, &modulation, caps, uv_mode)
        }
        "loft" => {
            // Sections are flat-packed into one `points` list, in section
            // order. `heights` lists the Y of each section. The number of
            // points must be a multiple of `heights.len()` and the per-
            // section vertex count must be ≥ 3.
            let all_points = node.attr_list_pair("points").unwrap_or_default();
            let heights: Vec<f32> = match node.attr("heights") {
                Some(crate::ast::Value::List(v)) => v.to_vec(),
                // 3-element heights parse as Vec3 (grammar prefers vec3
                // over list when arity matches), so honour that shape too.
                Some(crate::ast::Value::Vec3(v)) => v.to_vec(),
                Some(crate::ast::Value::Number(n)) => vec![*n],
                _ => Vec::new(),
            };
            if heights.len() < 2 {
                return Some(Err(anyhow!(
                    "`loft` requires at least 2 entries in `heights=`, got {}",
                    heights.len(),
                )));
            }
            if all_points.is_empty() || all_points.len() % heights.len() != 0 {
                return Some(Err(anyhow!(
                    "`loft` `points=` length ({}) must be a non-zero multiple of \
                     `heights=` length ({}); pack each section's vertices in order \
                     and they must all share the same vertex count",
                    all_points.len(),
                    heights.len(),
                )));
            }
            let per_section = all_points.len() / heights.len();
            if per_section < 3 {
                return Some(Err(anyhow!(
                    "`loft` sections must have at least 3 vertices, got {per_section}"
                )));
            }
            let sections: Vec<Vec<[f32; 2]>> = (0..heights.len())
                .map(|i| all_points[i * per_section..(i + 1) * per_section].to_vec())
                .collect();
            let samples = seg("samples", 4, 1);
            let caps = node.attr_number("caps").map(|n| n != 0.0).unwrap_or(true);
            return Some(loft_mesh(&sections, &heights, samples, caps, uv_mode));
        }
        "mesh" => {
            let src = match node.attr_string("src") {
                Some(s) => s,
                None => {
                    return Some(Err(anyhow!(
                        "`mesh` primitive requires a `src` attribute (a file path or `stdlib:…` key)"
                    )));
                }
            };
            // Filesystem `src` resolves against the directory of the calling
            // .mog. `stdlib:` paths are byte-keyed and don't need a base dir.
            let base = source_dir();
            let load = (|| -> Result<Mesh> {
                // The caller's `Loader::load_binary` gets first refusal (see
                // `collect_mesh_binaries`): a host with no filesystem — the
                // browser lowering `.mog` source out of a fetched asset bundle
                // — can only reach an external mesh this way. Falling through
                // to the disk read when nothing was supplied is what keeps
                // every loader written before that method behaving identically.
                let bytes = match mesh_bytes(src) {
                    Some(b) => b,
                    None => read_glb_bytes(src, base.as_deref())?,
                };
                mesh_from_glb_bytes(&bytes).with_context(|| format!("decoding mesh `{src}`"))
            })();
            return Some(load);
        }
        _ => return None,
    };
    Some(Ok(m))
}

/// Resolve a user-supplied face name (`"+y"`, `"top"`, `"-x"`, …) to the
/// internal `InsetFace` enum. Aliases are case-insensitive and accept both
/// the axis-sign form and English directional names so authors don't have
/// to remember which is which.
fn parse_inset_face(s: &str) -> Result<InsetFace> {
    match s.to_ascii_lowercase().as_str() {
        "+x" | "right" | "east"        => Ok(InsetFace::PosX),
        "-x" | "left"  | "west"        => Ok(InsetFace::NegX),
        "+y" | "top"   | "up"          => Ok(InsetFace::PosY),
        "-y" | "bottom"| "down"        => Ok(InsetFace::NegY),
        "+z" | "front" | "south"       => Ok(InsetFace::PosZ),
        "-z" | "back"  | "north"       => Ok(InsetFace::NegZ),
        other => Err(anyhow!(
            "inset_box.face: expected one of \
             \"+x\"/\"-x\"/\"+y\"/\"-y\"/\"+z\"/\"-z\" \
             (or top/bottom/left/right/front/back), got \"{other}\""
        )),
    }
}
