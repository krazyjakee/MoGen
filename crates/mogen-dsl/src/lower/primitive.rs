use anyhow::{anyhow, Context, Result};
use glam::{Mat4, Vec3};

use mogen_core::{Mesh, UvMode};
use mogen_geom::{
    box_mesh, capsule_mesh, clean_csg_output, cone_mesh, curved_plane_mesh, cylinder_mesh,
    difference_many, disc_mesh, ellipsoid_mesh, frustum_mesh, half_cylinder_mesh, hemisphere_mesh,
    icosphere_mesh, lathe_mesh, leaf_card_mesh, mesh_from_glb_bytes, plane_mesh, prism_mesh,
    pyramid_mesh, quad_mesh, read_glb_bytes, rounded_box_mesh, sphere_mesh, spline_ribbon_mesh,
    spline_tube_mesh, superellipsoid_mesh, torus_arc_mesh, torus_mesh, transform_mesh, tube_mesh,
    wedge_mesh,
};

use crate::ast::Node;
use crate::lower::source_dir;

use super::helpers::{resolve_size3, resolve_size_xy, resolve_size_xz};
use super::lod::{scaled_default, scaled_subdivisions};

/// Dispatch a primitive `Node` to its mesh builder. Returns `None` for non-
/// primitive kinds (group, scene, material, CSG ops, animation decls, …) so
/// callers can handle those separately. The inner `Result` carries failures
/// from primitives whose construction can fail at lowering time (e.g. `mesh`
/// loading a `.glb` from disk).
pub(super) fn primitive_mesh(node: &Node, uv_mode: UvMode) -> Option<Result<Mesh>> {
    let m: Mesh = match node.kind.as_str() {
        "box" | "slab" | "post" | "panel" => {
            let s = resolve_size3(node, Vec3::ONE);
            box_mesh([s.x, s.y, s.z], uv_mode)
        }
        "plane" => {
            let s = resolve_size_xz(node, [1.0, 1.0]);
            plane_mesh(s, uv_mode)
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
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            cylinder_mesh(radius, height, segments, uv_mode)
        }
        "cone" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            cone_mesh(radius, height, segments, uv_mode)
        }
        "sphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or_else(|| scaled_default(16, 2));
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            sphere_mesh(radius, rings, segments, uv_mode)
        }
        "capsule" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or_else(|| scaled_default(8, 2));
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            capsule_mesh(radius, height, rings, segments, uv_mode)
        }
        "torus" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let major_segments = node
                .attr_number("major_segments")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(24, 3));
            let minor_segments = node
                .attr_number("minor_segments")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(12, 3));
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
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            disc_mesh(radius, segments, uv_mode)
        }
        "icosphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let subdivisions = node
                .attr_number("subdivisions")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_subdivisions(2));
            icosphere_mesh(radius, subdivisions, uv_mode)
        }
        "rounded_box" => {
            let s = resolve_size3(node, Vec3::ONE);
            let radius = node.attr_number("radius").unwrap_or(0.1);
            let segments = node
                .attr_number("segments")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(4, 1));
            rounded_box_mesh([s.x, s.y, s.z], radius, segments, uv_mode)
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
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            tube_mesh(outer, inner, height, segments, uv_mode)
        }
        "hemisphere" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or_else(|| scaled_default(8, 2));
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            hemisphere_mesh(radius, rings, segments, uv_mode)
        }
        "half_cylinder" => {
            let radius = node.attr_number("radius").unwrap_or(0.5);
            let height = node.attr_number("height").unwrap_or(1.0);
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            half_cylinder_mesh(radius, height, segments, uv_mode)
        }
        "torus_arc" => {
            let major = node.attr_number("major").unwrap_or(0.5);
            let minor = node.attr_number("minor").unwrap_or(0.15);
            let arc_deg = node.attr_number("arc").unwrap_or(90.0);
            let major_segments = node
                .attr_number("major_segments")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(24, 3));
            let minor_segments = node
                .attr_number("minor_segments")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(12, 3));
            torus_arc_mesh(major, minor, arc_deg.to_radians(), major_segments, minor_segments, uv_mode)
        }
        "ellipsoid" => {
            let s = resolve_size3(node, Vec3::ONE);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or_else(|| scaled_default(16, 2));
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            ellipsoid_mesh([s.x, s.y, s.z], rings, segments, uv_mode)
        }
        "superellipsoid" => {
            let s = resolve_size3(node, Vec3::ONE);
            let ew = node.attr_number("ew").unwrap_or(1.0);
            let ns = node.attr_number("ns").unwrap_or(1.0);
            let rings = node.attr_number("rings").map(|n| n as u32).unwrap_or_else(|| scaled_default(16, 2));
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
            superellipsoid_mesh([s.x, s.y, s.z], ew, ns, rings, segments, uv_mode)
        }
        "curved_plane" => {
            let s = resolve_size_xz(node, [1.0, 1.0]);
            let bend_u = node.attr_number("bend_u").unwrap_or(0.0).to_radians();
            let bend_v = node.attr_number("bend_v").unwrap_or(0.0).to_radians();
            let segments_u = node
                .attr_number("segments_u")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(12, 1));
            let segments_v = node
                .attr_number("segments_v")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(12, 1));
            curved_plane_mesh(s, bend_u, bend_v, segments_u, segments_v, uv_mode)
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
            let segments = node.attr_number("segments").map(|n| n as u32).unwrap_or_else(|| scaled_default(24, 3));
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
            let segments = node
                .attr_number("segments")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(12, 3));
            let samples = node
                .attr_number("samples")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(8, 2));
            let cap_ends = node.attr_number("cap_ends").map(|n| n != 0.0).unwrap_or(true);
            spline_tube_mesh(&points, &radii, segments, samples, cap_ends, uv_mode)
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
            let samples = node
                .attr_number("samples")
                .map(|n| n as u32)
                .unwrap_or_else(|| scaled_default(8, 2));
            // Author writes degrees (per the prompt), the mesh builder takes radians.
            let twist = node.attr_number("twist").unwrap_or(0.0).to_radians();
            spline_ribbon_mesh(&points, &widths, samples, twist, uv_mode)
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
                let bytes = read_glb_bytes(src, base.as_deref())?;
                mesh_from_glb_bytes(&bytes).with_context(|| format!("decoding mesh `{src}`"))
            })();
            return Some(load);
        }
        _ => return None,
    };
    Some(Ok(m))
}
