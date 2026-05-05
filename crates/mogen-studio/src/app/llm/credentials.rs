use crate::app::util::Credential;
use crate::app::MogenStudioApp;

impl MogenStudioApp {
    /// Resolve the credential for the active provider slot. The slot dictates
    /// which path is taken — there's no fallback between API-key and OAuth
    /// auth for Gemini, picking the slot in Preferences is the explicit
    /// choice.
    ///
    /// - `GeminiOAuth`: load the bundle from `google_auth.json`. No key
    ///   fallback even when one is saved.
    /// - `GeminiApiKey`: settings key → `GEMINI_API_KEY` env. No OAuth
    ///   fallback — the user picked API-key on purpose.
    /// - Other providers: settings key → env var → keyless empty key.
    pub(in crate::app) fn resolve_credential(&self) -> Option<Credential> {
        let slot = self.settings.provider_slot();
        if slot.is_gemini_oauth() {
            let path = mogen_llm::token_store_path()?;
            let bundle = mogen_llm::load_bundle(&path).ok().flatten()?;
            return Some(Credential::GeminiOAuth(bundle));
        }
        let provider = slot.to_provider();
        if let Some(k) = self.settings.provider_api_key() {
            return Some(Credential::ApiKey(k.to_string()));
        }
        let env_var = provider.env_var();
        if !env_var.is_empty() {
            if let Some(k) = std::env::var(env_var)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(Credential::ApiKey(k));
            }
        }
        if provider.is_keyless() {
            return Some(Credential::ApiKey(String::new()));
        }
        None
    }

    /// Back-compat shim that mirrors the old API-key surface for UI gating
    /// (`has_key`-style booleans). Returns `Some(())` whenever a usable
    /// credential exists — including a stored OAuth bundle for Gemini.
    pub(in crate::app) fn resolve_api_key(&self) -> Option<()> {
        self.resolve_credential().map(|_| ())
    }

    /// Provider-agnostic Gemini credential resolution for paths that
    /// hard-require Gemini regardless of the active text-LLM provider —
    /// currently just texture image generation.
    ///
    /// Precedence is steered by the user's `image_provider` preference
    /// (Preferences → LLM):
    /// - `Auto` (default): stored Antigravity OAuth bundle → persisted
    ///   `gemini_api_key` → `GEMINI_API_KEY` env → stored gemini-cli OAuth
    ///   bundle (kept last so the UI can surface a clear "wrong client"
    ///   error in `textures_run.rs`).
    /// - `Antigravity`: only the stored Antigravity OAuth bundle is
    ///   considered. Returns `None` when no bundle is on disk.
    /// - `ApiKey`: only the Gemini API key (settings or env) is considered.
    /// - `ZAI`: only the Z.ai key (settings or env) is considered. The
    ///   textures pipeline branches on [`Credential::Zai`] in
    ///   `textures_run.rs` and routes through [`mogen_llm::ImageClient::Zai`].
    pub(in crate::app) fn resolve_gemini_credential(&self) -> Option<Credential> {
        use crate::settings::ImageProvider;
        use mogen_llm::google_oauth::{ANTIGRAVITY_CONFIG, GEMINI_CLI_CONFIG};

        let pref = self.settings.image_provider();

        if matches!(pref, ImageProvider::ZAI) {
            if let Some(k) = self.settings.zai_api_key() {
                return Some(Credential::Zai(k.to_string()));
            }
            if let Some(k) = std::env::var("ZAI_API_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(Credential::Zai(k));
            }
            return None;
        }

        if matches!(pref, ImageProvider::Auto | ImageProvider::Antigravity) {
            if let Some(path) = mogen_llm::token_store_path_for(&ANTIGRAVITY_CONFIG) {
                if let Ok(Some(bundle)) = mogen_llm::load_bundle(&path) {
                    return Some(Credential::AntigravityOAuth(bundle));
                }
            }
            if matches!(pref, ImageProvider::Antigravity) {
                return None;
            }
        }

        if matches!(pref, ImageProvider::Auto | ImageProvider::ApiKey) {
            if let Some(k) = self.settings.gemini_api_key() {
                return Some(Credential::ApiKey(k.to_string()));
            }
            if let Some(k) = std::env::var("GEMINI_API_KEY")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
            {
                return Some(Credential::ApiKey(k));
            }
            if matches!(pref, ImageProvider::ApiKey) {
                return None;
            }
        }

        if let Some(path) = mogen_llm::token_store_path_for(&GEMINI_CLI_CONFIG) {
            if let Ok(Some(bundle)) = mogen_llm::load_bundle(&path) {
                return Some(Credential::GeminiOAuth(bundle));
            }
        }
        None
    }
}
