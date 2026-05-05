/// Image-generation provider preference. Surfaced in Preferences → LLM so
/// the user can choose between Gemini (API key or Antigravity OAuth) and
/// Z.ai (`glm-image`) without touching any credential's storage.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageProvider {
    /// Prefer Antigravity OAuth when a bundle is on disk; fall back to the
    /// Gemini API key. Mirrors the CLI's default resolver.
    Auto,
    /// Force the Gemini API key, even if an Antigravity bundle exists.
    ApiKey,
    /// Force the Antigravity OAuth bundle.
    Antigravity,
    /// Force Z.ai's `glm-image` endpoint. Reads the key from
    /// `Settings::zai_api_key`, falling back to the `ZAI_API_KEY` env
    /// var when the settings field is empty.
    ZAI,
}

pub const IMAGE_PROVIDERS: [ImageProvider; 4] = [
    ImageProvider::Auto,
    ImageProvider::ApiKey,
    ImageProvider::Antigravity,
    ImageProvider::ZAI,
];

impl Default for ImageProvider {
    fn default() -> Self {
        ImageProvider::Auto
    }
}

impl ImageProvider {
    pub fn label(self) -> &'static str {
        match self {
            ImageProvider::Auto => "Auto (Antigravity OAuth, fall back to API key)",
            ImageProvider::ApiKey => "Gemini API key",
            ImageProvider::Antigravity => "Antigravity OAuth",
            ImageProvider::ZAI => "Z.ai (glm-image)",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            ImageProvider::Auto => "auto",
            ImageProvider::ApiKey => "apikey",
            ImageProvider::Antigravity => "antigravity",
            ImageProvider::ZAI => "zai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "default" => Some(Self::Auto),
            "apikey" | "api_key" | "api-key" | "key" => Some(Self::ApiKey),
            "antigravity" | "oauth" => Some(Self::Antigravity),
            "zai" | "z.ai" | "z-ai" | "glm" | "glm-image" => Some(Self::ZAI),
            _ => None,
        }
    }
}
