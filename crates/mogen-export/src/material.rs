use serde_json::{json, Value};

use mogen_core::{AlphaMode, Material};

use crate::texture::{SlotKind, TextureTable};

pub(crate) fn emit_material(m: &Material, textures: &TextureTable) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(m.name.clone()));

    let mut pbr = serde_json::Map::new();
    pbr.insert("baseColorFactor".into(), json!(m.base_color));
    pbr.insert("metallicFactor".into(), json!(m.metallic));
    pbr.insert("roughnessFactor".into(), json!(m.roughness));
    if let Some(idx) = textures.index_of(&m.base_color_texture, SlotKind::Color) {
        pbr.insert("baseColorTexture".into(), json!({ "index": idx }));
    }
    if let Some(idx) = textures.index_of(&m.metallic_roughness_texture, SlotKind::Linear) {
        pbr.insert("metallicRoughnessTexture".into(), json!({ "index": idx }));
    }
    obj.insert("pbrMetallicRoughness".into(), Value::Object(pbr));

    if let Some(idx) = textures.index_of(&m.normal_texture, SlotKind::Linear) {
        obj.insert("normalTexture".into(), json!({ "index": idx }));
    }
    if let Some(idx) = textures.index_of(&m.occlusion_texture, SlotKind::Linear) {
        obj.insert("occlusionTexture".into(), json!({ "index": idx }));
    }
    if let Some(idx) = textures.index_of(&m.emissive_texture, SlotKind::Color) {
        obj.insert("emissiveTexture".into(), json!({ "index": idx }));
    }

    match m.alpha_mode {
        AlphaMode::Opaque => {}
        AlphaMode::Blend => {
            obj.insert("alphaMode".into(), Value::String("BLEND".into()));
        }
        AlphaMode::Mask => {
            obj.insert("alphaMode".into(), Value::String("MASK".into()));
            obj.insert("alphaCutoff".into(), json!(m.alpha_cutoff));
        }
    }

    if m.emissive != [0.0, 0.0, 0.0] {
        obj.insert("emissiveFactor".into(), json!(m.emissive));
    }

    if m.double_sided {
        obj.insert("doubleSided".into(), json!(true));
    }

    let mut extensions = serde_json::Map::new();
    if m.emissive_strength != 1.0 && m.emissive != [0.0, 0.0, 0.0] {
        extensions.insert(
            "KHR_materials_emissive_strength".into(),
            json!({ "emissiveStrength": m.emissive_strength }),
        );
    }
    if m.transmission > 0.0 {
        extensions.insert(
            "KHR_materials_transmission".into(),
            json!({ "transmissionFactor": m.transmission }),
        );
    }
    if !extensions.is_empty() {
        obj.insert("extensions".into(), Value::Object(extensions));
    }

    Value::Object(obj)
}

pub(crate) fn collect_material_extensions(materials: &[Material]) -> Vec<&'static str> {
    let mut used: Vec<&'static str> = Vec::new();
    let push = |name: &'static str, list: &mut Vec<&'static str>| {
        if !list.contains(&name) {
            list.push(name);
        }
    };
    for m in materials {
        if m.emissive_strength != 1.0 && m.emissive != [0.0, 0.0, 0.0] {
            push("KHR_materials_emissive_strength", &mut used);
        }
        if m.transmission > 0.0 {
            push("KHR_materials_transmission", &mut used);
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mat(name: &str) -> Material {
        Material::new(name)
    }

    #[test]
    fn opaque_material_omits_extensions_and_alpha_mode() {
        let m = mat("base");
        let v = emit_material(&m, &TextureTable::default());
        assert!(v.get("alphaMode").is_none());
        assert!(v.get("emissiveFactor").is_none());
        assert!(v.get("extensions").is_none());
    }

    #[test]
    fn blend_material_emits_alpha_mode_but_no_cutoff() {
        let mut m = mat("gel");
        m.alpha_mode = AlphaMode::Blend;
        m.base_color[3] = 0.3;
        let v = emit_material(&m, &TextureTable::default());
        assert_eq!(v["alphaMode"], json!("BLEND"));
        assert!(v.get("alphaCutoff").is_none());
    }

    #[test]
    fn mask_material_emits_cutoff() {
        let mut m = mat("leaf");
        m.alpha_mode = AlphaMode::Mask;
        m.alpha_cutoff = 0.25;
        let v = emit_material(&m, &TextureTable::default());
        assert_eq!(v["alphaMode"], json!("MASK"));
        assert_eq!(v["alphaCutoff"], json!(0.25));
    }

    #[test]
    fn double_sided_emits_core_flag_without_extension() {
        // `doubleSided` is a core glTF property — no extension to register.
        let mut m = mat("leaf");
        m.double_sided = true;
        let v = emit_material(&m, &TextureTable::default());
        assert_eq!(v["doubleSided"], json!(true));
        assert!(v.get("extensions").is_none());
        assert!(collect_material_extensions(std::slice::from_ref(&m)).is_empty());

        // Default stays off, and off materials omit the field entirely.
        let off = mat("wood");
        let v_off = emit_material(&off, &TextureTable::default());
        assert!(v_off.get("doubleSided").is_none());
    }

    #[test]
    fn transmission_triggers_extension_and_extensions_used() {
        let mut m = mat("glass");
        m.transmission = 0.8;
        let v = emit_material(&m, &TextureTable::default());
        let got = v["extensions"]["KHR_materials_transmission"]["transmissionFactor"]
            .as_f64()
            .unwrap();
        assert_eq!(got as f32, 0.8);
        let used = collect_material_extensions(std::slice::from_ref(&m));
        assert_eq!(used, vec!["KHR_materials_transmission"]);
    }

    #[test]
    fn emissive_strength_only_counts_when_emissive_is_nonzero() {
        // Strength alone, with zero emissive, is not a useful extension.
        let mut m = mat("dim");
        m.emissive_strength = 5.0;
        assert!(collect_material_extensions(std::slice::from_ref(&m)).is_empty());

        // With a real emissive colour, the extension should ship.
        m.emissive = [1.0, 0.2, 1.0];
        let v = emit_material(&m, &TextureTable::default());
        assert!(v.get("emissiveFactor").is_some());
        let got = v["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"]
            .as_f64()
            .unwrap();
        assert_eq!(got as f32, 5.0);
    }

    #[test]
    fn default_emissive_strength_with_emissive_does_not_add_extension() {
        let mut m = mat("dim_glow");
        m.emissive = [0.1, 0.1, 0.1];
        // strength stays at the 1.0 default
        let v = emit_material(&m, &TextureTable::default());
        assert!(v.get("emissiveFactor").is_some());
        assert!(v.get("extensions").is_none());
    }

    #[test]
    fn extensions_used_deduplicates_across_materials() {
        let mut a = mat("a");
        a.transmission = 0.5;
        let mut b = mat("b");
        b.transmission = 0.7;
        let used = collect_material_extensions(&[a, b]);
        assert_eq!(used, vec!["KHR_materials_transmission"]);
    }
}
