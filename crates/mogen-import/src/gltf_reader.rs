//! glTF 2.0 / GLB → [`MeshScene`].
//!
//! Reads the default scene (or the first, if none is marked), walking nodes
//! depth-first from its roots in source order. Everything the IR has no place
//! for — cameras, lights, skins, animations, morph targets, vertex colours,
//! extra UV sets, point and line primitives — is counted in the report.
//!
//! Textures are copied out as their original encoded bytes (PNG / JPEG); they
//! are never decoded, so nothing is lost and nothing depends on an image
//! codec's version.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use glam::{Quat, Vec3};
use gltf::mesh::Mode;
use gltf::Semantic;

use crate::ir::{AlphaMode, Material, MeshData, Node, Primitive, Textures};
use crate::normalize::{sanitize, Namer};
use crate::{Asset, Ingested, Report};

/// Extensions whose effect the import reproduces (or whose absence changes
/// nothing about the geometry). Anything else in `extensionsUsed` is reported.
const UNDERSTOOD_EXTENSIONS: &[&str] = &[
    "KHR_lights_punctual", // reported per light
    "KHR_materials_emissive_strength",
    "KHR_materials_transmission",
];

pub fn read(bytes: &[u8], base: Option<&Path>, texture_dir: &str) -> Result<Ingested> {
    let gltf = gltf::Gltf::from_slice(bytes).context("not a valid glTF / GLB file")?;
    let doc = gltf.document;
    let buffers = gltf::import_buffers(&doc, base, gltf.blob)
        .context("loading glTF buffers (external .bin files resolve next to the input)")?;

    let mut r = Reader {
        doc: &doc,
        buffers: &buffers,
        base,
        texture_dir,
        out: Ingested::default(),
        node_map: vec![None; doc.nodes().len()],
        mesh_map: vec![None; doc.meshes().len()],
        material_map: vec![None; doc.materials().len()],
        default_material: None,
        image_map: vec![None; doc.images().len()],
        asset_names: Namer::default(),
    };
    r.read_scene()?;
    Ok(r.out)
}

struct Reader<'a> {
    doc: &'a gltf::Document,
    buffers: &'a [gltf::buffer::Data],
    base: Option<&'a Path>,
    texture_dir: &'a str,
    out: Ingested,
    /// glTF index → IR index, filled on first visit.
    node_map: Vec<Option<usize>>,
    mesh_map: Vec<Option<usize>>,
    material_map: Vec<Option<usize>>,
    /// The IR material standing in for glTF's implicit default.
    default_material: Option<usize>,
    /// glTF image index → asset index; `Some(None)` = tried and failed.
    image_map: Vec<Option<Option<usize>>>,
    asset_names: Namer,
}

impl Reader<'_> {
    fn report(&mut self) -> &mut Report {
        &mut self.out.report
    }

    fn read_scene(&mut self) -> Result<()> {
        let doc = self.doc;
        for ext in doc.extensions_used() {
            if !UNDERSTOOD_EXTENSIONS.contains(&ext) {
                let required = doc.extensions_required().any(|r| r == ext);
                self.report().note(format!(
                    "extension {ext} is not supported{}; its effect is ignored",
                    if required {
                        " (the file marks it required)"
                    } else {
                        ""
                    }
                ));
            }
        }

        let scene = doc.default_scene().or_else(|| doc.scenes().next());
        let roots: Vec<gltf::Node> = match scene {
            Some(s) => s.nodes().collect(),
            // No scenes at all: every node that is nobody's child is a root.
            None => {
                let mut is_child = vec![false; doc.nodes().len()];
                for n in doc.nodes() {
                    for c in n.children() {
                        is_child[c.index()] = true;
                    }
                }
                doc.nodes().filter(|n| !is_child[n.index()]).collect()
            }
        };
        let extra_scenes = doc.scenes().len().saturating_sub(1);
        self.report().skip_n("additional scenes", extra_scenes);

        for root in roots {
            if let Some(i) = self.read_node(&root)? {
                self.out.scene.roots.push(i);
            }
        }

        let animations = doc.animations().len();
        self.report().skip_n("animations", animations);
        let skins = doc.skins().len();
        self.report().skip_n("skins", skins);
        Ok(())
    }

    fn read_node(&mut self, node: &gltf::Node) -> Result<Option<usize>> {
        if self.node_map[node.index()].is_some() {
            // glTF forbids a node having two parents; a file that does it
            // anyway gets the first placement only.
            self.report()
                .note("a node was referenced more than once; later references ignored");
            return Ok(None);
        }
        let (t, r, s) = node.transform().decomposed();
        let (role, mut tags, kind) = semantic_extras(node.extras());
        // MoGen's `debug_show_poi` markers: placement spheres the validator
        // exempts from the connectivity check by kind. A `poly` has no such
        // kind, so the exemption is carried as a tag instead.
        if kind.as_deref() == Some("poi") && node.mesh().is_some() {
            if !tags.iter().any(|t| t == "floating") {
                tags.push("floating".to_string());
            }
            self.report()
                .note("MoGen POI marker meshes imported as polys tagged `floating`");
        }
        let idx = self.out.scene.nodes.len();
        self.node_map[node.index()] = Some(idx);
        self.out.scene.nodes.push(Node {
            source_name: node.name().map(str::to_string),
            name: String::new(),
            translation: Vec3::from(t),
            rotation: Quat::from_array(r).normalize(),
            scale: Vec3::from(s),
            mesh: None,
            children: Vec::new(),
            role,
            tags,
        });

        if node.camera().is_some() {
            self.report().skip("cameras");
        }
        if node.light().is_some() {
            self.report().skip("lights");
        }
        if node.weights().is_some_and(|w| !w.is_empty()) {
            self.report().skip("morph target weights");
        }
        if let Some(mesh) = node.mesh() {
            let m = self.read_mesh(&mesh)?;
            self.out.scene.nodes[idx].mesh = Some(m);
        }

        for child in node.children() {
            if let Some(c) = self.read_node(&child)? {
                self.out.scene.nodes[idx].children.push(c);
            }
        }
        Ok(Some(idx))
    }

    fn read_mesh(&mut self, mesh: &gltf::Mesh) -> Result<usize> {
        if let Some(i) = self.mesh_map[mesh.index()] {
            return Ok(i);
        }
        let mut data = MeshData {
            source_name: mesh.name().map(str::to_string),
            ..Default::default()
        };
        for prim in mesh.primitives() {
            if let Some(p) = self.read_primitive(&prim)? {
                data.primitives.push(p);
            }
        }
        let i = self.out.scene.meshes.len();
        self.out.scene.meshes.push(data);
        self.mesh_map[mesh.index()] = Some(i);
        Ok(i)
    }

    fn read_primitive(&mut self, prim: &gltf::Primitive) -> Result<Option<Primitive>> {
        let mode = prim.mode();
        if matches!(
            mode,
            Mode::Points | Mode::Lines | Mode::LineLoop | Mode::LineStrip
        ) {
            self.report().skip("point / line primitives");
            return Ok(None);
        }
        if prim.morph_targets().len() > 0 {
            self.report().skip("morph targets");
        }
        for (semantic, _) in prim.attributes() {
            match semantic {
                Semantic::Colors(_) => self.report().note("vertex colours dropped (COLOR_n)"),
                Semantic::TexCoords(n) if n > 0 => self
                    .report()
                    .note(format!("extra UV set TEXCOORD_{n} dropped")),
                _ => {}
            }
        }
        if prim.get(&Semantic::Joints(0)).is_some() {
            self.report()
                .note("skin weights dropped; the mesh is imported in its bind pose");
        }

        let buffers = self.buffers;
        let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
        let Some(positions) = reader.read_positions() else {
            self.report().skip("primitives without POSITION");
            return Ok(None);
        };
        let positions: Vec<[f32; 3]> = positions.collect();
        let normals: Vec<[f32; 3]> = reader
            .read_normals()
            .map(|n| n.collect())
            .unwrap_or_default();
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .map(|t| t.into_f32().collect())
            .unwrap_or_default();
        let raw: Vec<u32> = match reader.read_indices() {
            Some(i) => i.into_u32().collect(),
            None => (0..positions.len() as u32).collect(),
        };
        let indices = match mode {
            Mode::TriangleStrip => strip_to_list(&raw),
            Mode::TriangleFan => fan_to_list(&raw),
            _ => raw,
        };

        let material = self.read_material(&prim.material())?;
        Ok(Some(Primitive {
            positions,
            normals,
            uvs,
            indices,
            material: Some(material),
        }))
    }

    fn read_material(&mut self, mat: &gltf::Material) -> Result<usize> {
        let cached = match mat.index() {
            Some(i) => self.material_map[i],
            None => self.default_material,
        };
        if let Some(i) = cached {
            return Ok(i);
        }

        let pbr = mat.pbr_metallic_roughness();
        let mut m = Material {
            source_name: match mat.index() {
                Some(_) => mat.name().map(str::to_string),
                None => Some("default".to_string()),
            },
            base_color: pbr.base_color_factor(),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: mat.emissive_factor(),
            emissive_strength: mat.emissive_strength().unwrap_or(1.0),
            transmission: mat
                .transmission()
                .map(|t| t.transmission_factor())
                .unwrap_or(0.0),
            alpha_mode: match mat.alpha_mode() {
                gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                gltf::material::AlphaMode::Mask => AlphaMode::Mask,
                gltf::material::AlphaMode::Blend => AlphaMode::Blend,
            },
            alpha_cutoff: mat.alpha_cutoff().unwrap_or(0.5),
            double_sided: mat.double_sided(),
            ..Default::default()
        };

        let label = mat.name().unwrap_or("material").to_string();
        m.textures = Textures {
            base_color: self.read_texture(
                pbr.base_color_texture()
                    .map(|i| (i.texture(), i.tex_coord())),
                &label,
                "base_color",
            )?,
            metallic_roughness: self.read_texture(
                pbr.metallic_roughness_texture()
                    .map(|i| (i.texture(), i.tex_coord())),
                &label,
                "metallic_roughness",
            )?,
            normal: self.read_texture(
                mat.normal_texture().map(|i| (i.texture(), i.tex_coord())),
                &label,
                "normal",
            )?,
            occlusion: self.read_texture(
                mat.occlusion_texture()
                    .map(|i| (i.texture(), i.tex_coord())),
                &label,
                "occlusion",
            )?,
            emissive: self.read_texture(
                mat.emissive_texture().map(|i| (i.texture(), i.tex_coord())),
                &label,
                "emissive",
            )?,
        };
        if mat.normal_texture().is_some_and(|n| n.scale() != 1.0) {
            self.report()
                .note("normal texture scale is not supported; imported at 1.0");
        }
        if mat.occlusion_texture().is_some_and(|o| o.strength() != 1.0) {
            self.report()
                .note("occlusion texture strength is not supported; imported at 1.0");
        }

        let i = self.out.scene.materials.len();
        self.out.scene.materials.push(m);
        match mat.index() {
            Some(g) => self.material_map[g] = Some(i),
            None => self.default_material = Some(i),
        }
        Ok(i)
    }

    /// Extract one texture's image as an asset, reusing it if another slot
    /// already pulled the same image out.
    fn read_texture(
        &mut self,
        tex: Option<(gltf::Texture, u32)>,
        material: &str,
        slot: &str,
    ) -> Result<Option<usize>> {
        let Some((tex, set)) = tex else {
            return Ok(None);
        };
        if set != 0 {
            self.report().note(format!(
                "{slot} texture on material \"{material}\" uses TEXCOORD_{set}; dropped (only TEXCOORD_0 is imported)"
            ));
            return Ok(None);
        }
        let image = tex.source();
        if let Some(done) = self.image_map[image.index()] {
            return Ok(done);
        }
        let result = match self.image_bytes(&image) {
            Ok((bytes, ext)) => {
                let stem = image
                    .name()
                    .map(|n| n.to_string())
                    .or_else(|| match image.source() {
                        gltf::image::Source::Uri { uri, .. } if !uri.starts_with("data:") => {
                            Path::new(uri)
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| format!("{material}_{slot}"));
                let name = self
                    .asset_names
                    .claim(Some(&sanitize(&stem, "texture")), "texture");
                let i = self.out.assets.len();
                self.out.assets.push(Asset {
                    path: format!("{}/{name}.{ext}", self.texture_dir),
                    bytes,
                });
                Some(i)
            }
            Err(e) => {
                self.report().note(format!(
                    "{slot} texture on material \"{material}\" dropped: {e}"
                ));
                None
            }
        };
        self.image_map[image.index()] = Some(result);
        Ok(result)
    }

    /// The encoded bytes of an image and the extension to save them under.
    fn image_bytes(&self, image: &gltf::Image) -> Result<(Vec<u8>, &'static str)> {
        let (bytes, mime) = match image.source() {
            gltf::image::Source::View { view, mime_type } => {
                let buf = &self.buffers[view.buffer().index()].0;
                let start = view.offset();
                let end = start + view.length();
                let bytes = buf
                    .get(start..end)
                    .ok_or_else(|| anyhow!("image buffer view is out of range"))?
                    .to_vec();
                (bytes, Some(mime_type.to_string()))
            }
            gltf::image::Source::Uri { uri, mime_type } => {
                if let Some(rest) = uri.strip_prefix("data:") {
                    let (header, payload) = rest
                        .split_once(',')
                        .ok_or_else(|| anyhow!("malformed data URI"))?;
                    if !header.ends_with(";base64") {
                        return Err(anyhow!("data URI is not base64-encoded"));
                    }
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(payload)
                        .context("decoding base64 image")?;
                    let mime = header.trim_end_matches(";base64").to_string();
                    (bytes, Some(mime_type.map(str::to_string).unwrap_or(mime)))
                } else {
                    let base = self
                        .base
                        .ok_or_else(|| anyhow!("external image {uri:?} with no base directory"))?;
                    let path = base.join(percent_decode(uri));
                    let bytes = std::fs::read(&path)
                        .with_context(|| format!("reading {}", path.display()))?;
                    (bytes, mime_type.map(str::to_string))
                }
            }
        };
        let ext = sniff(&bytes).ok_or_else(|| {
            anyhow!(
                "{} images are not supported (PNG and JPEG only)",
                mime.as_deref().unwrap_or("unrecognised")
            )
        })?;
        Ok((bytes, ext))
    }
}

/// `role`, `tags` and `kind` from a node's `extras`, in the shape MoGen writes
/// them (`{"kind": "box", "role": "leg", "tags": ["a", "b"]}`). Anything else
/// in `extras` is another tool's business and is left alone.
fn semantic_extras(extras: &gltf::json::Extras) -> (Option<String>, Vec<String>, Option<String>) {
    let Some(raw) = extras else {
        return (None, Vec::new(), None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.get()) else {
        return (None, Vec::new(), None);
    };
    let kind = value
        .get("kind")
        .and_then(|k| k.as_str())
        .map(str::to_string);
    let role = value
        .get("role")
        .and_then(|r| r.as_str())
        .map(str::to_string);
    let tags = match value.get("tags") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|t| t.as_str())
            .map(str::to_string)
            .collect(),
        Some(serde_json::Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    (role, tags, kind)
}

/// Identify PNG / JPEG by magic bytes. The declared MIME type is advisory and
/// wrong often enough in the wild that the bytes are the better authority.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else {
        None
    }
}

/// Relative URIs in glTF are percent-encoded (`my%20texture.png`).
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `TRIANGLE_STRIP` → list, alternating winding so every triangle keeps the
/// strip's facing.
fn strip_to_list(idx: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(idx.len().saturating_sub(2) * 3);
    for i in 2..idx.len() {
        if i % 2 == 0 {
            out.extend_from_slice(&[idx[i - 2], idx[i - 1], idx[i]]);
        } else {
            out.extend_from_slice(&[idx[i - 1], idx[i - 2], idx[i]]);
        }
    }
    out
}

/// `TRIANGLE_FAN` → list.
fn fan_to_list(idx: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(idx.len().saturating_sub(2) * 3);
    for i in 2..idx.len() {
        out.extend_from_slice(&[idx[i - 1], idx[i], idx[0]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_and_fans_become_lists_with_consistent_winding() {
        assert_eq!(strip_to_list(&[0, 1, 2, 3]), vec![0, 1, 2, 2, 1, 3]);
        assert_eq!(fan_to_list(&[0, 1, 2, 3]), vec![1, 2, 0, 2, 3, 0]);
        assert!(strip_to_list(&[0, 1]).is_empty());
    }

    #[test]
    fn percent_encoded_uris_decode() {
        assert_eq!(percent_decode("my%20tex.png"), "my tex.png");
        assert_eq!(percent_decode("plain.png"), "plain.png");
        assert_eq!(percent_decode("bad%zz.png"), "bad%zz.png");
        assert_eq!(percent_decode("end%2"), "end%2");
    }

    #[test]
    fn mogen_role_and_tags_are_read_from_extras() {
        let raw = serde_json::value::RawValue::from_string(
            r#"{"kind":"box","role":"leg","tags":["floating","wood"]}"#.to_string(),
        )
        .unwrap();
        let (role, tags, kind) = semantic_extras(&Some(raw));
        assert_eq!(role.as_deref(), Some("leg"));
        assert_eq!(tags, vec!["floating", "wood"]);
        assert_eq!(kind.as_deref(), Some("box"));
        assert_eq!(semantic_extras(&None), (None, vec![], None));
    }

    #[test]
    fn images_are_identified_by_their_bytes() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff(b"RIFF....WEBP"), None);
    }
}
