use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use mogen_llm::{
    apply_style_to_prompt, embed_seed_header, format_imports_preserve_block,
    generate_edits_with_repair, generate_with_repair, parse_prompt_header, parse_seed_header,
    parse_style_header, repair_message, stamp_style_header, summarize_imports, validate_text,
    GenerateConfig, GoogleCredential, ImageInput, LlmClient, OAuthBundle, Provider, RepairConfig,
    Style, ThinkingLevel, Usage, EDIT_BLOCK_INSTRUCTIONS,
};

use crate::app::error_class::classify;
use crate::app::types::{LlmKind, LlmMessage, LlmOutcome, LlmProgress};

/// Tuning knobs for one LLM run. Gathered into a struct rather than a long
/// parameter list because `run_llm` already takes seven positional args and
/// every new setting would push another through every call site.
#[derive(Clone)]
pub(in crate::app) struct LlmRunConfig {
    pub resume: bool,
    pub modeling: Arc<std::sync::Mutex<mogen_llm::session::ModelingProject>>,
    pub control: mogen_llm::session::SessionControl,
    pub recovery_path: Option<PathBuf>,
    pub brief_revision: u64,

    pub model: String,
    pub thinking: ThinkingLevel,
    pub temperature: f32,
    pub max_repair_iters: u32,
    /// `None` → pick from the DSL header if present, else random; `Some(v)` →
    /// use exactly that seed (so the user can reproduce a prior generation).
    pub seed_override: Option<u64>,
    /// Per-provider base URLs / binary paths that don't fit the bare
    /// `LlmClient::new(provider, api_key)` signature (Claude Code binary
    /// path, Z.ai endpoint toggle, Ollama base URL, generic
    /// OpenAI-compatible base URL). Consumed by [`build_provider_client`].
    pub endpoints: ProviderEndpoints,
    /// Directory of the file being edited (for `Modify`/`Animate`/`Repair`),
    /// used to resolve relative `import "X.mog"` paths so the prompt can
    /// quote bounds for each `use`. `None` for unsaved buffers — the prompt
    /// still lists imports verbatim, just without AABBs.
    pub base_dir: Option<PathBuf>,
    /// Visual-style hint for this call. `None` is a complete passthrough
    /// (no prompt suffix, no `meta(style=…)` line). For modify/animate/
    /// repair, the spawn site falls this back to the file's stamped
    /// `meta(style=…)` so styled files stay styled across edits.
    pub style: Option<Style>,
    /// When `true` and the call is `LlmKind::Generate`, run an Architect
    /// planner pass before the Coder pass. Mirrors the CLI's
    /// `mogen generate --plan` flag. Ignored on every other `LlmKind`.
    pub plan: bool,
    /// Absolute path of the `.mog` this run is attributed to. Carried
    /// through to the spend tracker (issue 60) so the Spending panel
    /// can show "this scene cost $X to date". `None` for untitled
    /// buffers — the call still records, just without a scene attribution.
    pub scene_path: Option<String>,
    /// Process-wide session id (UUID-shaped string). Lets the spend
    /// panel split today's session from lifetime totals. Empty when
    /// the caller didn't allocate one; the panel falls back to "all
    /// time" in that case.
    pub session_id: String,
}

/// Resolved credential for one LLM call. Carries either an API key (any
/// provider) or a Google OAuth bundle (Gemini-only). Construction stays in
/// `app/llm.rs::resolve_credential`; the worker threads consume the enum and
/// hand it to [`build_provider_client`].
///
/// There are two flavours of Gemini OAuth, mirroring the two desktop
/// clients we authenticate as:
///
/// - [`GeminiOAuth`](Self::GeminiOAuth) — the gemini-cli client, written
///   to `~/.mogen/google_auth.json` by `mogen auth login`. Works for text
///   generation against `cloudcode-pa.googleapis.com/v1internal:generateContent`,
///   but the image surface (`:streamGenerateContent` for nano-banana /
///   Gemini 3 Pro Image) rejects it with 403.
/// - [`AntigravityOAuth`](Self::AntigravityOAuth) — the Antigravity
///   client, written to `~/.mogen/antigravity_auth.json` by `mogen auth
///   login --antigravity`. Works for both text and image generation; the
///   only credential the image surface accepts.
#[derive(Clone)]
pub(in crate::app) enum Credential {
    ApiKey(String),
    GeminiOAuth(OAuthBundle),
    AntigravityOAuth(OAuthBundle),
    /// Z.ai (`glm-image`) API key. Used only by the textures pipeline —
    /// Z.ai isn't a text provider, so this never flows through
    /// [`build_provider_client`].
    Zai(String),
}

impl Credential {
    /// Convenience for callers that previously took a bare `String` —
    /// returns the API key if this is an [`ApiKey`](Self::ApiKey), else
    /// empty. OAuth and Z.ai callers must branch on the enum directly.
    /// Z.ai keys are intentionally NOT surfaced through this accessor so
    /// they never accidentally flow into a non-Z.ai provider.
    pub(in crate::app) fn api_key_or_empty(&self) -> String {
        match self {
            Credential::ApiKey(k) => k.clone(),
            Credential::Zai(_) | Credential::GeminiOAuth(_) | Credential::AntigravityOAuth(_) => {
                String::new()
            }
        }
    }
}

/// Per-provider connection settings that don't fit the bare
/// `LlmClient::new(provider, api_key)` signature. Bundled into one struct so
/// [`build_provider_client`] keeps a stable signature as providers gain
/// base-URL knobs. Every field is "" when unset; each provider arm treats a
/// blank value as "use the library default".
#[derive(Clone, Default)]
pub(in crate::app) struct ProviderEndpoints {
    /// Path to the `claude` binary for [`Provider::ClaudeCode`]. Blank →
    /// the client falls back to `claude` on `PATH`.
    pub claude_code_path: String,
    pub codex_path: String,
    /// Base URL for the Z.ai chat-completions surface (GLM Coding Plan vs
    /// general PaaS). Blank → library default.
    pub zai_base_url: String,
    /// Base URL for the Ollama chat endpoint (issue 67). Blank →
    /// `http://localhost:11434`. Honoured only for [`Provider::Ollama`].
    pub ollama_base_url: String,
    /// Base URL for the generic OpenAI-compatible local server (issue 68),
    /// e.g. `http://localhost:1234/v1`. Honoured only for
    /// [`Provider::OpenAiCompat`]; the client posts to `{base}/chat/completions`.
    pub openai_compat_base_url: String,
}

/// Construct an [`LlmClient`] honoring Studio-only settings that don't fit
/// the bare `LlmClient::new(provider, api_key)` signature. Claude Code
/// reroutes through `with_base_url` to honour the binary-path setting; a
/// Gemini OAuth credential routes through `gemini_from_credential` so the
/// resulting client speaks Cloud Code Assist instead of the public API.
/// Z.ai routes through `with_base_url` to honour the GLM Coding Plan
/// endpoint toggle. Ollama and the generic OpenAI-compatible server route
/// through `with_base_url` only when the user set a non-empty base URL,
/// otherwise they fall back to `LlmClient::new`'s library default.
pub(in crate::app) fn build_provider_client(
    provider: Provider,
    credential: Credential,
    endpoints: &ProviderEndpoints,
) -> LlmClient {
    match (provider, credential) {
        (Provider::Gemini, Credential::GeminiOAuth(bundle)) => {
            LlmClient::gemini_from_credential(GoogleCredential::OAuth(bundle))
        }
        (Provider::Gemini, Credential::AntigravityOAuth(bundle)) => {
            LlmClient::gemini_from_credential(GoogleCredential::AntigravityOAuth(bundle))
        }
        (Provider::Codex, _) => LlmClient::with_base_url(provider, "", &endpoints.codex_path),
        (Provider::ClaudeCode, cred) => LlmClient::with_base_url(
            provider,
            cred.api_key_or_empty(),
            &endpoints.claude_code_path,
        ),
        (Provider::Zai, cred) => {
            LlmClient::with_base_url(provider, cred.api_key_or_empty(), &endpoints.zai_base_url)
        }
        (Provider::Ollama, cred) if !endpoints.ollama_base_url.trim().is_empty() => {
            LlmClient::with_base_url(
                provider,
                cred.api_key_or_empty(),
                &endpoints.ollama_base_url,
            )
        }
        (Provider::OpenAiCompat, cred) if !endpoints.openai_compat_base_url.trim().is_empty() => {
            LlmClient::with_base_url(
                provider,
                cred.api_key_or_empty(),
                &endpoints.openai_compat_base_url,
            )
        }
        (provider, cred) => LlmClient::new(provider, cred.api_key_or_empty()),
    }
}

/// Map a Studio-side [`LlmKind`] to the spend-tracker operation tag. Used
/// to attribute calls in `~/.mogen/spend.db` so the Spending panel can
/// answer "how much went to texture generation?" / "how much to repair?".
pub(in crate::app) fn kind_to_operation(kind: LlmKind) -> mogen_llm::Operation {
    match kind {
        LlmKind::Generate => mogen_llm::Operation::Generate,
        LlmKind::Modify => mogen_llm::Operation::Modify,
        LlmKind::Animate => mogen_llm::Operation::Animate,
        LlmKind::Repair => mogen_llm::Operation::Repair,
        LlmKind::Textures => mogen_llm::Operation::Textures,
    }
}

pub(in crate::app) fn run_llm(
    kind: LlmKind,
    prompt: String,
    existing: Option<String>,
    provider: Provider,
    image: Option<ImageInput>,
    credential: Credential,
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

    let client = build_provider_client(provider, credential, &run_cfg.endpoints);
    if run_cfg.resume {
        let mut project = run_cfg.modeling.lock().unwrap().clone();
        let mut cfg = GenerateConfig::new(project.session_prompt.clone());
        cfg.model = run_cfg.model.clone();
        if let Some(settings) = &project.request_settings {
            settings.apply(&mut cfg);
        }
        cfg.user_images = project.session_images.clone();
        cfg.session_control = Some(run_cfg.control.clone());
        cfg.spend_context = mogen_llm::CallContext {
            operation: "refine".into(),
            scene_path: run_cfg.scene_path.clone(),
            session_id: Some(run_cfg.session_id.clone()),
        };
        let mut renderer = crate::app::modeling::WorkerRenderer {
            tx: tx.clone(),
            control: run_cfg.control.clone(),
            base_dir: run_cfg.base_dir.clone(),
            framing: None,
            front_yaw: None,
            last_capture: None,
            fit_image: None,
            capture_part: None,
        };
        let mut checkpoint = |p: &mogen_llm::session::ModelingProject| {
            let mut stored = run_cfg.modeling.lock().unwrap();
            if stored.brief.revision != run_cfg.brief_revision {
                return;
            }
            *stored = p.clone();
            if let Some(path) = run_cfg
                .scene_path
                .as_ref()
                .map(PathBuf::from)
                .or_else(|| run_cfg.recovery_path.clone())
            {
                if let Err(e) = p.save(&path) {
                    run_cfg.control.stop(&format!("Checkpoint failed: {e}"));
                }
            }
            send_progress(LlmProgress::Status(format!(
                "{}; {} saved responses",
                p.stage,
                p.attempts.len()
            )));
        };
        let source = existing.unwrap_or_default();
        let result = mogen_llm::session::resume_session(
            &mut project,
            &source,
            run_cfg.base_dir.as_deref(),
            &cfg,
            provider.key(),
            &mut |cfg| Ok(client.generate(cfg)?),
            &mut renderer,
            &mut checkpoint,
        );
        let (dsl, error) = resume_outcome(result, source, &mut project, &mut checkpoint);
        return LlmOutcome {
            subscription: provider == Provider::Codex,
            dsl,
            diagnostics: vec![],
            usage: run_cfg.control.meter().usage,
            calls: run_cfg.control.meter().calls,
            model: run_cfg.model,
            image_calls: 0,
            retry_prompt: None,
            error,
            kind,
        };
    }
    // Persist target context before the first potentially long provider call.
    {
        let mut project = run_cfg.modeling.lock().unwrap().clone();
        if let Some(source) = existing.as_ref() {
            if mogen_llm::session::compile(source, run_cfg.base_dir.as_deref()).is_ok()
                && !project.candidates.iter().any(|c| &c.source == source)
            {
                let mut cfg = GenerateConfig::new(&prompt);
                cfg.model = run_cfg.model.clone();
                let _ = project.record(
                    source.clone(),
                    run_cfg.base_dir.as_deref(),
                    &cfg,
                    provider.key(),
                    "Before AI edit".into(),
                    vec![],
                );
            }
        }
        let mut stored = run_cfg.modeling.lock().unwrap();
        if stored.brief.revision == run_cfg.brief_revision && run_cfg.control.check().is_ok() {
            *stored = project;
            let path = run_cfg
                .scene_path
                .as_ref()
                .map(PathBuf::from)
                .or_else(|| run_cfg.recovery_path.clone());
            if let Some(path) = path {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = stored.save(&path) {
                    send_progress(LlmProgress::Status(format!(
                        "Session checkpoint failed: {e}"
                    )));
                }
            }
        }
    }
    let seed = run_cfg.seed_override.unwrap_or_else(|| {
        existing
            .as_deref()
            .and_then(parse_seed_header)
            .unwrap_or_else(pick_default_seed)
    });

    // For edit-an-existing-file kinds, keep the original `meta(prompt=…)` value
    // so the provenance line isn't overwritten with the modify/animate text.
    let header_prompt = match kind {
        LlmKind::Generate | LlmKind::Textures => {
            // When an image was attached, annotate the prompt so the stamped
            // `meta(prompt=…)` records *why* the file looks the way it does
            // (otherwise an image-only generate writes an empty prompt, which
            // is misleading).
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

    // Precedence:
    //   - Generate / Textures: the dialog's pick is the only source.
    //   - Modify / Animate / Repair: the dialog's pick wins, but if it's
    //     `None` we fall back to the file's `meta(style=…)` so styled
    //     files stay styled across edits even when the user didn't
    //     re-pick a style on this turn.
    let effective_style: Option<Style> = match kind {
        LlmKind::Generate | LlmKind::Textures => run_cfg.style,
        LlmKind::Modify | LlmKind::Animate | LlmKind::Repair => run_cfg
            .style
            .or_else(|| existing.as_deref().and_then(parse_style_header)),
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
        LlmKind::Modify => {
            let imports_block = existing
                .as_deref()
                .and_then(|src| {
                    format_imports_preserve_block(&summarize_imports(
                        src,
                        run_cfg.base_dir.as_deref(),
                    ))
                })
                .map(|s| format!("{s}\n"))
                .unwrap_or_default();
            // Edit mode (the default): the existing file becomes the
            // baseline and the model returns SEARCH/REPLACE blocks instead
            // of re-emitting the full DSL. The repair loop transparently
            // falls back to a full rewrite when the model ignores the
            // format, so this is safe even when the response isn't blocks.
            // Skipped only when there's no existing buffer to edit (an
            // unsaved Modify call shouldn't really happen, but the spawn
            // path allows `existing = None`).
            if existing.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
                format!(
                    "You are editing an existing mogen DSL file. Apply this modification:\n\n\
                    {mod_prompt}\n\n\
                    {imports_block}\
                    Make the smallest edit that satisfies the request. Do not rename, reorder, \
                    reformat, or restyle parts the modification does not touch.\n\n\
                    Reply with one or more SEARCH/REPLACE blocks that apply your edit to the \
                    existing file below. Do not write a `meta(...)` block; the caller stamps \
                    it after generation. {edit_spec}\n\n\
                    Existing file:\n\n{existing}",
                    mod_prompt = prompt.trim(),
                    existing = existing.as_deref().unwrap_or("").trim_end(),
                    edit_spec = EDIT_BLOCK_INSTRUCTIONS,
                )
            } else {
                format!(
                    "You are editing an existing mogen DSL file. Apply this modification:\n\n\
                    {mod_prompt}\n\n\
                    {imports_block}\
                    Make the smallest edit that satisfies the request. Do not rename, reorder, \
                    reformat, or restyle parts the modification does not touch.\n\n\
                    Reply with ONLY the full modified DSL — no commentary, no markdown fences. \
                    Do not write a `meta(...)` block; the caller stamps it after generation.\n\n\
                    Existing file:\n\n{existing}",
                    mod_prompt = prompt.trim(),
                    existing = existing.as_deref().unwrap_or("").trim_end(),
                )
            }
        }
        LlmKind::Animate => {
            let imports_block = existing
                .as_deref()
                .and_then(|src| {
                    format_imports_preserve_block(&summarize_imports(
                        src,
                        run_cfg.base_dir.as_deref(),
                    ))
                })
                .map(|s| format!("{s}\n"))
                .unwrap_or_default();
            format!(
            "You are editing an existing mogen DSL file. APPEND new animation and rigging \
            declarations to satisfy this request:\n\n\
            {anim_prompt}\n\n\
            {imports_block}\
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
            - Do not touch geometry. Preserve every `import`, `scene`, `material`, `mesh`, \
              `primitive`, `group`, `array`, `mirror`, `attach`, `connector`, `socket`, `plug`, \
              `use`, and `module` exactly as written — except you MAY add a single `skin=\"…\"` \
              attribute to the one primitive that a new (B)-style rig deforms.\n\
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
            not write a `meta(...)` block; the caller stamps it after generation.\n\n\
            Existing file:\n\n{existing}",
            anim_prompt = prompt.trim(),
            existing = existing.as_deref().unwrap_or("").trim_end(),
            )
        }
        LlmKind::Repair => {
            // The validator already ran in `start_llm_repair` before we got
            // here, but we re-run it on the worker thread to get the exact
            // diagnostics + spans. `repair_message` folds the previous DSL,
            // every diagnostic (with caret excerpts), and each code's fix
            // hint into the prompt — the same shape the repair loop uses on
            // subsequent iterations.
            let existing_src = existing.as_deref().unwrap_or("");
            let diags = validate_text(existing_src);
            repair_message(
                &header_prompt,
                existing_src,
                &diags,
                &[],
                mogen_llm::repair::RepairMode::Rewrite,
            )
        }
        LlmKind::Textures => unreachable!("run_llm is text-only; textures uses run_llm_textures"),
    };

    let user_prompt = apply_style_to_prompt(&user_prompt, effective_style);
    let mut cfg = GenerateConfig::new(user_prompt);
    cfg.model = run_cfg.model.clone();
    cfg.session_control = Some(run_cfg.control.clone());
    cfg.max_output_tokens = Some(run_cfg.control.limits().output_tokens);
    cfg.seed = Some(seed);
    cfg.thinking_level = Some(run_cfg.thinking);
    cfg.temperature = Some(run_cfg.temperature);
    cfg.spend_context = mogen_llm::CallContext {
        operation: kind_to_operation(kind).as_str().to_string(),
        scene_path: run_cfg.scene_path.clone(),
        session_id: if run_cfg.session_id.is_empty() {
            None
        } else {
            Some(run_cfg.session_id.clone())
        },
    };
    // Session calls send the instruction inline: a cache-creation request must
    // not escape the session admission/billing boundary.
    cfg.system_instruction = Some((*sys_instr).clone());
    {
        let project = run_cfg.modeling.lock().unwrap();
        if let Err(e) = project.brief.attach(&mut cfg) {
            return LlmOutcome {
                subscription: provider == Provider::Codex,
                dsl: existing.unwrap_or_default(),
                diagnostics: vec![],
                usage: Usage::default(),
                calls: 0,
                model: cfg.model,
                image_calls: 0,
                retry_prompt: Some(prompt),
                error: Some(classify(&mogen_llm::ProviderError::InvalidResponse(
                    e.to_string(),
                ))),
                kind,
            };
        }
        cfg.user_prompt
            .push_str(&format!("\nEnforced part locks: {:?}\n", project.locks));
        if let Some(part) = &project.selected_part {
            cfg.user_prompt.push_str(&format!("Focus this edit only on the authored subtree named {part:?}. Preserve every byte outside it, including the existing meta block.\n"));
        }
        if project.experimental_guidance {
            cfg.system_instruction = Some(mogen_llm::session::experimental_system_instruction());
        }
    }
    if let Some(img) = image {
        // Carried through every repair iteration: `repair.rs` rewrites
        // `cfg.user_prompt` but leaves `cfg.user_images` alone, so the model
        // keeps the visual reference while it fixes validator errors.
        if kind != LlmKind::Generate {
            cfg.user_prompt.push_str("\nImage roles: the last attached image is the current model render; preceding images are original target references.\n");
            cfg.user_images.push(img);
        } else if !cfg.user_images.iter().any(|i| i.data == img.data) {
            cfg.user_images.push(img);
        }
    }

    // Planning retains the same target context and accepts image-only input.
    let plan_prompt_text = cfg.user_prompt.clone();
    let want_plan = kind == LlmKind::Generate
        && run_cfg.plan
        && (!plan_prompt_text.is_empty() || !cfg.user_images.is_empty());
    let mut prefix_usage = Usage::default();
    let mut prefix_calls: u32 = 0;
    if want_plan {
        send_progress(LlmProgress::Status(format!(
            "calling {} ({} planner) — thinking={:?}",
            provider.label(),
            kind.label(),
            run_cfg.thinking,
        )));
        match mogen_llm::generate_plan(&client, &cfg, &plan_prompt_text) {
            Ok(po) => {
                prefix_usage = po.usage.clone();
                prefix_calls = 1;
                cfg.user_prompt = mogen_llm::compose_coder_prompt(&plan_prompt_text, &po.plan);
            }
            Err(e) => {
                let info = classify(&e);
                return LlmOutcome {
                    subscription: provider == Provider::Codex,
                    dsl: existing.unwrap_or_default(),
                    diagnostics: Vec::new(),
                    usage: prefix_usage,
                    calls: prefix_calls,
                    model: run_cfg.model,
                    image_calls: 0,
                    retry_prompt: Some(prompt),
                    error: Some(info),
                    kind,
                };
            }
        }
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
        allow_edit_mode: true,
    };

    // For Modify with a non-empty existing buffer, run in edit-block mode
    // so the model can return SEARCH/REPLACE patches against the file
    // instead of re-emitting the whole DSL. The repair loop's transparent
    // rewrite fallback handles any model that ignores the format.
    let modify_baseline = match kind {
        LlmKind::Modify => existing
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    };
    let session_cfg = cfg.clone();
    let result = match modify_baseline {
        Some(baseline) => generate_edits_with_repair(&client, cfg, &repair, &baseline),
        None => generate_with_repair(&client, cfg, &repair),
    };
    match result {
        Ok(outcome) => {
            if !outcome.is_ok() {
                run_cfg.modeling.lock().unwrap().stop_reason =
                    "Candidate failed validation; existing work preserved".into();
                return LlmOutcome {
                    subscription: provider == Provider::Codex,
                    dsl: existing.unwrap_or_default(),
                    diagnostics: outcome.diagnostics,
                    usage: run_cfg.control.meter().usage,
                    calls: run_cfg.control.meter().calls,
                    model: run_cfg.model,
                    image_calls: 0,
                    retry_prompt: Some(prompt),
                    error: Some(classify(&mogen_llm::ProviderError::InvalidResponse(
                        "Candidate failed validation; existing work preserved".into(),
                    ))),
                    kind,
                };
            }
            // Roll planner usage/calls into the final summary so the
            // status line reflects the full cost of the run, not just
            // the Coder pass.
            let mut total_usage = prefix_usage.clone();
            total_usage.add(&outcome.usage);
            let total_calls = prefix_calls + outcome.call_count;
            send_progress(LlmProgress::Status(format!(
                "done — {} call(s), {} tokens",
                total_calls, total_usage.total_tokens
            )));
            let wrapped =
                embed_seed_header(&outcome.dsl, seed, &header_prompt, Some(run_cfg.thinking));
            let wrapped = mogen_dsl::stamp_mogen_version(&wrapped, env!("CARGO_PKG_VERSION"));
            let wrapped = stamp_style_header(&wrapped, effective_style);
            let mut project = run_cfg.modeling.lock().unwrap().clone();
            let wrapped = if project.selected_part.is_some() {
                outcome.dsl.clone()
            } else {
                wrapped
            };
            let mut checkpoint = |project: &mogen_llm::session::ModelingProject| {
                let mut stored = run_cfg.modeling.lock().unwrap();
                // A cancelled worker cannot overwrite a newer session or UI brief.
                if stored.brief.revision != run_cfg.brief_revision {
                    return;
                }
                if run_cfg.control.check().is_err() {
                    for candidate in &project.candidates {
                        if !stored
                            .candidates
                            .iter()
                            .any(|c| c.revision == candidate.revision)
                        {
                            stored.candidates.push(candidate.clone());
                        }
                    }
                    stored.stop_reason = project.stop_reason.clone();
                    stored.attempts = project.attempts.clone();
                    stored.stage = project.stage.clone();
                    stored.meter = project.meter.clone();
                    stored.elapsed_seconds = project.elapsed_seconds;
                    stored.session_initial = project.session_initial;
                    stored.session_context = project.session_context.clone();
                    stored.session_prompt = project.session_prompt.clone();
                    stored.session_images = project.session_images.clone();
                    stored.request_settings = project.request_settings.clone();
                } else {
                    *stored = project.clone();
                }
                let project = &*stored;
                let path = run_cfg
                    .scene_path
                    .as_ref()
                    .map(PathBuf::from)
                    .or_else(|| run_cfg.recovery_path.clone());
                if let Some(path) = path {
                    if run_cfg.scene_path.is_none() {
                        if let Some(candidate) = project
                            .selected_candidate
                            .and_then(|i| project.candidates.get(i))
                        {
                            let _ = std::fs::write(&path, &candidate.source);
                        }
                    }
                    if let Err(e) = project.save(&path) {
                        send_progress(LlmProgress::Status(format!(
                            "Session checkpoint failed: {e}"
                        )));
                        run_cfg.control.stop("Session checkpoint write failed");
                    }
                }
            };
            let guarded = existing
                .as_deref()
                .map(|old| {
                    mogen_llm::session::enforce_locks(
                        old,
                        &wrapped,
                        &project.locks,
                        run_cfg.base_dir.as_deref(),
                    )
                    .and_then(|_| {
                        mogen_llm::session::enforce_scope(
                            old,
                            &wrapped,
                            project.selected_part.as_deref(),
                        )
                    })
                })
                .unwrap_or(Ok(()));
            let mut wrapped = wrapped;
            if let Err(e) = guarded {
                project.stop_reason = format!("Edit rejected: {e}");
                checkpoint(&project);
                return LlmOutcome {
                    subscription: provider == Provider::Codex,
                    dsl: existing.unwrap_or_default(),
                    diagnostics: vec![],
                    usage: run_cfg.control.meter().usage,
                    calls: run_cfg.control.meter().calls,
                    model: session_cfg.model.clone(),
                    image_calls: 0,
                    retry_prompt: Some(prompt),
                    error: Some(classify(&mogen_llm::ProviderError::InvalidResponse(
                        e.to_string(),
                    ))),
                    kind,
                };
            }
            if project.mode == mogen_llm::session::QualityMode::Refined
                && matches!(kind, LlmKind::Generate | LlmKind::Modify)
                && outcome.is_ok()
            {
                let mut renderer = crate::app::modeling::WorkerRenderer {
                    tx: tx.clone(),
                    control: run_cfg.control.clone(),
                    base_dir: run_cfg.base_dir.clone(),
                    framing: None,
                    front_yaw: None,
                    last_capture: None,
                    fit_image: None,
                    capture_part: None,
                };
                let mut call = |cfg: &GenerateConfig| {
                    send_progress(LlmProgress::Status(format!(
                        "{} · {}",
                        cfg.spend_context.operation, cfg.model
                    )));
                    client.generate(cfg).map_err(anyhow::Error::from)
                };
                match mogen_llm::session::refine_session(
                    &mut project,
                    &wrapped,
                    run_cfg.base_dir.as_deref(),
                    &session_cfg,
                    provider.key(),
                    &mut call,
                    &mut renderer,
                    &mut checkpoint,
                ) {
                    Ok(best) => wrapped = best,
                    Err(e) => {
                        project.stop_reason = format!("Refinement stopped: {e}");
                        checkpoint(&project);
                    }
                }
            } else if outcome.is_ok() {
                match project.record(
                    wrapped.clone(),
                    run_cfg.base_dir.as_deref(),
                    &session_cfg,
                    provider.key(),
                    "Draft candidate; visual quality has not been assessed".into(),
                    vec![],
                ) {
                    Ok(i) => project.selected_candidate = Some(i),
                    Err(e) => project.stop_reason = format!("Candidate checkpoint failed: {e}"),
                }
                checkpoint(&project);
            }
            let total_usage = run_cfg.control.meter().usage;
            let total_calls = run_cfg.control.meter().calls;
            LlmOutcome {
                subscription: provider == Provider::Codex,
                dsl: wrapped,
                diagnostics: outcome.diagnostics,
                usage: total_usage,
                calls: total_calls,
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
                subscription: provider == Provider::Codex,
                dsl: existing.unwrap_or_default(),
                diagnostics: Vec::new(),
                usage: run_cfg.control.meter().usage,
                calls: run_cfg.control.meter().calls,
                model: run_cfg.model,
                image_calls: 0,
                retry_prompt: Some(prompt),
                error: Some(info),
                kind,
            }
        }
    }
}

pub(in crate::app) fn pick_default_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}

/// Turns a `resume_session` result into the `(dsl, error)` pair `run_llm`'s
/// resume branch returns. On failure, records the failure on `project` and
/// checkpoints it, and falls back to `fallback_dsl` (the pre-resume source)
/// rather than losing the in-progress edit. Split out from `run_llm` so the
/// failure path — previously hardcoded to `error: None` even on `Err`, which
/// made a failed resume look like a completed run — is unit-testable without
/// the surrounding provider/channel plumbing.
fn resume_outcome(
    result: anyhow::Result<String>,
    fallback_dsl: String,
    project: &mut mogen_llm::session::ModelingProject,
    checkpoint: &mut dyn FnMut(&mogen_llm::session::ModelingProject),
) -> (String, Option<crate::app::types::LlmErrorInfo>) {
    match result {
        Ok(source) => (source, None),
        Err(e) => {
            let detail = format!("{e:#}");
            project.stop_reason = format!("Resume stopped: {detail}");
            checkpoint(project);
            (
                fallback_dsl,
                Some(crate::app::types::LlmErrorInfo {
                    headline: "Resume failed".into(),
                    detail,
                    class: crate::app::types::LlmErrorClass::Other,
                    retryable: true,
                    action: None,
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogen_llm::session::ModelingProject;

    #[test]
    fn successful_resume_reports_no_error() {
        let mut project = ModelingProject::default();
        let mut checkpoints = 0;
        let mut checkpoint = |_: &ModelingProject| checkpoints += 1;
        let (dsl, error) = resume_outcome(
            Ok("new source".into()),
            "old source".into(),
            &mut project,
            &mut checkpoint,
        );
        assert_eq!(dsl, "new source");
        assert!(error.is_none());
        assert_eq!(checkpoints, 0);
    }

    #[test]
    fn failed_resume_reports_a_retryable_error_and_checkpoints_the_failure() {
        // Regression: this used to hardcode `error: None` in both the `Ok`
        // and `Err` arms, so a failed resume (network error, invalid
        // response, etc.) looked like a completed run with the failure text
        // buried in `project.stop_reason`.
        let mut project = ModelingProject::default();
        let mut checkpoints = 0;
        let mut checkpoint = |_: &ModelingProject| checkpoints += 1;
        let (dsl, error) = resume_outcome(
            Err(anyhow::anyhow!("network unreachable")),
            "old source".into(),
            &mut project,
            &mut checkpoint,
        );
        assert_eq!(dsl, "old source");
        let error = error.expect("a failed resume must report an error");
        assert_eq!(error.headline, "Resume failed");
        assert!(error.detail.contains("network unreachable"));
        assert_eq!(error.class, crate::app::types::LlmErrorClass::Other);
        assert!(error.retryable);
        assert!(project.stop_reason.contains("network unreachable"));
        assert_eq!(checkpoints, 1);
    }
}
