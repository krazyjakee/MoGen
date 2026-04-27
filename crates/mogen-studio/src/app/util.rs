use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use mogen_core::SceneGraph;
use mogen_export::ExportOptions;
use mogen_llm::gemini::GeminiClient;
use mogen_llm::textures::{
    build_plan, default_textures_dir, run_plan, splice_textures, PlanAction,
    TextureProgress, TexturesArgs,
};
use mogen_llm::{
    embed_seed_header, generate_with_repair, parse_prompt_header, parse_seed_header,
    repair_message, validate_text, GenerateConfig, ImageInput, LlmClient, Provider, RepairConfig,
    ThinkingLevel, Usage, DEFAULT_IMAGE_MODEL,
};

use crate::pipeline::write_glb_with_source_and_options;

use super::error_class::classify;
use super::types::{
    BuildOutcome, EnhanceTarget, LlmKind, LlmMessage, LlmOutcome, LlmProgress, TextureUiConfig,
};

/// Walk upward from the CWD until we find the workspace root (the dir that
/// contains a `Cargo.toml`). Falls back to CWD when unfound. Used as the
/// default directory for the Open / Save-As file pickers so first-launch
/// dialogs start somewhere sensible.
pub(super) fn locate_project_root() -> PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cur = start.as_path();
    loop {
        if cur.join("Cargo.toml").is_file() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return start,
        }
    }
}

/// `(material_name, slot_name, authored_path)` triples for every populated
/// texture slot on every material in the scene. Ordered deterministically
/// (material order × slot order) so the UI doesn't jitter between rebuilds.
pub(super) fn gather_texture_refs(scene: &mogen_core::SceneGraph) -> Vec<(String, &'static str, PathBuf)> {
    const SLOTS: [&str; 5] = [
        "base_color",
        "metallic_roughness",
        "normal",
        "occlusion",
        "emissive",
    ];
    let mut out = Vec::new();
    for m in &scene.materials {
        let refs = [
            &m.base_color_texture,
            &m.metallic_roughness_texture,
            &m.normal_texture,
            &m.occlusion_texture,
            &m.emissive_texture,
        ];
        for (slot, r) in SLOTS.iter().zip(refs.iter()) {
            if let Some(t) = r {
                out.push((m.name.clone(), *slot, t.path.clone()));
            }
        }
    }
    out
}

pub(super) fn resolve_for_check(path: &Path, base: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base {
        Some(b) => b.join(path),
        None => path.to_path_buf(),
    }
}

/// PBR map suffixes the textures pipeline produces alongside each albedo.
/// Kept in sync with `mogen_llm::textures::SlotKind::file_suffix` — listed
/// in the order of a full regenerate so deletions run companion-first.
pub(super) const TEXTURE_COMPANION_SUFFIXES: [&str; 4] = [
    "_albedo.png",
    "_normal.png",
    "_metallicRoughness.png",
    "_ao.png",
];

/// List PNG files in `dir` whose absolute path isn't in `referenced`. Only
/// top-level `*.png` entries are considered — subdirectories aren't walked
/// because the textures pipeline never writes into them. Returns a sorted
/// list so the UI order is stable across repaints.
pub(super) fn scan_unused_textures(
    dir: &Path,
    referenced: &std::collections::HashSet<PathBuf>,
) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("png"))
                    .unwrap_or(false)
        })
        .filter(|p| !referenced.contains(p))
        .collect();
    out.sort();
    out
}

/// Stem a textures-pipeline PNG path down to its material stem by stripping
/// any of the known companion suffixes. `None` when the file doesn't match
/// the pipeline's naming convention, in which case we delete only it.
pub(super) fn texture_material_stem(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    for suffix in TEXTURE_COMPANION_SUFFIXES {
        if let Some(stem) = name.strip_suffix(suffix) {
            return Some(stem.to_string());
        }
    }
    None
}

/// Delete `path` and — when the filename matches the textures pipeline's
/// `_albedo.png` / `_normal.png` / `_metallicRoughness.png` / `_ao.png`
/// convention — every companion PBR map that shares its material stem.
/// Returns a human-readable status string for the footer. Missing files are
/// silently skipped; unlink failures are collected into the message.
pub(super) fn delete_texture_group(path: &Path) -> String {
    let dir = match path.parent() {
        Some(p) => p,
        None => return format!("delete: {} has no parent dir", path.display()),
    };
    let mut targets: Vec<PathBuf> = Vec::new();
    if let Some(stem) = texture_material_stem(path) {
        for suffix in TEXTURE_COMPANION_SUFFIXES {
            let candidate = dir.join(format!("{stem}{suffix}"));
            if candidate.is_file() {
                targets.push(candidate);
            }
        }
    }
    if targets.is_empty() {
        targets.push(path.to_path_buf());
    }

    let mut deleted: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for t in &targets {
        match fs::remove_file(t) {
            Ok(()) => {
                if let Some(name) = t.file_name().and_then(|n| n.to_str()) {
                    deleted.push(name.to_string());
                }
            }
            Err(e) => {
                errors.push(format!("{}: {e}", t.display()));
            }
        }
    }

    if errors.is_empty() {
        match deleted.len() {
            0 => format!("textures: nothing to delete at {}", path.display()),
            1 => format!("textures: deleted {}", deleted[0]),
            n => format!("textures: deleted {n} files ({})", deleted.join(", ")),
        }
    } else if deleted.is_empty() {
        format!("textures: delete failed: {}", errors.join("; "))
    } else {
        format!(
            "textures: deleted {} but failed: {}",
            deleted.join(", "),
            errors.join("; "),
        )
    }
}

/// Slot attribute names that get cleared when the user deletes a material's
/// textures from the right-click menu. Kept aligned with the slots reported
/// by [`gather_texture_refs`] so the on-disk sweep and the source rewrite
/// agree on what counts as "the textures" for a material.
const MATERIAL_TEXTURE_ATTRS: [&str; 5] = [
    "base_color_texture",
    "metallic_roughness_texture",
    "normal_texture",
    "occlusion_texture",
    "emissive_texture",
];

/// Delete every PNG belonging to `material`'s slots and strip the
/// corresponding `*_texture` attrs from the source. Returns the rewritten
/// source plus a footer-status string. `refs` is the result of
/// [`gather_texture_refs`] for the current scene; only refs whose material
/// matches are touched. The source is left untouched when no attrs are
/// present (e.g. material lives inside an imported module so its span isn't
/// in this file).
pub(super) fn delete_material_textures(
    source: &str,
    source_dir: Option<&Path>,
    material: &str,
    refs: &[(String, &'static str, PathBuf)],
) -> (String, String) {
    // File sweep — `delete_texture_group` finds the material stem from any
    // one ref and unlinks every companion in its `_albedo/_normal/...`
    // family. Deduplicate by stem so we don't double-report.
    let mut file_status = String::new();
    let mut seen_stems: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (m, _, rel) in refs.iter().filter(|(m, _, _)| m == material) {
        let abs = resolve_for_check(rel, source_dir);
        let stem_key = texture_material_stem(&abs)
            .unwrap_or_else(|| abs.to_string_lossy().into_owned());
        if !seen_stems.insert(stem_key) {
            continue;
        }
        let _ = m;
        if !file_status.is_empty() {
            file_status.push_str("; ");
        }
        file_status.push_str(&delete_texture_group(&abs));
    }

    // Source rewrite — strip every `*_texture` attr from this material. The
    // span shifts after each delete, so re-resolve between iterations.
    let mut new_source = source.to_string();
    let mut stripped: u32 = 0;
    for attr in MATERIAL_TEXTURE_ATTRS {
        let Some(span) = find_material_source_span(&new_source, material) else {
            break;
        };
        let after = crate::edit::delete_attr(&new_source, span, attr);
        if after != new_source {
            new_source = after;
            stripped += 1;
        }
    }

    let cleared_msg = if stripped > 0 {
        format!(
            "; cleared {stripped} attr{} on \"{material}\"",
            if stripped == 1 { "" } else { "s" },
        )
    } else {
        String::new()
    };
    let status = if file_status.is_empty() && stripped == 0 {
        format!("textures: nothing to remove for \"{material}\"")
    } else if file_status.is_empty() {
        format!("textures: cleared {stripped} attr(s) on \"{material}\"")
    } else {
        format!("{file_status}{cleared_msg}")
    };
    (new_source, status)
}

/// Show "…/dir/filename.png", keeping the filename intact and ellipsizing
/// the directory prefix from the left if the whole thing is too long.
pub(super) fn ellipsize_path(path: &Path, max_chars: usize) -> String {
    let s = path.to_string_lossy();
    let n = s.chars().count();
    if n <= max_chars {
        return s.into_owned();
    }
    // Always keep the filename intact — that's the part the user actually
    // recognizes. Trim the prefix and prepend an ellipsis.
    let file_chars = path
        .file_name()
        .map(|f| f.to_string_lossy().chars().count())
        .unwrap_or(0);
    if file_chars + 1 >= max_chars {
        // Filename alone is too long; keep its tail.
        let tail: String = s.chars().rev().take(max_chars.saturating_sub(1)).collect();
        let tail: String = tail.chars().rev().collect();
        return format!("…{tail}");
    }
    let keep = max_chars.saturating_sub(file_chars + 1); // 1 for ellipsis
    let prefix_chars = n.saturating_sub(file_chars);
    let drop = prefix_chars.saturating_sub(keep);
    let visible: String = s.chars().skip(drop).collect();
    format!("…{visible}")
}

/// Trim a float to four decimals and drop trailing zeros so inspector-
/// committed values splice cleanly into the DSL. Matches
/// `viewer::format_scalar` — duplicated here because app doesn't import the
/// viewer internals.
pub(super) fn format_inspector_scalar(v: f32) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Locate the DSL source span for the authored `material "name" (...)`
/// declaration. Materials can live at the top level or inside `scene { … }` —
/// both are checked. Returns `None` if the source no longer parses or the
/// material wasn't authored in the active file (e.g. it came from a module).
pub(super) fn find_material_source_span(src: &str, name: &str) -> Option<mogen_core::Span> {
    let ast = mogen_dsl::parse(src).ok()?;
    for n in &ast {
        if n.kind == "material" && n.name.as_deref() == Some(name) {
            return Some(n.span);
        }
        if n.kind == "scene" {
            for c in &n.children {
                if c.kind == "material" && c.name.as_deref() == Some(name) {
                    return Some(c.span);
                }
            }
        }
    }
    None
}

/// Locate the DSL source span for the `clip` (or procedural-template) node
/// whose resulting clip has `clip_name`. Scans the parsed AST recursively so
/// scene-nested clips are found alongside top-level ones. Returns `None` if
/// the source no longer parses or no matching authored node exists (e.g. for
/// multi-target templates whose clip names carry an `_{i}` suffix).
pub(super) fn find_clip_source_span(src: &str, clip_name: &str) -> Option<mogen_core::Span> {
    let ast = mogen_dsl::parse(src).ok()?;
    // Kinds that lower to `Clip` entries. Procedural templates take their
    // name from `node.name`; literal clips likewise. Multi-target templates
    // produce `{name}_{i}` clips — those won't match a bare name here and
    // the Delete button is disabled upstream.
    const ANIM_KINDS: &[&str] = &["clip", "spin", "open_close", "wave", "flap", "idle"];
    fn walk(
        node: &mogen_dsl::ast::Node,
        target: &str,
        kinds: &[&str],
    ) -> Option<mogen_core::Span> {
        if kinds.contains(&node.kind.as_str())
            && node.name.as_deref() == Some(target)
        {
            return Some(node.span);
        }
        for c in &node.children {
            if let Some(s) = walk(c, target, kinds) {
                return Some(s);
            }
        }
        None
    }
    for n in &ast {
        if let Some(s) = walk(n, clip_name, ANIM_KINDS) {
            return Some(s);
        }
    }
    None
}

pub(super) fn offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

pub(super) fn run_build(
    scene: SceneGraph,
    out: PathBuf,
    source_dir: Option<PathBuf>,
    opts: ExportOptions,
    stage: Arc<Mutex<String>>,
    file_index: usize,
) -> BuildOutcome {
    // Keep a copy of the *effective* scene (after merge, if enabled) so we
    // can pass it back to the UI for a viewer refresh. The merge transform
    // is the expensive stage, so rather than run it twice we compute once
    // and hand the result to the exporter ourselves.
    let effective_scene: SceneGraph = if opts.merge_sibling_meshes {
        {
            let mut s = stage.lock().unwrap();
            *s = "merging sibling meshes".into();
        }
        mogen_export::merge::merge_sibling_meshes(&scene, |_| {})
    } else {
        scene
    };

    // With `merge_sibling_meshes` already applied, the exporter below only
    // needs to run the non-merge passes. Construct a new opts that leaves
    // merge off so the exporter doesn't redo the work.
    let post_merge_opts = ExportOptions {
        merge_sibling_meshes: false,
        ..opts
    };

    let stage_for_progress = Arc::clone(&stage);
    let progress = move |label: &str| {
        if let Ok(mut s) = stage_for_progress.lock() {
            *s = label.to_string();
        }
    };

    let write_result = write_glb_with_source_and_options(
        &effective_scene,
        &out,
        source_dir.as_deref(),
        &post_merge_opts,
        progress,
    );

    match write_result {
        Ok(()) => {
            let bytes = fs::metadata(&out).map(|m| m.len()).ok();
            BuildOutcome {
                file_index,
                path: out,
                exported_scene: opts
                    .merge_sibling_meshes
                    .then_some(effective_scene),
                bytes,
                error: None,
            }
        }
        Err(e) => BuildOutcome {
            file_index,
            path: out,
            exported_scene: None,
            bytes: None,
            error: Some(format!("{e:#}")),
        },
    }
}

/// Tuning knobs for one LLM run. Gathered into a struct rather than a long
/// parameter list because `run_llm` already takes seven positional args and
/// every new setting would push another through every call site.
#[derive(Clone)]
pub(super) struct LlmRunConfig {
    pub model: String,
    pub thinking: ThinkingLevel,
    pub temperature: f32,
    pub max_repair_iters: u32,
    /// `None` → pick from the DSL header if present, else random; `Some(v)` →
    /// use exactly that seed (so the user can reproduce a prior generation).
    pub seed_override: Option<u64>,
    /// Path to the `claude` binary. Honoured only when the active provider is
    /// [`Provider::ClaudeCode`] (other providers ignore it). Empty/blank is a
    /// valid value — the underlying client falls back to `claude` on `PATH`.
    pub claude_code_path: String,
}

/// Construct an [`LlmClient`] honoring Studio-only settings that don't fit
/// the bare `LlmClient::new(provider, api_key)` signature. Today that's just
/// the Claude Code binary path.
pub(super) fn build_provider_client(
    provider: Provider,
    api_key: String,
    claude_code_path: &str,
) -> LlmClient {
    if matches!(provider, Provider::ClaudeCode) {
        LlmClient::with_base_url(provider, api_key, claude_code_path)
    } else {
        LlmClient::new(provider, api_key)
    }
}

pub(super) fn run_llm(
    kind: LlmKind,
    prompt: String,
    existing: Option<String>,
    provider: Provider,
    image: Option<ImageInput>,
    api_key: String,
    run_cfg: LlmRunConfig,
    sys_instr: Arc<String>,
    tx: Sender<LlmMessage>,
) -> LlmOutcome {
    let send_progress = |p: LlmProgress| {
        // If the receiver is gone (user cancelled / closed the tab) just drop
        // the message — worker keeps running so the HTTP client can finish,
        // but we're no longer obliged to report progress.
        let _ = tx.send(LlmMessage::Progress(p));
    };

    let client = build_provider_client(provider, api_key, &run_cfg.claude_code_path);
    let seed = run_cfg.seed_override.unwrap_or_else(|| {
        existing
            .as_deref()
            .and_then(parse_seed_header)
            .unwrap_or_else(pick_default_seed)
    });

    // For edit-an-existing-file kinds, keep the original `// prompt: …` header
    // so the provenance line isn't overwritten with the modify/animate text.
    let header_prompt = match kind {
        LlmKind::Generate | LlmKind::Textures => {
            // When an image was attached, annotate the seed header so the
            // `// prompt:` line records *why* the file looks the way it does
            // (otherwise an image-only generate writes an empty `prompt:` line,
            // which is misleading).
            if image.is_some() {
                let trimmed = prompt.trim();
                if trimmed.is_empty() {
                    "[image attached]".to_string()
                } else {
                    format!("[image attached] {trimmed}")
                }
            } else {
                prompt.clone()
            }
        }
        LlmKind::Modify | LlmKind::Animate | LlmKind::Repair => existing
            .as_deref()
            .and_then(parse_prompt_header)
            .unwrap_or_else(|| prompt.clone()),
    };

    let user_prompt = match kind {
        LlmKind::Generate => {
            // When the only input is an image, a non-empty text part still
            // helps the model commit to the DSL output mode (the system
            // instruction handles the schema, but Gemini's vision path
            // sometimes regresses to describing the image otherwise).
            // Concatenate the user's text with a short directive when an
            // image is attached; pass the prompt through unchanged when
            // there's no image so the legacy flow stays bit-for-bit.
            if image.is_some() {
                let trimmed = prompt.trim();
                if trimmed.is_empty() {
                    "Generate a mogen DSL scene that recreates the attached \
                     reference image as a 3D model."
                        .to_string()
                } else {
                    format!(
                        "Generate a mogen DSL scene that recreates the attached \
                         reference image as a 3D model. Additional guidance from \
                         the user:\n\n{trimmed}",
                    )
                }
            } else {
                prompt.clone()
            }
        }
        LlmKind::Modify => format!(
            "You are editing an existing mogen DSL file. Apply this modification:\n\n\
            {mod_prompt}\n\n\
            Make the smallest edit that satisfies the request. Do not rename, reorder, \
            reformat, or restyle parts the modification does not touch.\n\n\
            Reply with ONLY the full modified DSL — no commentary, no markdown fences. \
            Do not include the `// mogen-generate` header comments; the caller re-adds them.\n\n\
            Existing file:\n\n{existing}",
            mod_prompt = prompt.trim(),
            existing = existing.as_deref().unwrap_or("").trim_end(),
        ),
        LlmKind::Animate => format!(
            "You are editing an existing mogen DSL file. APPEND new animation and rigging \
            declarations to satisfy this request:\n\n\
            {anim_prompt}\n\n\
            mogen supports two rigging strategies. Pick the SIMPLER one that fits the request:\n\n\
            A) Node-transform animation (for articulations that can be expressed as rigid \
            transforms of existing scene nodes — door hinges, wheels, rotors, pistons, \
            breathing). Place these at the top level of the file (outside `scene {{ … }}`):\n\
              • `joint \"name\" (type=hinge|slider|ball|rotor, axis=[x,y,z], pivot=\"node\", limits=[lo,hi])`\n\
              • `clip \"name\" (seconds=N) {{ track \"joint_or_node\" (from=0, to=V, prop=\"rotation\"|\"translation\"|\"scale\") }}`\n\
              • procedural templates (one-liners): `spin`, `open_close`, `wave`, `flap`, `idle`\n\
                e.g. `spin \"rotor_spin\" (target=\"rotor\", axis=[0,0,1], rpm=30)`\n\
                     `open_close \"door_swing\" (target=\"door_hinge\", angle=90, seconds=1.2)`\n\
            When a template targets a scene node directly (not a joint), it MUST pass an \
            explicit `axis` (except `idle`, which is a scale breathe with no axis).\n\n\
            B) Skeletal skinning (for meshes that must deform smoothly — limbs bending, \
            tails whipping, any continuous body). Declare a `skeleton` INSIDE `scene {{ … }}` \
            and bind a primitive to it by adding `skin=\"skel_name\"` to its attrs:\n\
              • `skeleton \"skel_name\" {{ bone \"b1\" (pos=[x,y,z], envelope=R) {{ bone \"b2\" (pos=[…], envelope=R) {{ … }} }} }}`\n\
                — bones nest to form the chain; `pos` is RELATIVE to the parent bone; `envelope` \
                is the radius (in world units) within which vertices get weight from this bone.\n\
              • Any primitive in the same scene can bind to it by adding `skin=\"skel_name\"` \
                to its attribute list (e.g. `cylinder \"arm\" (…, skin=\"skel_name\")`). \
                Weights are assigned automatically by nearest-bone envelope falloff.\n\
              • Drive the deformation by rotating the bone scene nodes via a `clip` with \
                `track \"bone_name\" (prop=rotation, from=0, to=…)`. `from`/`to` are in \
                degrees when `prop=rotation`.\n\
            Minimal skinned example:\n\
              ```\n\
              scene {{\n\
                skeleton \"arm_skel\" {{\n\
                  bone \"shoulder\" (pos=[0,0,0], envelope=0.75) {{\n\
                    bone \"elbow\" (pos=[0,0.5,0], envelope=0.75)\n\
                  }}\n\
                }}\n\
                cylinder \"arm_mesh\" (pos=[0,0.5,0], radius=0.12, height=1.0, skin=\"arm_skel\")\n\
              }}\n\
              clip \"swing\" (seconds=1.0) {{ track \"elbow\" (prop=rotation, from=0, to=60) }}\n\
              ```\n\n\
            RULES:\n\
            - Prefer (A) for any rig the user describes in terms of hinges/sliders/spins. \
              Only reach for (B) when the request implies smooth continuous deformation of a \
              single mesh.\n\
            - Do not touch geometry. Preserve every `scene`, `material`, `mesh`, `primitive`, \
              `group`, `array`, `mirror`, `attach`, `connector`, `socket`, `plug`, `use`, and \
              `module` exactly as written — except you MAY add a single `skin=\"…\"` attribute \
              to the one primitive that a new (B)-style rig deforms.\n\
            - Preserve every existing `joint`, `clip`, `skeleton`, `spin`, `open_close`, \
              `wave`, `flap`, and `idle` declaration exactly as written. ADD new ones \
              alongside them; do not rewrite, rename, merge, or delete existing animation \
              or rigging. Only modify an existing declaration if the user's request \
              explicitly names it and asks to change it.\n\
            - Every animation `target=`, `joint pivot=`, and `track` name must reference a \
              node that already exists in the scene (bones become scene nodes once the \
              `skeleton` block is added). Do not invent or rename other nodes.\n\
            - New `joint`, `clip`, `skeleton`, and template names must not collide with \
              existing ones — pick a fresh unique name.\n\n\
            Reply with ONLY the full updated DSL — no commentary, no markdown fences. Do \
            not include the `// mogen-generate` header comments; the caller re-adds them.\n\n\
            Existing file:\n\n{existing}",
            anim_prompt = prompt.trim(),
            existing = existing.as_deref().unwrap_or("").trim_end(),
        ),
        LlmKind::Repair => {
            // The validator already ran in `start_llm_repair` before we got
            // here, but we re-run it on the worker thread to get the exact
            // diagnostics + spans. `repair_message` folds the previous DSL,
            // every diagnostic (with caret excerpts), and each code's fix
            // hint into the prompt — the same shape the repair loop uses on
            // subsequent iterations.
            let existing_src = existing.as_deref().unwrap_or("");
            let diags = validate_text(existing_src);
            repair_message(&header_prompt, existing_src, &diags, &[])
        }
        LlmKind::Textures => unreachable!("run_llm is text-only; textures uses run_llm_textures"),
    };

    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = run_cfg.model.clone();
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(run_cfg.thinking);
    cfg.temperature = Some(run_cfg.temperature);
    cfg.system_instruction = Some((*sys_instr).clone());
    if let Some(img) = image {
        // Carried through every repair iteration: `repair.rs` rewrites
        // `cfg.user_prompt` but leaves `cfg.user_images` alone, so the model
        // keeps the visual reference while it fixes validator errors.
        cfg.user_images.push(img);
    }

    send_progress(LlmProgress::Status(format!(
        "calling {} ({}) — thinking={:?}",
        provider.label(),
        kind.label(),
        run_cfg.thinking,
    )));

    let max_iters = run_cfg.max_repair_iters;
    let tx_for_repair = tx.clone();
    let repair = RepairConfig {
        max_iters,
        on_iteration: Some(Box::new(move |iter, diags| {
            let errors = diags
                .iter()
                .filter(|d| matches!(d.severity, mogen_core::Severity::Error))
                .count();
            let _ = tx_for_repair.send(LlmMessage::Progress(LlmProgress::Repair {
                iter,
                max: max_iters,
                errors,
            }));
        })),
    };

    match generate_with_repair(&client, cfg, &repair) {
        Ok(outcome) => {
            send_progress(LlmProgress::Status(format!(
                "done — {} call(s), {} tokens",
                outcome.call_count, outcome.usage.total_tokens
            )));
            let wrapped = embed_seed_header(
                &outcome.dsl,
                seed,
                &header_prompt,
                Some(run_cfg.thinking),
            );
            LlmOutcome {
                dsl: wrapped,
                diagnostics: outcome.diagnostics,
                usage: outcome.usage,
                calls: outcome.call_count,
                model: run_cfg.model,
                image_calls: 0,
                retry_prompt: Some(prompt),
                error: None,
                kind,
            }
        }
        Err(e) => {
            let info = classify(&e);
            LlmOutcome {
                dsl: existing.unwrap_or_default(),
                diagnostics: Vec::new(),
                usage: Usage::default(),
                calls: 0,
                model: run_cfg.model,
                image_calls: 0,
                retry_prompt: Some(prompt),
                error: Some(info),
                kind,
            }
        }
    }
}

/// Run the textures pipeline (image generation + splice) on a background
/// thread and shape the result into an [`LlmOutcome`] so it rides the same
/// channel as the text-LLM paths. Reports "PNGs written" in the `calls` slot
/// so `poll_llm` can display a counter without adding a new field.
///
/// Note: parsing and `build_plan` happen on this thread too — the previous
/// version did them on the UI thread before spawning, which stalled the
/// frame for big scenes.
pub(super) fn run_llm_textures(
    src: String,
    mg_path: PathBuf,
    api_key: String,
    cfg: TextureUiConfig,
    material_filter: Option<Vec<String>>,
    tx: Sender<LlmMessage>,
) -> LlmOutcome {
    let send_progress = |p: LlmProgress| {
        let _ = tx.send(LlmMessage::Progress(p));
    };
    let texture_model = DEFAULT_IMAGE_MODEL.to_string();
    let ast = match mogen_dsl::parse(&src) {
        Ok(a) => a,
        Err(e) => {
            return LlmOutcome {
                dsl: src,
                diagnostics: Vec::new(),
                usage: Usage::default(),
                calls: 0,
                model: texture_model,
                image_calls: 0,
                retry_prompt: None,
                error: Some(super::types::LlmErrorInfo {
                    headline: "Parse error".into(),
                    detail: format!("Could not parse the .mog source: {e}"),
                    class: super::types::LlmErrorClass::BadRequest,
                    retryable: false,
                }),
                kind: LlmKind::Textures,
            };
        }
    };

    // A per-material regenerate (right-click → Regenerate) implies "redo this
    // material's slots from scratch", so override `force` for the filtered
    // run regardless of what the panel checkbox says.
    let force = cfg.force || material_filter.is_some();
    let args = TexturesArgs {
        textures_dir: default_textures_dir(&mg_path),
        input: mg_path.clone(),
        out: None,
        glb: None,
        style: cfg.style.clone(),
        model: texture_model.clone(),
        force,
        dry_run: false,
        no_build: true,
        api_key: Some(api_key.clone()),
        no_pbr: false,
        no_normal: cfg.no_normal,
        no_metallic_roughness: cfg.no_metallic_roughness,
        no_occlusion: cfg.no_occlusion,
        texture_size: cfg.texture_size,
    };

    let plans: Vec<_> = build_plan(&ast, &args)
        .into_iter()
        .filter(|p| match &material_filter {
            Some(only) => only.iter().any(|m| m == &p.material),
            None => true,
        })
        .collect();

    // If nothing needs generating *or* deriving, leave the source untouched so
    // the editor doesn't get marked dirty.
    let anything_to_do = plans.iter().any(|p| {
        matches!(
            p.action,
            PlanAction::Generate | PlanAction::Derive | PlanAction::UseExisting
        )
    });
    if !anything_to_do {
        return LlmOutcome {
            dsl: src,
            diagnostics: Vec::new(),
            usage: Usage::default(),
            calls: 0,
            model: texture_model,
            image_calls: 0,
            retry_prompt: None,
            error: Some(super::types::LlmErrorInfo {
                headline: "Nothing to generate".into(),
                detail: "Every material already has a full PBR texture set.".into(),
                class: super::types::LlmErrorClass::BadRequest,
                retryable: false,
            }),
            kind: LlmKind::Textures,
        };
    }

    // Count the image-API calls we're about to make so we can charge the
    // session meter a per-image cost. Only albedo `Generate` plans hit the
    // API; cache hits and derivations are local.
    let image_call_count = plans
        .iter()
        .filter(|p| matches!(p.action, PlanAction::Generate))
        .count() as u32;

    // Image generation is Gemini-only — `LlmClient::new(provider, …)` doesn't
    // expose a synthesis API for the other backends. Callers MUST pass the
    // `GEMINI_API_KEY` here regardless of `settings.provider()` so this path
    // keeps working even when the user has selected OpenAI/Anthropic/Ollama
    // for the text DSL.
    let client = GeminiClient::new(api_key);
    let base_dir = mg_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let tx_for_progress = tx.clone();
    let progress_cb = move |ev: TextureProgress| {
        let _ = tx_for_progress.send(LlmMessage::Progress(LlmProgress::Texture {
            current: ev.current,
            total: ev.total,
            material: ev.material,
            stage: ev.stage,
        }));
    };

    let edits = match run_plan(
        Some(&client),
        &args.model,
        &args,
        &ast,
        &plans,
        &base_dir,
        Some(&progress_cb),
    ) {
        Ok(e) => e,
        Err(e) => {
            return LlmOutcome {
                dsl: src,
                diagnostics: Vec::new(),
                usage: Usage::default(),
                calls: 0,
                model: texture_model,
                image_calls: 0,
                retry_prompt: None,
                error: Some(super::types::LlmErrorInfo {
                    headline: "Texture generation failed".into(),
                    detail: format!("{e}"),
                    class: super::types::LlmErrorClass::Other,
                    retryable: true,
                }),
                kind: LlmKind::Textures,
            };
        }
    };

    send_progress(LlmProgress::Status(format!(
        "splicing {} texture path(s) into DSL…",
        edits.len()
    )));

    match splice_textures(&src, &edits) {
        Ok(new_src) => LlmOutcome {
            dsl: new_src,
            diagnostics: Vec::new(),
            usage: Usage::default(),
            calls: edits.len() as u32,
            model: texture_model,
            image_calls: image_call_count,
            retry_prompt: None,
            error: None,
            kind: LlmKind::Textures,
        },
        Err(e) => LlmOutcome {
            dsl: src,
            diagnostics: Vec::new(),
            usage: Usage::default(),
            calls: 0,
            model: texture_model,
            image_calls: image_call_count,
            retry_prompt: None,
            error: Some(super::types::LlmErrorInfo {
                headline: "Texture splice failed".into(),
                detail: format!("PNGs were written but rewriting the DSL failed: {e}"),
                class: super::types::LlmErrorClass::Other,
                retryable: false,
            }),
            kind: LlmKind::Textures,
        },
    }
}

/// Run a single prompt-enhancement call against the fast model. Returns the
/// rewritten prompt on success or a human-readable error string. Kept
/// separate from `run_llm` because the contract is plain prose, not DSL —
/// system instruction is intentionally minimal (no grammar, no stdlib) so
/// the token bill stays tiny.
///
/// `target` selects a rewrite template matched to the field being enhanced:
/// the Generate field wants a fresh asset description, Modify/Animate want
/// precise edit requests against an existing scene, and Texture Style wants
/// a compact PBR material descriptor. Using one template for all four
/// produced rewrites that were technically vivid but the wrong shape for
/// three of the four callers.
pub(super) fn run_prompt_enhance(
    target: EnhanceTarget,
    raw_prompt: String,
    provider: Provider,
    api_key: String,
    model: String,
    claude_code_path: String,
) -> Result<String, String> {
    let client = build_provider_client(provider, api_key, &claude_code_path);
    let raw = raw_prompt.trim();
    // Templates focus the rewrite on enriching the user's high-level
    // description — what the object looks like or how a part should change —
    // and deliberately stay silent about primitives, CSG, mirror/array, or
    // animation templates. The downstream generate/modify/animate prompts
    // already carry the DSL contract; bleeding it into the enhanced prompt
    // pre-bakes implementation choices and steers the actual generation step.
    let user = match target {
        EnhanceTarget::Generate => format!(
             "Enrich the following asset description with vivid, concrete visual \
              detail about the object itself — overall silhouette and proportion, \
              character or mood, surface materials and colour cues. Describe ONLY \
              the object. Do NOT add a setting, scene, environment, background, \
              location, lighting, weather, surroundings, or any other object \
              (companions, props, base, pedestal, ground, etc.). Do not prescribe \
              how it should be modelled, list parts, or suggest construction \
              steps. Keep it compact (1–3 sentences). The original subject phrase \
              \"{raw}\" MUST appear verbatim in your rewrite (typically as the \
              opening noun phrase) — you are adding detail to it, not replacing \
              it with a synonym or a different object. Do NOT use code fences, \
              do NOT prefix with \"Enhanced prompt:\" or similar. Reply with \
              only the rewritten prompt.\n\nPrompt: {raw}",
        ),
        EnhanceTarget::Modify => format!(
            "Rewrite the following instruction as a clear, specific edit request \
             against an existing 3D scene. Be precise about which named part \
             changes, in which direction, and by how much when it matters. Keep \
             it imperative (\"make…\", \"replace…\", \"scale…\"), 1–3 sentences. \
             Assume the scene already exists; do not redesign the whole object \
             and do not prescribe modelling steps or implementation details. Do \
             NOT use code fences, do NOT prefix with labels. Reply with only the \
             rewritten instruction.\n\nInstruction: {raw}",
        ),
        EnhanceTarget::Animate => format!(
            "Rewrite the following animation request for an existing 3D scene. \
             Be specific about which named part moves, what kind of motion \
             (rotation, swing, bob, …), the axis or direction, the amplitude or \
             angle, the speed or duration, and whether it loops. 1–3 sentences, \
             imperative tone. Do not redesign the object and do not prescribe \
             implementation details. Do NOT use code fences, do NOT prefix with \
             labels. Reply with only the rewritten request.\n\nRequest: {raw}",
        ),
        EnhanceTarget::TextureStyle => format!(
            "Rewrite the following texture / material hint into a compact PBR \
             style descriptor — colour palette, finish, wear, era or setting \
             cues. This text is appended to every material's albedo-image \
             generation prompt, so it must read as stylistic guidance, not as a \
             scene description. ≤ 20 words, comma-separated adjectives and short \
             noun phrases, no full sentences, no leading label. Do NOT use code \
             fences. Reply with only the rewritten hint.\n\nHint: {raw}",
        ),
    };

    let mut cfg = GenerateConfig::new(user);
    cfg.model = model;
    // Prompt rewriting is a low-reasoning task — Low budget is plenty and
    // keeps latency/cost in line with the "fast" label.
    cfg.thinking_level = Some(ThinkingLevel::Low);
    // A touch of variance produces more natural phrasing than the DSL default.
    cfg.temperature = Some(0.7);

    match client.generate(&cfg) {
        Ok(resp) => {
            let cleaned = resp.text.trim();
            // Strip markdown fences if the model ignored the "no code" hint.
            let cleaned = cleaned
                .strip_prefix("```")
                .map(|s| s.trim_start_matches(char::is_alphanumeric).trim_start())
                .unwrap_or(cleaned)
                .trim_end_matches("```")
                .trim();
            if cleaned.is_empty() {
                Err(format!("empty response from {}", provider.label()))
            } else {
                Ok(cleaned.to_string())
            }
        }
        Err(e) => Err(format!("{e}")),
    }
}

pub(super) fn pick_default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}
