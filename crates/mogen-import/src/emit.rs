//! [`MeshScene`] → `.mog` source.
//!
//! The baseline mapping is deliberately literal:
//!
//! - each IR material becomes a top-level `material`, with every factor
//!   written out (glTF and `.mog` disagree on defaults, so nothing is left
//!   implicit);
//! - each node becomes a `group` carrying its local `pos` / `rot` / `scale`, or
//!   — when it is a leaf holding a single-material mesh — a bare `poly` with
//!   that transform;
//! - each mesh primitive becomes a `poly`. `poly` takes one `mat=`, which is
//!   why the IR splits meshes by material;
//! - a mesh drawn by more than one node is written once as a parameterless
//!   `module` and placed with `use`.
//!
//! Vertices are written exactly as the reader produced them. `poly` shades
//! shared vertices smooth and unshared ones flat, and glTF already splits
//! vertices wherever normals differ, so hard and soft edges come through.

use std::collections::BTreeSet;
use std::fmt::Write;

use glam::{EulerRot, Quat, Vec3};

use crate::fmt;
use crate::ir::{AlphaMode, Material, MeshData, MeshScene, Node, Primitive};
use crate::normalize::Namer;
use crate::{Asset, Format, Options, Report};

/// Decimal places for rotation angles (degrees) and material factors. Fixed:
/// neither is tied to the model's scale the way positions are.
const ANGLE_PRECISION: usize = 4;
const FACTOR_PRECISION: usize = 4;

pub fn emit(
    scene: &MeshScene,
    report: &Report,
    assets: &[Asset],
    format: Format,
    source_label: &str,
    scene_name: &str,
    opts: &Options,
) -> String {
    let mut out = String::new();
    header(&mut out, report, format, source_label, scene_name);

    for mat in &scene.materials {
        material(&mut out, mat, assets);
    }
    if !scene.materials.is_empty() {
        out.push('\n');
    }

    let mut e = Emitter {
        scene,
        opts,
        names: Namer::default(),
        shared: shared_meshes(scene),
    };
    for node in &scene.nodes {
        e.names.claim(Some(&node.name), "node");
    }
    for mesh in &scene.meshes {
        e.names.claim(Some(&mesh.name), "mesh");
    }

    for (i, mesh) in scene.meshes.iter().enumerate() {
        if !e.shared[i] {
            continue;
        }
        let _ = writeln!(out, "module {} {{", fmt::string(&mesh.name));
        e.mesh_polys(&mut out, mesh, 1);
        out.push_str("}\n\n");
    }

    let _ = writeln!(out, "scene {} {{", fmt::string(scene_name));
    for &root in &scene.roots {
        e.node(&mut out, root, 1);
    }
    out.push_str("}\n");
    out
}

/// Asset indices some material still references.
pub fn referenced_assets(scene: &MeshScene) -> BTreeSet<usize> {
    let mut used = BTreeSet::new();
    for m in &scene.materials {
        let t = &m.textures;
        for slot in [
            t.base_color,
            t.metallic_roughness,
            t.normal,
            t.occlusion,
            t.emissive,
        ] {
            used.extend(slot);
        }
    }
    used
}

/// Meshes placed by more than one node.
fn shared_meshes(scene: &MeshScene) -> Vec<bool> {
    let mut uses = vec![0usize; scene.meshes.len()];
    for node in &scene.nodes {
        if let Some(m) = node.mesh {
            uses[m] += 1;
        }
    }
    uses.into_iter().map(|n| n > 1).collect()
}

fn header(out: &mut String, report: &Report, format: Format, label: &str, scene_name: &str) {
    let _ = writeln!(
        out,
        "// Imported from {} {} as {}.",
        format.label(),
        fmt::string(label),
        fmt::string(scene_name)
    );
    let mut lines = report.summary().into_iter();
    if let Some(counts) = lines.next() {
        let _ = writeln!(out, "// {counts}");
    }
    if !report.skipped.is_empty() {
        let skipped = lines.next().unwrap_or_default();
        let _ = writeln!(out, "// {skipped}");
    }
    let notes: Vec<String> = lines.collect();
    if !notes.is_empty() {
        let _ = writeln!(out, "//\n// {} item(s) needed attention:", notes.len());
        for n in notes {
            let _ = writeln!(out, "//   {n}");
        }
    }
    out.push('\n');
}

fn material(out: &mut String, m: &Material, assets: &[Asset]) {
    let f = |v: f32| fmt::num(v, FACTOR_PRECISION);
    let [r, g, b, a] = m.base_color;
    let mut attrs = vec![
        format!("color={}", fmt::vec([r, g, b], FACTOR_PRECISION)),
        format!("metallic={}", f(m.metallic)),
        format!("roughness={}", f(m.roughness)),
    ];
    match m.alpha_mode {
        // glTF ignores alpha on opaque materials; `.mog` would switch to blend
        // on seeing it, so it is not written.
        AlphaMode::Opaque => {}
        AlphaMode::Blend => {
            attrs.push("alpha_mode=\"blend\"".into());
            attrs.push(format!("alpha={}", f(a)));
        }
        AlphaMode::Mask => {
            attrs.push("alpha_mode=\"mask\"".into());
            attrs.push(format!("alpha_cutoff={}", f(m.alpha_cutoff)));
            if a < 1.0 {
                attrs.push(format!("alpha={}", f(a)));
            }
        }
    }
    if m.emissive.iter().any(|&c| c != 0.0) {
        attrs.push(format!(
            "emissive={}",
            fmt::vec(m.emissive, FACTOR_PRECISION)
        ));
        if m.emissive_strength != 1.0 {
            attrs.push(format!("emissive_strength={}", f(m.emissive_strength)));
        }
    }
    if m.transmission > 0.0 {
        attrs.push(format!("transmission={}", f(m.transmission)));
    }
    if m.double_sided {
        attrs.push("double_sided=1".into());
    }
    let t = &m.textures;
    for (key, slot) in [
        ("base_color_texture", t.base_color),
        ("metallic_roughness_texture", t.metallic_roughness),
        ("normal_texture", t.normal),
        ("occlusion_texture", t.occlusion),
        ("emissive_texture", t.emissive),
    ] {
        if let Some(asset) = slot.and_then(|i| assets.get(i)) {
            attrs.push(format!("{key}={}", fmt::string(&asset.path)));
        }
    }
    let _ = writeln!(
        out,
        "material {} ({})",
        fmt::string(&m.name),
        attrs.join(", ")
    );
}

struct Emitter<'a> {
    scene: &'a MeshScene,
    opts: &'a Options,
    names: Namer,
    shared: Vec<bool>,
}

impl Emitter<'_> {
    fn node(&mut self, out: &mut String, i: usize, depth: usize) {
        let node = &self.scene.nodes[i];
        let trs = self.transform(node);
        let mesh = node.mesh.map(|m| (m, &self.scene.meshes[m]));

        // A leaf with one unshared single-material mesh: the poly *is* the node.
        if let Some((m, mesh)) = mesh {
            if node.children.is_empty() && !self.shared[m] && mesh.primitives.len() == 1 {
                self.poly(out, &node.name, &trs, &mesh.primitives[0], depth);
                return;
            }
        }

        indent(out, depth);
        let _ = write!(out, "group {}", fmt::string(&node.name));
        if !trs.is_empty() {
            let _ = write!(out, " ({})", trs.join(", "));
        }
        out.push_str(" {\n");
        if let Some((m, mesh)) = mesh {
            if self.shared[m] {
                indent(out, depth + 1);
                let _ = writeln!(out, "use {}", fmt::string(&mesh.name));
            } else {
                self.mesh_polys(out, mesh, depth + 1);
            }
        }
        for &c in &node.children {
            self.node(out, c, depth + 1);
        }
        indent(out, depth);
        out.push_str("}\n");
    }

    /// One `poly` per primitive, named after the mesh (and material, when
    /// there is more than one).
    fn mesh_polys(&mut self, out: &mut String, mesh: &MeshData, depth: usize) {
        for prim in &mesh.primitives {
            let name = if mesh.primitives.len() == 1 {
                mesh.name.clone()
            } else {
                let mat = prim
                    .material
                    .map(|m| self.scene.materials[m].name.as_str())
                    .unwrap_or("default");
                self.names
                    .claim(Some(&format!("{}_{mat}", mesh.name)), "mesh")
            };
            self.poly(out, &name, &[], prim, depth);
        }
    }

    fn poly(&self, out: &mut String, name: &str, trs: &[String], prim: &Primitive, depth: usize) {
        let p = self.opts.precision;
        indent(out, depth);
        let _ = writeln!(out, "poly {} (", fmt::string(name));
        let mut attrs: Vec<String> = trs.to_vec();
        attrs.push(format!("points={}", fmt::vec_list(&prim.positions, p)));
        if !prim.uvs.is_empty() {
            attrs.push(format!("uvs={}", fmt::vec_list(&prim.uvs, p)));
        }
        attrs.push(format!("indices={}", fmt::index_list(&prim.indices)));
        if let Some(m) = prim.material {
            attrs.push(format!(
                "mat={}",
                fmt::string(&self.scene.materials[m].name)
            ));
        }
        let last = attrs.len() - 1;
        for (i, a) in attrs.iter().enumerate() {
            indent(out, depth + 1);
            out.push_str(a);
            out.push_str(if i == last { "\n" } else { ",\n" });
        }
        indent(out, depth);
        out.push_str(")\n");
    }

    /// `pos` / `rot` / `scale` (each only when it is not the identity), then
    /// `role` / `tags` when the source had them.
    fn transform(&self, node: &Node) -> Vec<String> {
        let p = self.opts.precision;
        let mut attrs = Vec::new();
        let t = node.translation;
        if fmt::num(t.x, p) != "0" || fmt::num(t.y, p) != "0" || fmt::num(t.z, p) != "0" {
            attrs.push(format!("pos={}", fmt::vec(t.to_array(), p)));
        }
        let euler = euler_degrees(node.rotation);
        if euler.iter().any(|&a| fmt::num(a, ANGLE_PRECISION) != "0") {
            attrs.push(format!("rot={}", fmt::vec(euler, ANGLE_PRECISION)));
        }
        let s = node.scale;
        let uniform = s.x == s.y && s.y == s.z;
        if uniform {
            if fmt::num(s.x, p) != "1" {
                attrs.push(format!("scale={}", fmt::num(s.x, p)));
            }
        } else if s != Vec3::ONE {
            attrs.push(format!("scale={}", fmt::vec(s.to_array(), p)));
        }
        if let Some(role) = &node.role {
            attrs.push(format!("role={}", fmt::string(role)));
        }
        if !node.tags.is_empty() {
            attrs.push(format!("tags={}", fmt::string(&node.tags.join(","))));
        }
        attrs
    }
}

/// The `rot=[x, y, z]` that lowering (`Quat::from_euler(EulerRot::XYZ, …)` in
/// degrees) turns back into `q`.
fn euler_degrees(q: Quat) -> [f32; 3] {
    let (x, y, z) = q.normalize().to_euler(EulerRot::XYZ);
    [x.to_degrees(), y.to_degrees(), z.to_degrees()]
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euler_round_trips_through_lowering_convention() {
        let q = Quat::from_euler(
            EulerRot::XYZ,
            30f32.to_radians(),
            -45f32.to_radians(),
            100f32.to_radians(),
        );
        let [x, y, z] = euler_degrees(q);
        let back = Quat::from_euler(
            EulerRot::XYZ,
            x.to_radians(),
            y.to_radians(),
            z.to_radians(),
        );
        assert!(q.dot(back).abs() > 0.99999, "{q:?} vs {back:?}");
    }
}
