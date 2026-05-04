//! Provider-agnostic image client. The textures pipeline only ever calls
//! [`ImageClient::generate_image`] — it shouldn't know or care whether the
//! bytes come from Gemini's `generateContent` surface, Antigravity Cloud
//! Code Assist, or Z.ai's `glm-image` endpoint.
//!
//! Each backend has its own concrete error type. We bridge them through
//! [`ImageError`] so call-site error handling stays simple. The Gemini
//! recitation retry path lives in `textures::run` and matches on
//! [`ImageError::Gemini`] specifically — Z.ai has no equivalent recitation
//! filter, so its errors propagate as-is on the first failure.
//!
//! The dispatcher *owns* the underlying client (not `&`), so callers
//! construct one [`ImageClient`] up-front and pass it through the pipeline.

use thiserror::Error;

use crate::gemini::{GeminiClient, GeminiError};
use crate::image::GeneratedImage;
use crate::zai::{ZaiClient, ZaiError};

/// Multiplexer over every supported image provider. Wraps the concrete
/// per-provider client and forwards `generate_image` through a single
/// signature the textures pipeline can drive without provider awareness.
pub enum ImageClient {
    Gemini(GeminiClient),
    Zai(ZaiClient),
}

#[derive(Debug, Error)]
pub enum ImageError {
    #[error(transparent)]
    Gemini(#[from] GeminiError),
    #[error(transparent)]
    Zai(#[from] ZaiError),
}

impl ImageClient {
    /// Issue an image generation request through whichever provider this
    /// client wraps. `model` is forwarded to the provider verbatim — pass
    /// the empty string to let the provider pick its default.
    pub fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        seed: Option<u64>,
    ) -> Result<GeneratedImage, ImageError> {
        match self {
            Self::Gemini(c) => Ok(c.generate_image(model, prompt, seed)?),
            Self::Zai(c) => Ok(c.generate_image(model, prompt, seed)?),
        }
    }

    /// Stable, lowercase tag for the active provider. Used by the CLI/Studio
    /// when reporting which backend produced (or failed to produce) a given
    /// material's textures.
    pub fn provider_name(&self) -> &'static str {
        match self {
            Self::Gemini(_) => "gemini",
            Self::Zai(_) => "zai",
        }
    }
}
