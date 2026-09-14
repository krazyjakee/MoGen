//! Provider / model resolution for [`Settings`]. Splits the slot →
//! credential → model-id logic out of `settings.rs` so the persisted-field
//! definitions and the runtime resolution read independently.

use mogen_llm::Provider;

use super::{
    ImageProvider, ProviderSlot, Settings, DEFAULT_OAUTH_GEMINI_FAST_MODEL,
    DEFAULT_OAUTH_GEMINI_MODEL,
};

impl Settings {
    /// Resolve the persisted slot key to a [`ProviderSlot`], falling back to
    /// [`ProviderSlot::default`] (OpenAI) when the field is empty or
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
            ProviderSlot::OpenAiCompat => self.openai_compat_api_key.as_str(),
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
        if self.use_preview_models {
            if let Some(preview) = preview_thinking_model(provider) {
                return preview.to_string();
            }
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
        if self.use_preview_models {
            if let Some(preview) = preview_fast_model(provider) {
                return preview.to_string();
            }
        }
        // Ollama and the generic OpenAI-compatible server both default fast ==
        // thinking, so let the user's thinking override apply to fast too when
        // fast is blank.
        if matches!(provider, Provider::Ollama | Provider::OpenAiCompat) {
            let thinking = self.thinking_model_field(provider).trim();
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
            Provider::OpenAiCompat => &self.openai_compat_model,
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
            Provider::OpenAiCompat => &self.openai_compat_fast_model,
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
            Provider::OpenAiCompat => Some(&mut self.openai_compat_model),
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
            Provider::OpenAiCompat => Some(&mut self.openai_compat_fast_model),
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

    /// True when any provider's API key field is non-empty, regardless of
    /// which slot is currently selected. Used to recognise a returning user
    /// on first launch of a new default (e.g. a pre-existing Gemini setup
    /// from before OpenAI became the default provider) — checking only
    /// [`Self::provider_api_key`] would miss credentials saved under a slot
    /// that isn't the active one.
    pub fn has_any_saved_api_key(&self) -> bool {
        [
            self.gemini_api_key.as_str(),
            self.openai_api_key.as_str(),
            self.anthropic_api_key.as_str(),
            self.ollama_api_key.as_str(),
            self.fireworks_api_key.as_str(),
            self.zai_api_key.as_str(),
            self.openai_compat_api_key.as_str(),
        ]
        .iter()
        .any(|k| !k.trim().is_empty())
    }
}

/// Bleeding-edge thinking-model id for a provider, or `None` when the
/// provider has no preview tier worth surfacing as a default. Driven by
/// [`Settings::use_preview_models`]; users still get the per-provider
/// override (left blank) → stable `*-latest` alias path when the toggle
/// is off.
pub fn preview_thinking_model(provider: Provider) -> Option<&'static str> {
    match provider {
        // Gemini 3.x Pro Preview. The OAuth slot already pins to this id
        // unconditionally; the toggle extends the same default to API-key
        // users who explicitly opt in.
        Provider::Gemini => Some("gemini-3.1-pro-preview"),
        _ => None,
    }
}

/// Bleeding-edge fast-model id, symmetric with [`preview_thinking_model`].
pub fn preview_fast_model(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Gemini => Some("gemini-3-flash-preview"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_unconfigured_settings_use_openai_astra() {
        for settings in [
            Settings::default(),
            serde_json::from_str("{}").unwrap(),
            serde_json::from_str(
                r#"{"provider_slot":"unknown","gemini_api_key":"saved-gemini-key"}"#,
            )
            .unwrap(),
        ] {
            assert_eq!(settings.provider_slot(), ProviderSlot::OpenAI);
            assert_eq!(settings.provider_model(), "gpt-6-astra");
            assert_eq!(settings.provider_fast_model(), "gpt-5-mini");
        }
    }

    #[test]
    fn saved_gemini_slots_and_custom_models_remain_available() {
        for slot in ["gemini", "gemini-apikey", "gemini-oauth"] {
            let mut settings: Settings = serde_json::from_value(
                serde_json::json!({"provider_slot": slot, "gemini_api_key": "saved-key"}),
            )
            .unwrap();
            assert_eq!(settings.provider(), Provider::Gemini);
            if slot == "gemini-oauth" {
                assert_eq!(settings.provider_model(), DEFAULT_OAUTH_GEMINI_MODEL);
                assert!(settings.provider_api_key().is_none());
            } else {
                assert_eq!(settings.provider_model(), mogen_llm::gemini::DEFAULT_MODEL);
                assert_eq!(settings.provider_api_key(), Some("saved-key"));
            }
            settings.gemini_model = "custom-gemini".into();
            assert_eq!(settings.provider_model(), "custom-gemini");
        }
        let settings: Settings =
            serde_json::from_str(r#"{"provider_slot":"openai","openai_model":"gpt-5.5"}"#).unwrap();
        assert_eq!(settings.provider_model(), "gpt-5.5");
    }

    #[test]
    fn has_any_saved_api_key_checks_every_provider_field() {
        assert!(!Settings::default().has_any_saved_api_key());

        let mut s = Settings::default();
        s.anthropic_api_key = "sk-ant-...".into();
        assert!(s.has_any_saved_api_key());

        // Whitespace-only fields don't count as configured.
        let mut s = Settings::default();
        s.fireworks_api_key = "   ".into();
        assert!(!s.has_any_saved_api_key());
    }
}
