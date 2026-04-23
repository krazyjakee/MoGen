//! Public Gemini pricing applied client-side so the studio can show a
//! running cost estimate. Rates are the published US list prices for the
//! model family and are approximate — the authoritative source is the
//! user's Google Cloud invoice.
//!
//! Rates are per million tokens. Input/output counts come from the API's
//! `usageMetadata`; `cached` is billed at a reduced rate when a
//! `cachedContents` resource is attached.

use mogen_llm::gemini::Usage;

#[derive(Debug, Clone, Copy)]
pub(super) struct TextPricing {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    pub cached_input_per_million_usd: f64,
}

/// Per-image flat cost for an image-gen model. The image API does not
/// report token usage, so we charge a constant per successful call.
#[derive(Debug, Clone, Copy)]
pub(super) struct ImagePricing {
    pub per_image_usd: f64,
}

/// Match a model name against the price table. Prefix matching means
/// `gemini-2.5-pro-preview-…` or `gemini-pro-latest` both land on Pro.
pub(super) fn text_pricing(model: &str) -> TextPricing {
    let m = model.to_ascii_lowercase();
    // Gemini 2.5 Pro (≤200k prompt) — current published list prices.
    if m.starts_with("gemini-pro") || m.contains("2.5-pro") || m.contains("2-5-pro") {
        return TextPricing {
            input_per_million_usd: 1.25,
            output_per_million_usd: 10.00,
            cached_input_per_million_usd: 0.31,
        };
    }
    // Gemini 2.5 Flash-Lite (cheapest text tier).
    if m.contains("flash-lite") {
        return TextPricing {
            input_per_million_usd: 0.10,
            output_per_million_usd: 0.40,
            cached_input_per_million_usd: 0.025,
        };
    }
    // Gemini 2.5 Flash / flash-latest.
    if m.starts_with("gemini-flash") || m.contains("2.5-flash") || m.contains("2-5-flash") {
        return TextPricing {
            input_per_million_usd: 0.30,
            output_per_million_usd: 2.50,
            cached_input_per_million_usd: 0.075,
        };
    }
    // Unknown — zero-cost fallback keeps the meter honest instead of
    // fabricating a number.
    TextPricing {
        input_per_million_usd: 0.0,
        output_per_million_usd: 0.0,
        cached_input_per_million_usd: 0.0,
    }
}

/// Image-model pricing. The published rate for `gemini-2.5-flash-image` is
/// $0.039 per generated image (~1290 output tokens at $30/M).
pub(super) fn image_pricing(model: &str) -> ImagePricing {
    let m = model.to_ascii_lowercase();
    if m.contains("flash-image") || m.contains("nano-banana") {
        return ImagePricing { per_image_usd: 0.039 };
    }
    // `imagen-4.0-generate-*`: $0.04 per image (1:1, standard quality).
    if m.contains("imagen") {
        return ImagePricing { per_image_usd: 0.04 };
    }
    ImagePricing { per_image_usd: 0.0 }
}

/// Convert usage to an estimated USD cost under the given model's prices.
/// Cached tokens are treated as a separate line at the reduced rate; the
/// balance is billed at the full input rate.
pub(super) fn cost_text(usage: &Usage, price: TextPricing) -> f64 {
    let cached = usage.cached_tokens as f64;
    let uncached_input = (usage.prompt_tokens as f64 - cached).max(0.0);
    let output = usage.response_tokens as f64;
    uncached_input * price.input_per_million_usd / 1_000_000.0
        + cached * price.cached_input_per_million_usd / 1_000_000.0
        + output * price.output_per_million_usd / 1_000_000.0
}

pub(super) fn cost_images(count: u32, price: ImagePricing) -> f64 {
    count as f64 * price.per_image_usd
}

/// Format a USD amount for the session footer. Two decimals for amounts
/// ≥ $0.01, four for finer-grained totals so "1 token" rounds to something
/// more useful than "$0.00".
pub(super) fn format_usd(v: f64) -> String {
    if v >= 0.01 {
        format!("${v:.2}")
    } else if v > 0.0 {
        format!("${v:.4}")
    } else {
        "$0.00".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_pricing_matches_published_rates() {
        let p = text_pricing("gemini-pro-latest");
        assert!((p.input_per_million_usd - 1.25).abs() < 1e-9);
        assert!((p.output_per_million_usd - 10.00).abs() < 1e-9);
    }

    #[test]
    fn flash_pricing_matches_published_rates() {
        let p = text_pricing("gemini-2.5-flash");
        assert!((p.input_per_million_usd - 0.30).abs() < 1e-9);
        assert!((p.output_per_million_usd - 2.50).abs() < 1e-9);
    }

    #[test]
    fn cost_uses_cached_rate_for_cached_portion() {
        let mut usage = Usage::default();
        usage.prompt_tokens = 1_000_000;
        usage.cached_tokens = 500_000;
        usage.response_tokens = 0;
        let price = text_pricing("gemini-pro-latest");
        // 500k @ $1.25/M + 500k @ $0.31/M = 0.625 + 0.155 = 0.78
        let cost = cost_text(&usage, price);
        assert!((cost - 0.78).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn image_pricing_for_flash_image() {
        let p = image_pricing("gemini-2.5-flash-image");
        assert!((p.per_image_usd - 0.039).abs() < 1e-9);
    }
}
