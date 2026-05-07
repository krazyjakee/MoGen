//! Public Gemini pricing applied client-side so the studio can show a
//! running cost estimate. Rates are the published US list prices for the
//! model family and are approximate — the authoritative source is the
//! user's Google Cloud invoice.
//!
//! Rates are per million tokens. Input/output counts come from the API's
//! `usageMetadata`; `cached` is billed at a reduced rate when a
//! `cachedContents` resource is attached. The 2.5 Pro and 3.x Pro tiers
//! switch to a higher "long context" rate once the prompt crosses 200k
//! tokens — see [`LONG_CONTEXT_THRESHOLD`].

use mogen_llm::gemini::Usage;

/// Prompt size at which Pro-tier pricing flips from the short-context rate
/// to the long-context rate. Google bills the entire request at the long
/// rate once the input crosses this threshold.
pub(super) const LONG_CONTEXT_THRESHOLD: u32 = 200_000;

#[derive(Debug, Clone, Copy)]
pub(super) struct TextPricing {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
    pub cached_input_per_million_usd: f64,
    /// Long-context (>200k prompt) input rate. Equal to the short-context
    /// rate for models without a tier split.
    pub input_per_million_usd_long: f64,
    pub output_per_million_usd_long: f64,
    pub cached_input_per_million_usd_long: f64,
}

impl TextPricing {
    /// True when the model has a separate >200k prompt tier.
    pub(super) fn is_tiered(&self) -> bool {
        (self.input_per_million_usd - self.input_per_million_usd_long).abs() > f64::EPSILON
    }

    fn flat(input: f64, output: f64, cached: f64) -> Self {
        Self {
            input_per_million_usd: input,
            output_per_million_usd: output,
            cached_input_per_million_usd: cached,
            input_per_million_usd_long: input,
            output_per_million_usd_long: output,
            cached_input_per_million_usd_long: cached,
        }
    }

    fn tiered(short: (f64, f64, f64), long: (f64, f64, f64)) -> Self {
        Self {
            input_per_million_usd: short.0,
            output_per_million_usd: short.1,
            cached_input_per_million_usd: short.2,
            input_per_million_usd_long: long.0,
            output_per_million_usd_long: long.1,
            cached_input_per_million_usd_long: long.2,
        }
    }
}

/// Per-image flat cost for an image-gen model. The image API does not
/// report token usage, so we charge a constant per successful call.
#[derive(Debug, Clone, Copy)]
pub(super) struct ImagePricing {
    pub per_image_usd: f64,
}

/// Match a model name against the price table. More-specific (Gemini 3.x)
/// matchers run first so 2.5 prefix matches don't shadow them.
pub(super) fn text_pricing(model: &str) -> TextPricing {
    let m = model.to_ascii_lowercase();

    // --- Gemini 3.x (preview) — checked before 2.5 because the prefix
    // `gemini-3-…` does not match the 2.5 matchers below, but we want the
    // ordering explicit so a future `gemini-3-pro-latest` alias keeps
    // landing on the right row.
    if m.contains("3.1-pro") || m.contains("3-pro") {
        // Gemini 3.1 Pro Preview, tiered at 200k.
        return TextPricing::tiered(
            (2.00, 12.00, 0.50),
            (4.00, 18.00, 1.00),
        );
    }
    if m.contains("3.1-flash-lite") || m.contains("3-flash-lite") {
        return TextPricing::flat(0.25, 1.50, 0.025);
    }
    if m.contains("3.1-flash") || m.contains("3-flash") {
        // Gemini 3 / 3.1 Flash Preview.
        return TextPricing::flat(0.50, 3.00, 0.05);
    }

    // --- Gemini 2.5 Pro (tiered at 200k).
    if m.starts_with("gemini-pro") || m.contains("2.5-pro") || m.contains("2-5-pro") {
        return TextPricing::tiered(
            (1.25, 10.00, 0.125),
            (2.50, 15.00, 0.25),
        );
    }
    // --- Gemini 2.5 Flash-Lite (cheapest text tier).
    if m.contains("flash-lite") {
        return TextPricing::flat(0.10, 0.40, 0.01);
    }
    // --- Gemini 2.5 Flash / flash-latest.
    if m.starts_with("gemini-flash") || m.contains("2.5-flash") || m.contains("2-5-flash") {
        return TextPricing::flat(0.30, 2.50, 0.03);
    }

    // Unknown — zero-cost fallback keeps the meter honest instead of
    // fabricating a number.
    TextPricing::flat(0.0, 0.0, 0.0)
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
/// Once the prompt crosses [`LONG_CONTEXT_THRESHOLD`] the entire request
/// is billed at the long-context rate, matching Google's tier behaviour.
pub(super) fn cost_text(usage: &Usage, price: TextPricing) -> f64 {
    let long = usage.prompt_tokens > LONG_CONTEXT_THRESHOLD;
    let (in_rate, out_rate, cache_rate) = if long {
        (
            price.input_per_million_usd_long,
            price.output_per_million_usd_long,
            price.cached_input_per_million_usd_long,
        )
    } else {
        (
            price.input_per_million_usd,
            price.output_per_million_usd,
            price.cached_input_per_million_usd,
        )
    };
    let cached = usage.cached_tokens as f64;
    let uncached_input = (usage.prompt_tokens as f64 - cached).max(0.0);
    let output = usage.response_tokens as f64;
    uncached_input * in_rate / 1_000_000.0
        + cached * cache_rate / 1_000_000.0
        + output * out_rate / 1_000_000.0
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

/// Format a per-million-tokens rate compactly for the prefs UI. `$1.25/M`
/// reads better than `$1.25 per million tokens` in a tight column.
pub(super) fn format_per_million(rate: f64) -> String {
    if rate <= 0.0 {
        "—".to_string()
    } else if rate < 1.0 {
        format!("${rate:.3}/M")
    } else {
        format!("${rate:.2}/M")
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
        assert!((p.cached_input_per_million_usd - 0.125).abs() < 1e-9);
        // >200k tier doubles input/cached and steps output to $15.
        assert!((p.input_per_million_usd_long - 2.50).abs() < 1e-9);
        assert!((p.output_per_million_usd_long - 15.00).abs() < 1e-9);
        assert!(p.is_tiered());
    }

    #[test]
    fn flash_pricing_matches_published_rates() {
        let p = text_pricing("gemini-2.5-flash");
        assert!((p.input_per_million_usd - 0.30).abs() < 1e-9);
        assert!((p.output_per_million_usd - 2.50).abs() < 1e-9);
        assert!((p.cached_input_per_million_usd - 0.03).abs() < 1e-9);
        assert!(!p.is_tiered());
    }

    #[test]
    fn flash_lite_pricing_matches_published_rates() {
        let p = text_pricing("gemini-2.5-flash-lite");
        assert!((p.input_per_million_usd - 0.10).abs() < 1e-9);
        assert!((p.output_per_million_usd - 0.40).abs() < 1e-9);
        assert!((p.cached_input_per_million_usd - 0.01).abs() < 1e-9);
    }

    #[test]
    fn gemini_3_pro_preview_pricing() {
        let p = text_pricing("gemini-3.1-pro-preview");
        assert!((p.input_per_million_usd - 2.00).abs() < 1e-9);
        assert!((p.output_per_million_usd - 12.00).abs() < 1e-9);
        assert!((p.input_per_million_usd_long - 4.00).abs() < 1e-9);
        assert!((p.output_per_million_usd_long - 18.00).abs() < 1e-9);
        assert!(p.is_tiered());
    }

    #[test]
    fn gemini_3_flash_preview_pricing() {
        let p = text_pricing("gemini-3-flash-preview");
        assert!((p.input_per_million_usd - 0.50).abs() < 1e-9);
        assert!((p.output_per_million_usd - 3.00).abs() < 1e-9);
    }

    #[test]
    fn cost_uses_cached_rate_for_cached_portion() {
        let mut usage = Usage::default();
        usage.prompt_tokens = 100_000;
        usage.cached_tokens = 50_000;
        usage.response_tokens = 0;
        let price = text_pricing("gemini-pro-latest");
        // 50k @ $1.25/M + 50k @ $0.125/M = 0.0625 + 0.00625 = 0.06875
        let cost = cost_text(&usage, price);
        assert!((cost - 0.06875).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn cost_switches_to_long_tier_above_200k() {
        let mut usage = Usage::default();
        usage.prompt_tokens = 250_000;
        usage.cached_tokens = 0;
        usage.response_tokens = 10_000;
        let price = text_pricing("gemini-pro-latest");
        // 250k @ $2.50/M + 10k @ $15/M = 0.625 + 0.15 = 0.775
        let cost = cost_text(&usage, price);
        assert!((cost - 0.775).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn image_pricing_for_flash_image() {
        let p = image_pricing("gemini-2.5-flash-image");
        assert!((p.per_image_usd - 0.039).abs() < 1e-9);
    }
}
