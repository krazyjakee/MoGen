//! Persisted Studio configuration. Split into focused submodules and
//! re-exported here so existing `crate::settings::*` callers keep working.
//!
//! - `provider_slot` — `ProviderSlot`, `PROVIDER_SLOTS`, OAuth-Gemini
//!   default model constants.
//! - `image_provider` — `ImageProvider` and `IMAGE_PROVIDERS`.
//! - `paths` — `settings.json` path resolution + MoGHub URL default.

mod image_provider;
mod paths;
mod provider_slot;

pub use image_provider::{ImageProvider, IMAGE_PROVIDERS};
pub use provider_slot::{
    ProviderSlot, DEFAULT_OAUTH_GEMINI_FAST_MODEL, DEFAULT_OAUTH_GEMINI_MODEL, PROVIDER_SLOTS,
};

use std::fs;

use mogen_llm::gemini::{DEFAULT_MODEL, DEFAULT_TEMPERATURE};
use mogen_llm::{Provider, Style, ThinkingLevel, STYLES};
use mogen_moghub_client::session_store as moghub_session;
use serde::{Deserialize, Serialize};

use crate::preview_shader::{
    parse_preview_shader, preview_shader_key, PreviewShader, DEFAULT_PREVIEW_SHADER,
};
use crate::theme::{parse_theme, theme_key, Theme, DEFAULT_THEME};
use crate::viewer::environment::{
    environment_key, parse_environment, Environment, DEFAULT_ENVIRONMENT,
};
use crate::viewer::shadows::{
    parse_shadow_quality, shadow_quality_key, ShadowQuality, DEFAULT_SHADOW_QUALITY,
};

/// Library default for the text-LLM repair budget. Matches
/// [`mogen_llm::RepairConfig::default`].
pub const DEFAULT_MAX_REPAIR_ITERS: u32 = 2;

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub gemini_api_key: String,
    /// Base URL for the MoGHub registry / community API. Default points
    /// at production; set to `http://localhost:3000` for dev or to a
    /// private deployment URL. Honoured at runtime by the Community
    /// window and by the registry-aware `mogen build`.
    #[serde(default = "paths::default_moghub_url")]
    pub moghub_url: String,
    /// In-memory cache of the MoGHub session UUID returned by the
    /// loopback OAuth flow. Authoritative storage is
    /// `~/.mogen/moghub_auth.json` (shared with the `mogen` CLI); this
    /// field is populated from that file on load and is never
    /// serialised back. The legacy field on disk (pre-`~/.mogen/`
    /// migration) is still tolerated on deserialisation so users
    /// upgrading from older Studios get migrated transparently.
    #[serde(default, skip_serializing)]
    pub moghub_session: String,
    /// Z.ai (`glm-image`) API key. Used by the textures pipeline only when
    /// `image_provider == ImageProvider::ZAI`. Falls back to the
    /// `ZAI_API_KEY` env var when this field is empty.
    #[serde(default)]
    pub zai_api_key: String,
    /// Persisted as a lowercase label (`low` | `medium` | `high` | `xhigh`) so
    /// new `ThinkingLevel` variants can be added without a migration. Empty /
    /// unknown falls back to the library default at read time.
    #[serde(default)]
    pub thinking_level: String,
    /// Persisted as the [`Style::key`] slug (e.g. `"ps1"`, `"low_poly"`). Empty
    /// or unknown means "no style" — the dialog opens on `Default (no style)`
    /// and `apply_style_to_prompt` is a passthrough.
    #[serde(default)]
    pub style: String,
    /// Absolute path of the last `.mog` opened in the GUI. Used at startup to
    /// reopen the previous file. With [`Self::open_tabs`] populated this also
    /// names which tab to activate after the strip is restored.
    #[serde(default)]
    pub last_opened: Option<String>,
    /// Absolute paths of every titled tab open in the studio at last persist
    /// time, in tab-strip order. Untitled buffers are skipped (no path to key
    /// off). Empty after upgrade-from-old-settings, in which case startup
    /// falls back to opening just [`Self::last_opened`].
    #[serde(default)]
    pub open_tabs: Vec<String>,
    /// Most-recently-opened `.mog` files, newest first. Capped at
    /// [`Self::MAX_RECENT`] entries. Drives the File → Open Recent menu.
    #[serde(default)]
    pub recent_files: Vec<String>,
    /// Directory the custom file picker last browsed. Persisted across
    /// sessions so reopening Open / Save As / Import lands the user back
    /// where they were instead of in the project root every time. `None`
    /// falls back to the active file's parent (or the project root) at
    /// open time.
    #[serde(default)]
    pub last_picker_dir: Option<String>,
    /// Persisted as a lowercase label (see `theme_key`) so new `Theme` variants
    /// can be added without a migration. Empty / unknown falls back to
    /// `DEFAULT_THEME` at read time.
    #[serde(default)]
    pub theme: String,
    /// Persisted as a lowercase label (see `preview_shader_key`). Empty /
    /// unknown falls back to `DEFAULT_PREVIEW_SHADER`.
    #[serde(default)]
    pub preview_shader: String,

    /// "Thinking" model id used for the heavy text paths (generate, modify,
    /// animate and their repair loops) when the active provider is Gemini.
    /// Empty -> library default (`gemini-pro-latest`). Exposed in
    /// Options → Models.
    #[serde(default)]
    pub gemini_model: String,

    /// "Fast" model id used for low-stakes rewrites like the Prompt Enhancer
    /// when the active provider is Gemini. Empty -> [`mogen_llm::gemini::DEFAULT_FAST_MODEL`]
    /// (`gemini-flash-latest`). Kept separate from `gemini_model` so users
    /// can pay Flash rates for prompt polish while still running Pro for the
    /// actual DSL generation.
    #[serde(default)]
    pub gemini_fast_model: String,

    /// OpenAI thinking-model override. Empty -> [`mogen_llm::openai::DEFAULT_MODEL`].
    #[serde(default)]
    pub openai_model: String,

    /// OpenAI fast-model override. Empty -> [`mogen_llm::openai::DEFAULT_FAST_MODEL`].
    #[serde(default)]
    pub openai_fast_model: String,

    /// Anthropic thinking-model override. Empty -> [`mogen_llm::anthropic::DEFAULT_MODEL`].
    #[serde(default)]
    pub anthropic_model: String,

    /// Anthropic fast-model override. Empty -> [`mogen_llm::anthropic::DEFAULT_FAST_MODEL`].
    #[serde(default)]
    pub anthropic_fast_model: String,

    /// Ollama thinking-model override. Empty -> [`mogen_llm::ollama::DEFAULT_MODEL`].
    /// Ollama only ships one default model id; the "fast" slot reuses the
    /// same string unless the user overrides it explicitly below.
    #[serde(default)]
    pub ollama_model: String,

    /// Ollama fast-model override. Empty -> falls back to [`Self::ollama_model`]
    /// (or the library default when both are empty).
    #[serde(default)]
    pub ollama_fast_model: String,

    /// Sampling temperature. `None` uses the library default (0.3).
    /// Serialised as f32 so downgrades to older binaries don't crash on
    /// missing fields — deserialises to `None` via `serde(default)`.
    #[serde(default)]
    pub gemini_temperature: Option<f32>,

    /// Max repair iterations. `None` uses the library default (2). A higher
    /// value lets the model self-correct longer at the cost of extra calls.
    #[serde(default)]
    pub max_repair_iters: Option<u32>,

    /// User-chosen deterministic seed. `None` → derive from the DSL header if
    /// present, else random per call (what the CLI does). Exposed so users
    /// can reproduce a prior generation when they saw one they liked.
    #[serde(default)]
    pub seed_override: Option<u64>,

    /// Set once the first-launch onboarding modal has been dismissed (whether
    /// the user pasted a key or skipped). False on a fresh install or after
    /// the settings file is wiped, which is the signal to show the welcome
    /// flow again.
    #[serde(default)]
    pub onboarded: bool,

    /// Legacy LLM-provider field. Kept as a serde field so old settings files
    /// (pre-`ProviderSlot`) still deserialise; on first read after upgrade the
    /// migration in [`Settings::load`] copies the value into `provider_slot`.
    /// New writes leave this empty — `provider_slot` is the source of truth.
    #[serde(default)]
    pub provider: String,

    /// Selected provider slot, persisted as [`ProviderSlot::key`]. Splits
    /// Gemini into `gemini-apikey` and `gemini-oauth` so users can pick the
    /// auth mode explicitly. Empty falls back to the legacy `provider` field
    /// at load time, then to [`ProviderSlot::default`].
    #[serde(default)]
    pub provider_slot: String,

    /// API key for the OpenAI provider. Stored alongside the Gemini key so
    /// switching providers in Options doesn't require re-pasting credentials.
    #[serde(default)]
    pub openai_api_key: String,

    /// API key for the Anthropic (Claude) provider.
    #[serde(default)]
    pub anthropic_api_key: String,

    /// Optional bearer token for an Ollama endpoint sitting behind an
    /// authenticating reverse proxy. Usually empty — local Ollama is keyless.
    #[serde(default)]
    pub ollama_api_key: String,

    /// Optional override for the Ollama base URL. Empty → library default
    /// (`http://localhost:11434`). Set this to point at a self-hosted
    /// instance.
    #[serde(default)]
    pub ollama_base_url: String,

    /// Optional override for the Claude Code binary path. Empty → resolve
    /// `claude` from `PATH`. Set this when the user's install lives outside
    /// `PATH` (e.g. a `~/.local/bin/claude` they haven't shimmed in yet).
    #[serde(default)]
    pub claude_code_path: String,

    /// API key for Fireworks AI. Stored as a plain string so switching to
    /// Fireworks in the provider dropdown doesn't require re-pasting the
    /// `fw_…` token. Empty → fall back to the `FIREWORKS_API_KEY` env var.
    #[serde(default)]
    pub fireworks_api_key: String,

    /// Fireworks thinking-model override. Empty → [`mogen_llm::fireworks::DEFAULT_MODEL`]
    /// (the `kimi-k2p6` Fire Pass router).
    #[serde(default)]
    pub fireworks_model: String,

    /// Fireworks fast-model override. Empty → [`mogen_llm::fireworks::DEFAULT_FAST_MODEL`]
    /// (the `kimi-k2p6-turbo` Fire Pass router).
    #[serde(default)]
    pub fireworks_fast_model: String,

    /// Z.ai (chat) thinking-model override. Empty → [`mogen_llm::zai_chat::DEFAULT_MODEL`]
    /// (`glm-5.1`). Note: the existing [`Self::zai_api_key`] doubles as the
    /// chat key — Z.ai issues one key per account that covers both surfaces.
    #[serde(default)]
    pub zai_chat_model: String,

    /// Z.ai fast-model override. Empty → [`mogen_llm::zai_chat::DEFAULT_FAST_MODEL`].
    #[serde(default)]
    pub zai_chat_fast_model: String,

    /// Persisted 3D viewport background colour, as `[r, g, b]` 0..=255. `None`
    /// falls back to [`DEFAULT_VIEWER_BG_RGB`] — a neutral charcoal that
    /// matches the look of every major DCC app. Stored as bytes so the JSON
    /// stays readable; alpha is implied 255.
    #[serde(default)]
    pub viewer_bg_rgb: Option<[u8; 3]>,

    /// Whether the ground-plane reference grid is drawn in the 3D viewport.
    /// `None` falls back to `true` so existing settings files keep the grid
    /// visible after upgrade.
    #[serde(default)]
    pub show_grid: Option<bool>,

    /// Whether the per-`light`-node indicator overlays (point sphere / spot
    /// cone / directional arrow) are drawn in the 3D viewport. `None` falls
    /// back to `true` so existing settings files keep the indicators visible
    /// after upgrade.
    #[serde(default)]
    pub show_light_gizmos: Option<bool>,

    /// Whether the translate/rotate/scale gizmo handles are drawn on the
    /// selected node. `None` falls back to `true`.
    #[serde(default)]
    pub show_transform_gizmo: Option<bool>,

    /// Whether the AABB collider wireframe overlay is drawn in the 3D
    /// viewport. `None` falls back to `false` — colliders are an opt-in
    /// view that most users won't be authoring against.
    #[serde(default)]
    pub show_colliders: Option<bool>,

    /// Active environment-lighting preset, persisted as a lowercase label
    /// (see `environment_key`). Empty / unknown falls back to
    /// [`DEFAULT_ENVIRONMENT`] at read time so adding new presets later
    /// doesn't invalidate old settings files.
    #[serde(default)]
    pub environment: String,

    /// Viewport shadow-mapping quality. Persisted as a lowercase label
    /// (see `shadow_quality_key`); empty / unknown falls back to
    /// [`DEFAULT_SHADOW_QUALITY`] (Off) so existing settings files keep the
    /// historical no-shadow look after upgrade.
    #[serde(default)]
    pub shadow_quality: String,

    /// Cap on continuous viewport repaints (animation tick, cinema pan, gizmo
    /// drag). The cap is applied by routing the per-frame repaint request
    /// through `request_repaint_after(1 / fps)` instead of the immediate
    /// variant — input-driven repaints still fire as soon as input arrives.
    ///
    /// Encoding: `None` = unset → falls back to [`DEFAULT_MAX_FPS`] at read
    /// time. `Some(0)` = explicit "Unlimited" (defer to the display's vsync).
    /// `Some(n)` for `n > 0` = capped at `n` FPS. The sentinel lets a fresh
    /// install land on the 60 FPS default while still letting users opt back
    /// out via the Options dialog.
    #[serde(default)]
    pub max_fps: Option<u32>,

    /// User decision on sending crash reports to MoGen's self-hosted
    /// GlitchTip endpoint. `None` means undecided — the first-launch privacy
    /// prompt asks the user, then latches `Some(true)` (allow) or
    /// `Some(false)` (decline). The `MOGEN_DISABLE_TELEMETRY` and
    /// `DO_NOT_TRACK` env vars short-circuit to disabled regardless of the
    /// saved value, so users can opt out before touching this file.
    #[serde(default)]
    pub crash_reports_enabled: Option<bool>,

    /// Image-generation provider preference. `"auto"` (or empty) prefers an
    /// Antigravity OAuth bundle when one is present, falls back to the
    /// Gemini API key. `"antigravity"` forces the OAuth bundle (errors if
    /// none stored). `"apikey"` forces the API key (errors if blank). Lets
    /// users sidestep the Antigravity image surface when it 404s on a model
    /// — falling back to the public `generativelanguage` API key path —
    /// without re-authenticating.
    #[serde(default)]
    pub image_provider: String,
}

/// Default viewer background. Independent of the UI theme so the model's
/// colours read consistently regardless of the panel scheme. Tuned to match
/// Blender / Maya / Modo defaults.
pub const DEFAULT_VIEWER_BG_RGB: [u8; 3] = [54, 58, 64];

/// Factory default for the viewport repaint cap. Picked to match the most
/// common display refresh rate while keeping battery / thermals reasonable
/// during long animation playback. Users can raise the cap or pick
/// "Unlimited" from the Options dialog.
pub const DEFAULT_MAX_FPS: u32 = 60;

impl Settings {
    /// Maximum number of entries kept in [`Self::recent_files`].
    pub const MAX_RECENT: usize = 12;

    pub fn load() -> Self {
        let mut s = Self::load_raw();
        s.hydrate_moghub_session();
        s
    }

    fn load_raw() -> Self {
        // Read mode walks legacy locations first, so existing installs
        // (Studio's old `dirs::config_dir()/mogen/settings.json` plus
        // earlier `~/.cache/mogen/` users) load on first launch after
        // the move. `Settings::save` then rewrites to `~/.mogen/`.
        let Some(path) = paths::settings_path(mogen_llm::PathMode::Read) else {
            return Self::default();
        };
        let bytes_result = fs::read(&path);
        let bytes = match bytes_result {
            Ok(b) => b,
            Err(_) => {
                // `mogen_llm::settings_store_path(Read)` falls through
                // to the canonical `~/.mogen/` write target when no
                // legacy file exists. If reading that came up empty,
                // try the explicit pre-move Studio path too — covers
                // setups where `MOGEN_CACHE_DIR` is set (which makes
                // the read helper skip the `dirs::config_dir()` slot).
                let Some(legacy) = paths::legacy_settings_path() else {
                    return Self::default();
                };
                let Ok(b) = fs::read(&legacy) else {
                    return Self::default();
                };
                b
            }
        };
        let mut s: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        // Pre-`ProviderSlot` settings only stored `provider`; copy it over so
        // the rest of the app reads the slot field uniformly. Migration is a
        // one-shot — once `provider_slot` is non-empty, `provider` is ignored.
        if s.provider_slot.trim().is_empty() {
            if let Some(slot) = ProviderSlot::parse(&s.provider) {
                s.provider_slot = slot.key().to_string();
            }
        }
        // First-time onboarding: when no slot has ever been picked, prefer the
        // OAuth slot if a Google token bundle already exists on disk. Users
        // who ran `mogen auth login` shouldn't have to also manually flip the
        // provider dropdown — picking the OAuth slot makes both the text-LLM
        // and image-gen paths route through their paid Antigravity plan.
        if s.provider_slot.trim().is_empty() {
            if let Some(path) = mogen_llm::token_store_path() {
                if matches!(mogen_llm::load_bundle(&path), Ok(Some(_))) {
                    s.provider_slot = ProviderSlot::GeminiOAuth.key().to_string();
                }
            }
        }
        s
    }

    pub fn save(&self) -> Result<(), String> {
        // Always write to the canonical `~/.mogen/settings.json` so
        // the CLI (which does the same path resolution) can read keys
        // entered in Studio. Atomic write via sibling-tmp + rename so a
        // crash mid-write can't truncate the file; on Unix we chmod 0600
        // before the rename so the API keys aren't world-readable.
        let path = paths::settings_path(mogen_llm::PathMode::Write)
            .ok_or_else(|| "no config directory available".to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;

        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&tmp, perms)
                .map_err(|e| format!("chmod {}: {e}", tmp.display()))?;
        }

        fs::rename(&tmp, &path)
            .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// On every load: prefer the value in `~/.mogen/moghub_auth.json`
    /// over whatever the legacy `settings.json` field carried. If the
    /// new file is empty and the legacy field is set (the typical
    /// upgrade path from a Studio that wrote the secret into
    /// settings.json or stored it in the OS keyring), migrate the
    /// value to the new file and resave settings.json so the field
    /// stops round-tripping there. The CLI reads the same file, so
    /// this also surfaces a Studio sign-in to `mogen auth moghub
    /// status`.
    fn hydrate_moghub_session(&mut self) {
        match moghub_session::read_session() {
            Some(token) => {
                self.moghub_session = token;
            }
            None => {
                if !self.moghub_session.is_empty() {
                    let base = if self.moghub_url.trim().is_empty() {
                        None
                    } else {
                        Some(self.moghub_url.as_str())
                    };
                    if moghub_session::save_session(&self.moghub_session, base).is_ok() {
                        // Re-save settings.json so the legacy
                        // moghub_session field is dropped from disk
                        // (the field is `skip_serializing`, so this
                        // overwrite is what actually clears it).
                        let _ = self.save();
                    }
                }
            }
        }
    }

    /// Persist a freshly-issued MoGHub session token. Writes
    /// `~/.mogen/moghub_auth.json` and updates the in-memory cache.
    /// `save()` is best-effort so anything else mutated alongside the
    /// sign-in still lands.
    pub fn set_moghub_session(&mut self, token: &str) -> Result<(), String> {
        let base = if self.moghub_url.trim().is_empty() {
            None
        } else {
            Some(self.moghub_url.as_str())
        };
        moghub_session::save_session(token, base)
            .map_err(|e| format!("write moghub session: {e}"))?;
        self.moghub_session = token.to_string();
        self.save()
    }

    /// Wipe the persisted MoGHub session. Removes the on-disk session
    /// file (across canonical + legacy paths) and clears the cached
    /// field, then saves settings.json so any in-memory mutations
    /// alongside the sign-out also land.
    pub fn clear_moghub_session(&mut self) -> Result<(), String> {
        moghub_session::clear_session()
            .map_err(|e| format!("remove moghub session: {e}"))?;
        self.moghub_session.clear();
        self.save()
    }

    pub fn gemini_api_key(&self) -> Option<&str> {
        let k = self.gemini_api_key.trim();
        if k.is_empty() {
            None
        } else {
            Some(k)
        }
    }

    /// Persisted Z.ai API key. `None` when the field is blank — callers
    /// should fall back to the `ZAI_API_KEY` env var before failing.
    pub fn zai_api_key(&self) -> Option<&str> {
        let k = self.zai_api_key.trim();
        if k.is_empty() {
            None
        } else {
            Some(k)
        }
    }

    /// Resolve the persisted label to a `ThinkingLevel`, falling back to the
    /// library default (`High`) when the field is empty or unknown.
    pub fn thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::parse(&self.thinking_level).unwrap_or(ThinkingLevel::High)
    }

    /// Resolve the persisted slug to a [`Style`], or `None` for the
    /// "Default (no style)" choice. Unknown values map to `None` so a
    /// hand-edited settings file with a deprecated slug degrades
    /// silently to the default.
    pub fn style(&self) -> Option<Style> {
        Style::parse(&self.style)
    }

    /// Persist a fresh style choice. `None` clears the field so future
    /// loads land on `Default (no style)`.
    pub fn set_style(&mut self, s: Option<Style>) {
        self.style = s.map(|s| s.key().to_string()).unwrap_or_default();
    }

    /// Resolve the persisted label to a `Theme`, falling back to `DEFAULT_THEME`
    /// when the field is empty or unknown.
    pub fn theme(&self) -> Theme {
        parse_theme(&self.theme).unwrap_or(DEFAULT_THEME)
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme_key(theme).to_string();
    }

    /// Resolve the persisted label to a `PreviewShader`, falling back to
    /// `DEFAULT_PREVIEW_SHADER` when the field is empty or unknown.
    pub fn preview_shader(&self) -> PreviewShader {
        parse_preview_shader(&self.preview_shader).unwrap_or(DEFAULT_PREVIEW_SHADER)
    }

    pub fn set_preview_shader(&mut self, shader: PreviewShader) {
        self.preview_shader = preview_shader_key(shader).to_string();
    }

    /// Current Gemini text model, falling back to the library default when
    /// the setting is empty.
    pub fn gemini_model(&self) -> String {
        let m = self.gemini_model.trim();
        if m.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            m.to_string()
        }
    }

    /// Sampling temperature, clamped to a sane range so a corrupted file
    /// can't feed nonsense into the API.
    pub fn temperature(&self) -> f32 {
        self.gemini_temperature
            .unwrap_or(DEFAULT_TEMPERATURE)
            .clamp(0.0, 2.0)
    }

    /// Max repair iterations, clamped to [0, 5]. Zero disables the loop.
    pub fn max_repair_iters(&self) -> u32 {
        self.max_repair_iters
            .unwrap_or(DEFAULT_MAX_REPAIR_ITERS)
            .min(5)
    }

    pub fn seed_override(&self) -> Option<u64> {
        self.seed_override
    }

    /// Resolve the persisted slot key to a [`ProviderSlot`], falling back to
    /// [`ProviderSlot::default`] (GeminiApiKey) when the field is empty or
    /// unknown. Migration from the legacy `provider` field happens in
    /// [`Self::load`].
    pub fn provider_slot(&self) -> ProviderSlot {
        ProviderSlot::parse(&self.provider_slot).unwrap_or_default()
    }

    /// Wire-level [`Provider`] for the active slot. Used by every callsite
    /// that talks to `mogen-llm` (which doesn't model the API-key vs OAuth
    /// split — that's a Studio-side credential decision).
    pub fn provider(&self) -> Provider {
        self.provider_slot().to_provider()
    }

    /// API key for the currently-selected provider, or `None` when no key is
    /// applicable. The Gemini OAuth slot returns `None` even if the user has
    /// a Gemini API key saved — picking the OAuth slot is an explicit "use
    /// the OAuth bundle" instruction, not a fallback. Other slots fall
    /// through to their per-provider key field.
    pub fn provider_api_key(&self) -> Option<&str> {
        let raw = match self.provider_slot() {
            ProviderSlot::GeminiApiKey => self.gemini_api_key.as_str(),
            ProviderSlot::GeminiOAuth => return None,
            ProviderSlot::OpenAI => self.openai_api_key.as_str(),
            ProviderSlot::Anthropic => self.anthropic_api_key.as_str(),
            ProviderSlot::Ollama => self.ollama_api_key.as_str(),
            ProviderSlot::ClaudeCode => "",
            ProviderSlot::Fireworks => self.fireworks_api_key.as_str(),
            ProviderSlot::Zai => self.zai_api_key.as_str(),
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Path to the Claude Code binary, falling back to `claude` (resolved
    /// against `PATH`) when unset.
    pub fn claude_code_path(&self) -> String {
        let p = self.claude_code_path.trim();
        if p.is_empty() {
            mogen_llm::claude_code::DEFAULT_BINARY.to_string()
        } else {
            p.to_string()
        }
    }

    /// Heavy / "thinking" model id for the active provider. Reads the
    /// per-provider override; falls back to [`Provider::default_model`]
    /// when the override is empty. The Gemini OAuth slot uses a different
    /// default — the public-API `gemini-pro-latest` alias does not resolve
    /// on `cloudcode-pa.googleapis.com/v1internal`, so OAuth pins to a
    /// concrete preview tag.
    pub fn provider_model(&self) -> String {
        let slot = self.provider_slot();
        let provider = slot.to_provider();
        let override_str = self.thinking_model_field(provider).trim();
        if !override_str.is_empty() {
            return override_str.to_string();
        }
        if slot.is_gemini_oauth() {
            return DEFAULT_OAUTH_GEMINI_MODEL.to_string();
        }
        provider.default_model().to_string()
    }

    /// Fast / cheap model id for the active provider. Symmetric with
    /// [`Self::provider_model`] — OAuth pins fast to `gemini-2.5-flash`
    /// for the same `latest`-alias reason.
    pub fn provider_fast_model(&self) -> String {
        let slot = self.provider_slot();
        let provider = slot.to_provider();
        let override_str = self.fast_model_field(provider).trim();
        if !override_str.is_empty() {
            return override_str.to_string();
        }
        if slot.is_gemini_oauth() {
            return DEFAULT_OAUTH_GEMINI_FAST_MODEL.to_string();
        }
        // Ollama is the only provider whose library default for fast == thinking,
        // so let the user's thinking override apply to fast too when fast is blank.
        if matches!(provider, Provider::Ollama) {
            let thinking = self.ollama_model.trim();
            if !thinking.is_empty() {
                return thinking.to_string();
            }
        }
        provider.default_fast_model().to_string()
    }

    /// Borrow the per-provider thinking-model setting field. Used by the
    /// Preferences UI so the same combobox can read/write whichever provider
    /// is currently selected. Returns `""` for providers that don't yet have
    /// a dedicated override slot (e.g. Claude Code).
    pub fn thinking_model_field(&self, provider: Provider) -> &str {
        match provider {
            Provider::Gemini => &self.gemini_model,
            Provider::OpenAI => &self.openai_model,
            Provider::Anthropic => &self.anthropic_model,
            Provider::Ollama => &self.ollama_model,
            Provider::Fireworks => &self.fireworks_model,
            Provider::Zai => &self.zai_chat_model,
            _ => "",
        }
    }

    /// Borrow the per-provider fast-model setting field.
    pub fn fast_model_field(&self, provider: Provider) -> &str {
        match provider {
            Provider::Gemini => &self.gemini_fast_model,
            Provider::OpenAI => &self.openai_fast_model,
            Provider::Anthropic => &self.anthropic_fast_model,
            Provider::Ollama => &self.ollama_fast_model,
            Provider::Fireworks => &self.fireworks_fast_model,
            Provider::Zai => &self.zai_chat_fast_model,
            _ => "",
        }
    }

    /// Mutably borrow the per-provider thinking-model field for editing.
    /// Returns `None` for providers without a dedicated override slot.
    pub fn thinking_model_field_mut(&mut self, provider: Provider) -> Option<&mut String> {
        match provider {
            Provider::Gemini => Some(&mut self.gemini_model),
            Provider::OpenAI => Some(&mut self.openai_model),
            Provider::Anthropic => Some(&mut self.anthropic_model),
            Provider::Ollama => Some(&mut self.ollama_model),
            Provider::Fireworks => Some(&mut self.fireworks_model),
            Provider::Zai => Some(&mut self.zai_chat_model),
            _ => None,
        }
    }

    /// Mutably borrow the per-provider fast-model field for editing.
    pub fn fast_model_field_mut(&mut self, provider: Provider) -> Option<&mut String> {
        match provider {
            Provider::Gemini => Some(&mut self.gemini_fast_model),
            Provider::OpenAI => Some(&mut self.openai_fast_model),
            Provider::Anthropic => Some(&mut self.anthropic_fast_model),
            Provider::Ollama => Some(&mut self.ollama_fast_model),
            Provider::Fireworks => Some(&mut self.fireworks_fast_model),
            Provider::Zai => Some(&mut self.zai_chat_fast_model),
            _ => None,
        }
    }

    /// Persist a fresh provider selection by slot. Clears the legacy
    /// `provider` string so a downgrade to a pre-slot binary doesn't read a
    /// stale value.
    pub fn set_provider_slot(&mut self, slot: ProviderSlot) {
        self.provider_slot = slot.key().to_string();
        self.provider.clear();
    }

    /// Currently chosen image-generation provider. `Auto` is the default —
    /// prefers Antigravity OAuth when a bundle is on disk, falls back to
    /// the Gemini API key.
    pub fn image_provider(&self) -> ImageProvider {
        ImageProvider::parse(&self.image_provider).unwrap_or_default()
    }

    /// Persist a fresh image-provider preference.
    pub fn set_image_provider(&mut self, p: ImageProvider) {
        self.image_provider = p.key().to_string();
    }

    /// Persisted viewport background as raw `[r, g, b]`, falling back to
    /// [`DEFAULT_VIEWER_BG_RGB`] when unset.
    pub fn viewer_bg_rgb(&self) -> [u8; 3] {
        self.viewer_bg_rgb.unwrap_or(DEFAULT_VIEWER_BG_RGB)
    }

    /// Replace the viewport background. Pass [`DEFAULT_VIEWER_BG_RGB`] to
    /// clear back to the default — we still persist it explicitly so the
    /// chosen colour is what survives a downgrade-then-upgrade.
    pub fn set_viewer_bg_rgb(&mut self, rgb: [u8; 3]) {
        self.viewer_bg_rgb = Some(rgb);
    }

    /// Whether the viewport grid is currently visible. Defaults to `true`
    /// when unset.
    pub fn show_grid(&self) -> bool {
        self.show_grid.unwrap_or(true)
    }

    pub fn set_show_grid(&mut self, on: bool) {
        self.show_grid = Some(on);
    }

    /// Whether the per-`light` indicator overlays are visible. Defaults to
    /// `true` when unset.
    pub fn show_light_gizmos(&self) -> bool {
        self.show_light_gizmos.unwrap_or(true)
    }

    pub fn set_show_light_gizmos(&mut self, on: bool) {
        self.show_light_gizmos = Some(on);
    }

    /// Whether the translate/rotate/scale handles are drawn on the selected
    /// node. Defaults to `true` when unset.
    pub fn show_transform_gizmo(&self) -> bool {
        self.show_transform_gizmo.unwrap_or(true)
    }

    pub fn set_show_transform_gizmo(&mut self, on: bool) {
        self.show_transform_gizmo = Some(on);
    }

    /// Whether the AABB collider overlay is visible. Defaults to `false`
    /// when unset — opt-in view for users actively working on collision.
    pub fn show_colliders(&self) -> bool {
        self.show_colliders.unwrap_or(false)
    }

    pub fn set_show_colliders(&mut self, on: bool) {
        self.show_colliders = Some(on);
    }

    /// Resolve the persisted label to an [`Environment`], falling back to
    /// [`DEFAULT_ENVIRONMENT`] when the field is empty or unknown.
    pub fn environment(&self) -> Environment {
        parse_environment(&self.environment).unwrap_or(DEFAULT_ENVIRONMENT)
    }

    pub fn set_environment(&mut self, env: Environment) {
        self.environment = environment_key(env).to_string();
    }

    /// Resolve the persisted label to a [`ShadowQuality`], falling back to
    /// [`DEFAULT_SHADOW_QUALITY`] when the field is empty or unknown.
    pub fn shadow_quality(&self) -> ShadowQuality {
        parse_shadow_quality(&self.shadow_quality).unwrap_or(DEFAULT_SHADOW_QUALITY)
    }

    pub fn set_shadow_quality(&mut self, q: ShadowQuality) {
        self.shadow_quality = shadow_quality_key(q).to_string();
    }

    /// Viewport repaint cap. `None` = uncapped (display vsync), `Some(n)` =
    /// cap at `n` FPS. Clamped at read time so a corrupted file can't drive
    /// a 1 fps or 10 000 fps cap. An unset stored value falls back to
    /// [`DEFAULT_MAX_FPS`]; a stored `Some(0)` is the explicit "Unlimited"
    /// opt-out (see field doc).
    pub fn max_fps(&self) -> Option<u32> {
        match self.max_fps {
            None => Some(DEFAULT_MAX_FPS),
            Some(0) => None,
            Some(n) => Some(n.clamp(15, 240)),
        }
    }

    pub fn set_max_fps(&mut self, fps: Option<u32>) {
        self.max_fps = Some(fps.unwrap_or(0));
    }

    /// Promote `path` to the front of [`Self::recent_files`], dedup'ing any
    /// previous occurrence and trimming the list to [`Self::MAX_RECENT`].
    pub fn push_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_string());
        self.recent_files.truncate(Self::MAX_RECENT);
    }

    /// Drop `path` from [`Self::recent_files`] if present.
    pub fn forget_recent(&mut self, path: &str) {
        self.recent_files.retain(|p| p != path);
    }

    /// Persist the directory the custom picker last browsed. Pass an empty
    /// string to clear. Best-effort — the caller is expected to call
    /// [`Self::save`] separately so persistence and the in-memory update
    /// land in the same write.
    pub fn set_last_picker_dir(&mut self, dir: &std::path::Path) {
        let s = dir.display().to_string();
        if s.is_empty() {
            self.last_picker_dir = None;
        } else {
            self.last_picker_dir = Some(s);
        }
    }
}

/// Lowercase label matching what `ThinkingLevel::parse` accepts.
pub fn thinking_level_key(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
    }
}

/// Human-facing label for the combobox; includes the token budget so users can
/// see why one setting is slower than another.
pub fn thinking_level_label(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Low => "Low (512 tok — fast)",
        ThinkingLevel::Medium => "Medium (2048 tok)",
        ThinkingLevel::High => "High (8192 tok — default)",
        ThinkingLevel::XHigh => "XHigh (24576 tok — slow)",
    }
}

pub const THINKING_LEVELS: [ThinkingLevel; 4] = [
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::XHigh,
];

/// Human-facing label for the style combobox. `None` is the leading
/// "Default (no style)" entry that keeps the existing behaviour
/// bit-for-bit (no prompt suffix, no `meta(style=…)` line).
pub fn style_label(s: Option<Style>) -> &'static str {
    match s {
        None => "Default (no style)",
        Some(s) => s.label(),
    }
}

/// Dropdown ordering: `None` first, then the catalogue order from
/// [`mogen_llm::STYLES`]. Length is `STYLES.len() + 1` (= 11 today).
pub const STYLE_OPTIONS: [Option<Style>; 11] = [
    None,
    Some(Style::Ps1),
    Some(Style::N64),
    Some(Style::LowPoly),
    Some(Style::HighDetail),
    Some(Style::Arcade),
    Some(Style::Voxel),
    Some(Style::CelShaded),
    Some(Style::StylizedFantasy),
    Some(Style::Cyberpunk),
    Some(Style::PixelArt),
];

// Compile-time guard: STYLE_OPTIONS must list every published style plus the
// leading `None`. Bumping `STYLES` without growing this array is a build
// error rather than a silent UI regression.
const _: () = {
    assert!(STYLE_OPTIONS.len() == STYLES.len() + 1);
};
