//! Worker functions for each wizard stage. The UI spawns these on dedicated
//! threads and reads the result via the wizard's mpsc channel.
//!
//! Every worker takes everything it needs by value so it can run independent
//! of the app state. Anything sharing GL or `MogenStudioApp` lives back in
//! `ui_wizard.rs` instead.

use std::path::PathBuf;
use std::sync::Arc;

use mogen_core::subtree_local_aabb;
use mogen_dsl::ast::Node;
use mogen_llm::{
    apply_style_to_prompt, generate_with_repair, embed_seed_header, GenerateConfig, LlmClient,
    RepairConfig, Style, ThinkingLevel,
};
use mogen_llm::image_client::{ImageClient, ImageError};

use crate::app::util::Credential;

use super::state::{
    ObjectEntry, ObjectGenResult, ObjectReview, PositionCorrection, PositionGuide, SceneReview,
};

/// Runtime knobs shared by every wizard worker. Built once in `ui_wizard`
/// from the live Settings + resolved credential and cloned per call.
///
/// `claude_code_path` and `zai_base_url` round-trip through
/// [`build_text_client`] but aren't read here — they go straight into the
/// provider-client constructor.
#[derive(Clone)]
pub struct WizardRunConfig {
    pub model: String,
    pub thinking: ThinkingLevel,
    pub temperature: f32,
    pub max_repair_iters: u32,
    pub seed: u64,
    pub style: Option<Style>,
    #[allow(dead_code)]
    pub claude_code_path: String,
    #[allow(dead_code)]
    pub zai_base_url: String,
    pub session_id: String,
}

/// Stage 1: produce a one-paragraph "scene brief" from the user's free-form
/// prompt. The brief is what every downstream stage anchors on.
pub fn run_brief(
    client: LlmClient,
    sys_instr: Arc<String>,
    prompt: String,
    cfg: WizardRunConfig,
) -> Result<String, String> {
    let user = format!(
        "You are planning an ISOMETRIC, DENSELY POPULATED 3D scene from this user prompt:\n\n\
         \"{}\"\n\n\
         Write a single design brief paragraph (60-120 words). Cover:\n\
         - the setting and mood\n\
         - lighting / time of day\n\
         - the kind of objects that should appear (broad strokes — the next stage will list them)\n\
         - any background / floor / wall hints\n\n\
         The camera is always isometric (30° pitch, 45° yaw) and the floor is the Y=0 plane.\n\
         Write the brief and nothing else — no headings, no markdown, no commentary.",
        prompt.replace('"', "'")
    );
    let user = apply_style_to_prompt(&user, cfg.style);
    let mut gc = GenerateConfig::new(user);
    gc.model = cfg.model.clone();
    gc.seed = Some(cfg.seed);
    gc.temperature = Some(cfg.temperature);
    gc.thinking_level = Some(cfg.thinking);
    gc.system_instruction = Some((*sys_instr).clone());
    gc.spend_context = mogen_llm::CallContext {
        operation: "Generate".into(),
        scene_path: None,
        session_id: if cfg.session_id.is_empty() {
            None
        } else {
            Some(cfg.session_id.clone())
        },
    };
    let resp = client.generate(&gc).map_err(|e| e.to_string())?;
    Ok(resp.text.trim().to_string())
}

/// Stage 2: parse the brief into a structured object manifest. The LLM
/// returns strict JSON; we hand-parse via `serde_json` so a stray markdown
/// fence (a model misbehaviour) gets a clean error rather than a panic.
pub fn run_manifest(
    client: LlmClient,
    sys_instr: Arc<String>,
    prompt: String,
    brief: String,
    cfg: WizardRunConfig,
) -> Result<Vec<ObjectEntry>, String> {
    let user = format!(
        "You are filling in the OBJECT MANIFEST for an isometric, densely-populated 3D scene.\n\n\
         Original user prompt: \"{}\"\n\n\
         Design brief:\n{}\n\n\
         Return STRICT JSON describing every object in the scene. The shape:\n\
         {{\n  \"objects\": [\n    {{\n      \"id\": \"snake_case_unique_id\",\n      \"name\": \"Human readable\",\n      \"role\": \"hero\" | \"filler\" | \"decor\",\n      \"prompt\": \"5-15 word focused prompt for this single object\",\n      \"size\": [width, height, depth] in metres,\n      \"position\": [x, y, z] in metres with Y up and floor at Y=0,\n      \"rotation_y_deg\": yaw rotation in degrees\n    }}, ...\n  ]\n}}\n\n\
         Rules:\n\
         - For a dense scene, target 10-18 objects across roles.\n\
         - Cluster props sensibly (a desk has a chair near it, a lamp on top, etc.).\n\
         - Keep `id` lowercase, snake_case, unique across the manifest.\n\
         - Position values: small scenes typically span -3..3 metres on X/Z.\n\
         - All values numeric — no strings inside arrays.\n\n\
         Reply with ONLY the JSON object — no markdown fences, no commentary, no leading prose.",
        prompt.replace('"', "'"),
        brief,
    );
    let user = apply_style_to_prompt(&user, cfg.style);
    let mut gc = GenerateConfig::new(user);
    gc.model = cfg.model.clone();
    gc.seed = Some(cfg.seed);
    gc.temperature = Some(cfg.temperature);
    gc.thinking_level = Some(cfg.thinking);
    gc.system_instruction = Some((*sys_instr).clone());
    gc.spend_context = mogen_llm::CallContext {
        operation: "Generate".into(),
        scene_path: None,
        session_id: if cfg.session_id.is_empty() {
            None
        } else {
            Some(cfg.session_id.clone())
        },
    };
    let resp = client.generate(&gc).map_err(|e| e.to_string())?;
    parse_manifest_json(&resp.text)
}

/// Stage 3: generate one reference image per object using the image client.
/// Writes the PNG to `<project>/wizard/references/<id>.png` and returns the
/// path. The wizard loops over objects, one per call, so a single failure
/// only loses one object's reference rather than the whole batch.
pub fn run_reference_image(
    image_client: ImageClient,
    obj: ObjectEntry,
    out_path: PathBuf,
    seed: u64,
) -> Result<PathBuf, String> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let prompt = format!(
        "A clean studio reference photo of {name} — {prompt}. \
         Single object on a plain neutral background, no text, no watermark, \
         no logos, no people. Reference-grade lighting, full object visible.",
        name = obj.name,
        prompt = obj.prompt
    );
    // Three attempts: image APIs occasionally return RECITATION (Gemini) or
    // transient 5xx. The textures pipeline does the same retry shape.
    let mut last_err: Option<String> = None;
    for attempt in 0..3u8 {
        let model = "";
        match image_client.generate_image(model, &prompt, Some(seed ^ attempt as u64)) {
            Ok(img) => {
                std::fs::write(&out_path, &img.png_bytes)
                    .map_err(|e| format!("write {}: {e}", out_path.display()))?;
                return Ok(out_path);
            }
            Err(e) => {
                let msg = format!("{e}");
                // Recitation: retry with a different seed bit; transient
                // server error: also retry. Any other error: bail.
                let retryable = matches!(e, ImageError::Gemini(ref g) if format!("{g}").contains("RECITATION"))
                    || msg.to_ascii_lowercase().contains("500")
                    || msg.to_ascii_lowercase().contains("503");
                last_err = Some(msg);
                if !retryable {
                    break;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "image generation failed".into()))
}

/// Stage 4: generate the per-object `.mog` module. The DSL is written via
/// `generate_with_repair` so validation errors auto-repair up to the user's
/// configured budget. If a reference image is present, it is attached as
/// vision input so the model has a visual target.
pub fn run_object_mog(
    client: LlmClient,
    sys_instr: Arc<String>,
    obj: ObjectEntry,
    out_path: PathBuf,
    reference_bytes: Option<Vec<u8>>,
    reference_mime: Option<String>,
    cfg: WizardRunConfig,
) -> Result<ObjectGenResult, String> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let header_prompt = format!("{} — {}", obj.name, obj.prompt);
    let user = format!(
        "Generate a SELF-CONTAINED mogen DSL module for ONE object:\n\n\
         - id (used as module name): \"{id}\"\n\
         - human name: \"{name}\"\n\
         - description: \"{description}\"\n\
         - rough size (W x H x D): {w:.2} x {h:.2} x {d:.2} metres\n\n\
         REQUIREMENTS:\n\
         - The output MUST be wrapped in `module \"{id}\" () {{ ... }}` so the assembly file can `use \"{id}\" ()`.\n\
         - Centre the geometry in its OWN local frame on the X/Z origin with the floor on Y=0.\n\
         - The whole module's bounds should fit within roughly the rough size above.\n\
         - Add 1-3 named `connector` declarations where another object might attach (e.g. on top, on the side).\n\
         - Materials: use 1-3 simple PBR materials; no `texture=` attributes (the wizard skins later).\n\
         - Do not emit a top-level `scene {{ }}` — the assembly does that.\n\
         - Do not emit a `meta(...)` block — the wizard stamps it after generation.\n\
         {image_hint}\n\n\
         Reply with ONLY the DSL — no commentary, no markdown fences.",
        id = obj.id,
        name = obj.name,
        description = obj.prompt,
        w = obj.size[0],
        h = obj.size[1],
        d = obj.size[2],
        image_hint = if reference_bytes.is_some() {
            "- A reference image is attached. Match its silhouette and proportions where possible."
        } else {
            ""
        },
    );
    let user = apply_style_to_prompt(&user, cfg.style);
    let mut gc = GenerateConfig::new(user);
    gc.model = cfg.model.clone();
    gc.seed = Some(cfg.seed);
    gc.temperature = Some(cfg.temperature);
    gc.thinking_level = Some(cfg.thinking);
    gc.system_instruction = Some((*sys_instr).clone());
    gc.spend_context = mogen_llm::CallContext {
        operation: "Generate".into(),
        scene_path: Some(out_path.display().to_string()),
        session_id: if cfg.session_id.is_empty() {
            None
        } else {
            Some(cfg.session_id.clone())
        },
    };
    if let (Some(data), Some(mime)) = (reference_bytes, reference_mime) {
        gc.user_images.push(mogen_llm::ImageInput { mime_type: mime, data });
    }
    let repair = RepairConfig {
        max_iters: cfg.max_repair_iters,
        on_iteration: None,
        allow_edit_mode: true,
    };
    let outcome = generate_with_repair(&client, gc, &repair).map_err(|e| e.to_string())?;
    let mut dsl = outcome.dsl;
    // Belt-and-braces: ensure the result actually defines a `module "<id>"`.
    // If the model forgot the wrapper, wrap the produced body ourselves so
    // the assembler's `use "<id>" ()` still resolves.
    if !dsl.contains(&format!("module \"{}\"", obj.id)) {
        dsl = format!("module \"{}\" () {{\n{}\n}}\n", obj.id, dsl.trim());
    }
    let wrapped = embed_seed_header(&dsl, cfg.seed, &header_prompt, Some(cfg.thinking));
    let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));
    std::fs::write(&out_path, wrapped.as_bytes())
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;
    let guide = compute_position_guide(&wrapped, &obj.id).unwrap_or(PositionGuide {
        anchor: [0.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        footprint_min: [-obj.size[0] * 0.5, 0.0, -obj.size[2] * 0.5],
        footprint_max: [obj.size[0] * 0.5, obj.size[1], obj.size[2] * 0.5],
        connectors: Vec::new(),
    });
    Ok(ObjectGenResult {
        mog_path: out_path,
        guide,
    })
}

/// Stage 6: ask the LLM whether a rendered object preview is a reasonable
/// take on its prompt. The verdict is a small JSON blob — `{pass, notes}`.
pub fn run_object_review(
    client: LlmClient,
    sys_instr: Arc<String>,
    obj: ObjectEntry,
    image_bytes: Vec<u8>,
    image_mime: String,
    cfg: WizardRunConfig,
) -> Result<ObjectReview, String> {
    let user = format!(
        "You are reviewing a 3D model rendered from this prompt:\n\n\
         \"{} — {}\"\n\n\
         The attached image is a clean isometric render of the generated model. \
         Judge whether it is a recognisable, well-formed take on the prompt. \
         A pass means a human would agree this is the requested object; a fail means \
         it is unrecognisable, broken, or wildly wrong-shaped.\n\n\
         Reply with ONLY a JSON object: {{ \"pass\": true | false, \"notes\": \"short rationale\" }}.",
        obj.name,
        obj.prompt,
    );
    let mut gc = GenerateConfig::new(user);
    gc.model = cfg.model.clone();
    gc.seed = Some(cfg.seed);
    gc.temperature = Some(cfg.temperature);
    gc.thinking_level = Some(cfg.thinking);
    gc.system_instruction = Some((*sys_instr).clone());
    gc.user_images.push(mogen_llm::ImageInput {
        mime_type: image_mime,
        data: image_bytes,
    });
    gc.spend_context = mogen_llm::CallContext {
        operation: "Generate".into(),
        scene_path: None,
        session_id: if cfg.session_id.is_empty() {
            None
        } else {
            Some(cfg.session_id.clone())
        },
    };
    let resp = client.generate(&gc).map_err(|e| e.to_string())?;
    parse_object_review_json(&resp.text)
}

/// Stage 7: ask the LLM to inspect a full-scene screenshot and emit
/// position-correction proposals. Empty `corrections` list = scene looks
/// fine. The full manifest is included as context so the model knows what
/// each object id corresponds to.
pub fn run_scene_review(
    client: LlmClient,
    sys_instr: Arc<String>,
    prompt: String,
    manifest: Vec<ObjectEntry>,
    image_bytes: Vec<u8>,
    image_mime: String,
    iteration: u32,
    cfg: WizardRunConfig,
) -> Result<SceneReview, String> {
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("serialise manifest: {e}"))?;
    let user = format!(
        "You are reviewing an isometric render of a 3D scene generated for this prompt:\n\n\
         \"{prompt}\"\n\n\
         Here is the object manifest the wizard placed (id → position):\n\
         {manifest}\n\n\
         Look for OBVIOUS positional problems only:\n\
         - objects floating above the floor (Y way above 0)\n\
         - objects clipping badly through each other\n\
         - objects behind / occluded by larger objects that should be in front\n\
         - objects far from where they should be given the prompt's spatial language\n\n\
         Reply with ONLY a JSON object:\n\
         {{\n  \"notes\": \"short overall remark\",\n  \"corrections\": [\n    {{ \"object_id\": \"...\", \"new_position\": [x, y, z], \"new_rotation_y_deg\": null, \"rationale\": \"...\" }}\n  ]\n}}\n\
         Omit objects that look correct. Either field on a correction may be null if only one needs changing. \
         Cap the list at 6 corrections per round; pick the worst first.",
        prompt = prompt.replace('"', "'"),
        manifest = manifest_json
    );
    let mut gc = GenerateConfig::new(user);
    gc.model = cfg.model.clone();
    gc.seed = Some(cfg.seed);
    gc.temperature = Some(cfg.temperature);
    gc.thinking_level = Some(cfg.thinking);
    gc.system_instruction = Some((*sys_instr).clone());
    gc.user_images.push(mogen_llm::ImageInput {
        mime_type: image_mime,
        data: image_bytes,
    });
    gc.spend_context = mogen_llm::CallContext {
        operation: "Generate".into(),
        scene_path: None,
        session_id: if cfg.session_id.is_empty() {
            None
        } else {
            Some(cfg.session_id.clone())
        },
    };
    let resp = client.generate(&gc).map_err(|e| e.to_string())?;
    let mut review = parse_scene_review_json(&resp.text)?;
    review.iteration = iteration;
    Ok(review)
}

/// Lower the just-written per-object DSL and compute a position guide
/// (anchor on the floor centre, footprint AABB from the lowered geometry,
/// connector names if any). Best-effort — the wizard falls back to the
/// rough manifest size if lowering fails.
fn compute_position_guide(dsl: &str, _id: &str) -> Option<PositionGuide> {
    let nodes = mogen_dsl::parse(dsl).ok()?;
    let scene = mogen_dsl::lower(&nodes).ok()?;
    // Walk every root subtree to get the local-frame extent. The per-object
    // module is centred on its own origin so "local" is also what the
    // assembler instances at the manifest position.
    let mut accum = mogen_core::Aabb::empty();
    for root in &scene.roots {
        if let Some(aabb) = subtree_local_aabb(&scene, *root) {
            let root_node = &scene.nodes[root.0 as usize];
            accum.merge(aabb.transformed(root_node.transform.to_mat4()));
        }
    }
    if accum.is_empty() {
        return None;
    }
    let connectors: Vec<String> = scene
        .nodes
        .iter()
        .flat_map(|n| n.connectors.iter().map(|c| c.tag.clone()))
        .collect();
    Some(PositionGuide {
        anchor: [
            (accum.min.x + accum.max.x) * 0.5,
            accum.min.y,
            (accum.min.z + accum.max.z) * 0.5,
        ],
        up: [0.0, 1.0, 0.0],
        footprint_min: [accum.min.x, accum.min.y, accum.min.z],
        footprint_max: [accum.max.x, accum.max.y, accum.max.z],
        connectors,
    })
}

/// Parse the manifest JSON, tolerating a leading/trailing markdown fence
/// (models sometimes wrap their answer in ```json … ``` even when told not to).
fn parse_manifest_json(text: &str) -> Result<Vec<ObjectEntry>, String> {
    let cleaned = strip_markdown_fences(text);
    let value: serde_json::Value = serde_json::from_str(&cleaned)
        .map_err(|e| format!("manifest is not valid JSON: {e}\n\n--- raw response ---\n{cleaned}"))?;
    let arr = value
        .get("objects")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "manifest JSON missing top-level `objects` array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let parsed: ObjectEntry = serde_json::from_value(entry.clone()).map_err(|e| {
            format!(
                "manifest entry #{i} couldn't be parsed: {e}\n\n--- entry ---\n{entry}",
                entry = entry
            )
        })?;
        out.push(parsed);
    }
    if out.is_empty() {
        return Err("manifest is empty — no objects to generate".into());
    }
    // Enforce unique ids; if duplicates slipped through, suffix them.
    let mut seen = std::collections::HashSet::new();
    for o in &mut out {
        let base = o.id.clone();
        let mut tag = 1usize;
        while !seen.insert(o.id.clone()) {
            o.id = format!("{base}_{tag}");
            tag += 1;
        }
    }
    Ok(out)
}

fn parse_object_review_json(text: &str) -> Result<ObjectReview, String> {
    let cleaned = strip_markdown_fences(text);
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        format!("object review is not valid JSON: {e}\n\n--- raw response ---\n{cleaned}")
    })?;
    let pass = v.get("pass").and_then(|v| v.as_bool()).unwrap_or(false);
    let notes = v
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(ObjectReview { pass, notes })
}

fn parse_scene_review_json(text: &str) -> Result<SceneReview, String> {
    let cleaned = strip_markdown_fences(text);
    let v: serde_json::Value = serde_json::from_str(&cleaned).map_err(|e| {
        format!("scene review is not valid JSON: {e}\n\n--- raw response ---\n{cleaned}")
    })?;
    let notes = v
        .get("notes")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut corrections = Vec::new();
    if let Some(arr) = v.get("corrections").and_then(|v| v.as_array()) {
        for c in arr {
            let object_id = c
                .get("object_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if object_id.is_empty() {
                continue;
            }
            let new_position = c
                .get("new_position")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    if arr.len() == 3 {
                        Some([
                            arr[0].as_f64().unwrap_or(0.0) as f32,
                            arr[1].as_f64().unwrap_or(0.0) as f32,
                            arr[2].as_f64().unwrap_or(0.0) as f32,
                        ])
                    } else {
                        None
                    }
                });
            let new_rotation_y_deg = c
                .get("new_rotation_y_deg")
                .and_then(|v| v.as_f64())
                .map(|d| d as f32);
            let rationale = c
                .get("rationale")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if new_position.is_none() && new_rotation_y_deg.is_none() {
                continue;
            }
            corrections.push(PositionCorrection {
                object_id,
                new_position,
                new_rotation_y_deg,
                rationale,
            });
        }
    }
    Ok(SceneReview {
        notes,
        corrections,
        iteration: 0,
    })
}

/// Strip leading/trailing ```...``` fences and any "```json" hint. Idempotent.
pub(crate) fn strip_markdown_fences(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(stripped) = trimmed.strip_prefix("```") {
        let stripped = stripped.trim_start_matches("json").trim_start_matches('\n');
        if let Some(body) = stripped.strip_suffix("```") {
            return body.trim().to_string();
        }
        // Closing fence might be on its own line.
        if let Some(end) = stripped.rfind("```") {
            return stripped[..end].trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Walk an AST and return whether any node is a `module` with the given name.
/// Used in tests + the wizard's lowering sanity check.
#[allow(dead_code)]
pub(crate) fn ast_has_module(nodes: &[Node], name: &str) -> bool {
    nodes
        .iter()
        .any(|n| n.kind == "module" && n.name.as_deref() == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_json_fence() {
        let s = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_markdown_fences(s), "{\"a\": 1}");
    }

    #[test]
    fn strips_bare_fence() {
        let s = "```\n{\"a\": 1}\n```";
        assert_eq!(strip_markdown_fences(s), "{\"a\": 1}");
    }

    #[test]
    fn parses_minimal_manifest() {
        let s = r#"{
            "objects": [
                { "id": "chair", "name": "Chair", "role": "hero", "prompt": "a chair", "size": [1,1,1], "position": [0,0,0], "rotation_y_deg": 0 }
            ]
        }"#;
        let parsed = parse_manifest_json(s).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "chair");
    }

    #[test]
    fn manifest_dedupes_duplicate_ids() {
        let s = r#"{
            "objects": [
                { "id": "x", "name": "x", "role": "hero", "prompt": "p", "size": [1,1,1], "position": [0,0,0], "rotation_y_deg": 0 },
                { "id": "x", "name": "x", "role": "filler", "prompt": "p", "size": [1,1,1], "position": [1,0,0], "rotation_y_deg": 0 }
            ]
        }"#;
        let parsed = parse_manifest_json(s).unwrap();
        assert_eq!(parsed[0].id, "x");
        assert_eq!(parsed[1].id, "x_1");
    }

    #[test]
    fn manifest_empty_is_error() {
        let s = r#"{ "objects": [] }"#;
        assert!(parse_manifest_json(s).is_err());
    }

    #[test]
    fn parses_object_review() {
        let s = r#"{ "pass": true, "notes": "looks like a chair" }"#;
        let r = parse_object_review_json(s).unwrap();
        assert!(r.pass);
    }

    #[test]
    fn parses_scene_review_with_corrections() {
        let s = r#"{
            "notes": "lamp is floating",
            "corrections": [
                { "object_id": "lamp", "new_position": [0, 0, 1], "new_rotation_y_deg": null, "rationale": "drop to floor" }
            ]
        }"#;
        let r = parse_scene_review_json(s).unwrap();
        assert_eq!(r.corrections.len(), 1);
        assert_eq!(r.corrections[0].object_id, "lamp");
        assert_eq!(r.corrections[0].new_position, Some([0.0, 0.0, 1.0]));
    }

    #[test]
    fn parses_scene_review_with_rotation_only() {
        let s = r#"{
            "notes": "",
            "corrections": [
                { "object_id": "rug", "new_position": null, "new_rotation_y_deg": 90, "rationale": "" }
            ]
        }"#;
        let r = parse_scene_review_json(s).unwrap();
        assert_eq!(r.corrections.len(), 1);
        assert_eq!(r.corrections[0].new_rotation_y_deg, Some(90.0));
    }

    #[test]
    fn skips_correction_with_both_fields_null() {
        let s = r#"{
            "corrections": [
                { "object_id": "x", "new_position": null, "new_rotation_y_deg": null, "rationale": "" }
            ]
        }"#;
        let r = parse_scene_review_json(s).unwrap();
        assert!(r.corrections.is_empty());
    }
}

/// Provider/credential plumbing so the wizard can build an `LlmClient` from
/// the same Settings + Credential the rest of the app uses. Kept here (not in
/// `ui_wizard.rs`) so unit tests in this module can exercise it.
pub fn build_text_client(
    provider: mogen_llm::Provider,
    credential: Credential,
    claude_code_path: &str,
    zai_base_url: &str,
) -> LlmClient {
    crate::app::util::build_provider_client(provider, credential, claude_code_path, zai_base_url)
}
