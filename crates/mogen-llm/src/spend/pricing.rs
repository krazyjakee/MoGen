//! Pricing tables for spend computation.
//!
//! Rates are denominated **per million tokens** so they line up with the
//! provider's published pricing pages. The `pricing` SQLite table stores
//! them effective-dated — a [`PricingRow`] applies between
//! `effective_from` and (exclusive) `effective_to`. Editing today's price
//! sets a fresh `effective_from` on a new row rather than overwriting the
//! historical one, so a year-old [`CallRecord`] keeps billing at the
//! year-old rate.
//!
//! [`SEED`] is the baseline shipped on first run. Studio's Settings →
//! AI Pricing lets the user override individual rows; the seed is then
//! marked superseded (a fresh `effective_to`) and a new row carries the
//! user's number forward.

use crate::types::Usage;

/// Prompt-size threshold where some Pro models flip to a higher tier.
/// Matches Google's published 200k cut-off. Stored on the [`TextPricing`]
/// row so each model can declare its own threshold if needed (zero means
/// "no tier flip").
pub const DEFAULT_LONG_CONTEXT_THRESHOLD: u32 = 200_000;

/// Rates for one text model. Per-million-tokens, USD.
///
/// Tiered models (Gemini Pro family) carry distinct short- vs long-context
/// rates; flat-priced models leave the `_long` fields equal to the
/// short-context rates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cached_input_per_mtok: f64,
    pub input_per_mtok_long: f64,
    pub output_per_mtok_long: f64,
    pub cached_input_per_mtok_long: f64,
    /// Prompt-size threshold for long-context billing. `0` means "no
    /// tiered pricing" — both short/long rates are identical and the
    /// threshold is unused.
    pub long_context_threshold: u32,
}

/// Per-image flat rate for an image-gen model. Image surfaces don't
/// report token counts, so cost is `count * per_image_usd`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImagePricing {
    pub per_image_usd: f64,
}

/// Which tier was used when costing a call. Surfaced on the per-call
/// detail row in the Spending panel so users can spot tier flips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PriceTier {
    Short,
    Long,
}

impl TextPricing {
    pub const fn flat(input: f64, output: f64, cached: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            cached_input_per_mtok: cached,
            input_per_mtok_long: input,
            output_per_mtok_long: output,
            cached_input_per_mtok_long: cached,
            long_context_threshold: 0,
        }
    }

    pub const fn tiered(
        threshold: u32,
        short: (f64, f64, f64),
        long: (f64, f64, f64),
    ) -> Self {
        Self {
            input_per_mtok: short.0,
            output_per_mtok: short.1,
            cached_input_per_mtok: short.2,
            input_per_mtok_long: long.0,
            output_per_mtok_long: long.1,
            cached_input_per_mtok_long: long.2,
            long_context_threshold: threshold,
        }
    }

    /// True when the model has a separate >threshold prompt tier.
    pub fn is_tiered(&self) -> bool {
        self.long_context_threshold > 0
            && (self.input_per_mtok - self.input_per_mtok_long).abs() > f64::EPSILON
    }

    /// Pick the tier that applies to a request with `prompt_tokens`. Models
    /// without a tier always return [`PriceTier::Short`].
    pub fn tier_for(&self, prompt_tokens: u32) -> PriceTier {
        if self.long_context_threshold > 0 && prompt_tokens > self.long_context_threshold {
            PriceTier::Long
        } else {
            PriceTier::Short
        }
    }
}

/// Combined seed row used to populate the `pricing` table on first run
/// (and for fallback lookups when the DB is unavailable).
#[derive(Debug, Clone, Copy)]
pub struct PricingSeed {
    pub provider: &'static str,
    pub model: &'static str,
    /// At least one of these is `Some`. Text-only models leave `image`
    /// `None`; image-only models leave `text` `None`.
    pub text: Option<TextPricing>,
    pub image: Option<ImagePricing>,
}

/// Compute USD cost for a [`Usage`] under `price`. Once `usage.prompt_tokens`
/// crosses [`TextPricing::long_context_threshold`] the whole request bills
/// at the long-context rate, matching how Google bills the Pro family.
///
/// Returns `0.0` for unknown / zero-rate models so the meter stays honest
/// instead of fabricating a number when the user runs an Ollama local model.
pub fn compute_cost(usage: &Usage, price: TextPricing) -> f64 {
    let (in_rate, out_rate, cache_rate) = match price.tier_for(usage.prompt_tokens) {
        PriceTier::Short => (
            price.input_per_mtok,
            price.output_per_mtok,
            price.cached_input_per_mtok,
        ),
        PriceTier::Long => (
            price.input_per_mtok_long,
            price.output_per_mtok_long,
            price.cached_input_per_mtok_long,
        ),
    };
    let cached = usage.cached_tokens as f64;
    let uncached_input = (usage.prompt_tokens as f64 - cached).max(0.0);
    let output = usage.response_tokens as f64;
    uncached_input * in_rate / 1_000_000.0
        + cached * cache_rate / 1_000_000.0
        + output * out_rate / 1_000_000.0
}

/// Best-effort lookup against [`SEED`] when the DB is unavailable. Matches
/// model ids loosely against the seed catalogue — see comments in [`SEED`]
/// for the precedence order. Returns the zero-rate row when nothing
/// matches so an unknown / hand-edited model id still records correctly.
pub fn text_price_for_model(model: &str) -> TextPricing {
    let m = model.to_ascii_lowercase();

    // --- Gemini 3.x preview family — more-specific tags first.
    if m.contains("3.5-flash") {
        return TextPricing::flat(1.50, 9.00, 0.15);
    }
    if m.contains("3.1-flash-lite") || m.contains("3-flash-lite") {
        return TextPricing::flat(0.25, 1.50, 0.025);
    }
    if m.contains("3.1-flash") || m.contains("3-flash") {
        return TextPricing::flat(0.50, 3.00, 0.05);
    }
    if m.contains("3.1-pro") || m.contains("3-pro") {
        return TextPricing::tiered(
            DEFAULT_LONG_CONTEXT_THRESHOLD,
            (2.00, 12.00, 0.50),
            (4.00, 18.00, 1.00),
        );
    }

    // --- Gemini 2.5 family.
    if m.starts_with("gemini-pro") || m.contains("2.5-pro") || m.contains("2-5-pro") {
        return TextPricing::tiered(
            DEFAULT_LONG_CONTEXT_THRESHOLD,
            (1.25, 10.00, 0.125),
            (2.50, 15.00, 0.25),
        );
    }
    if m.contains("flash-lite") {
        return TextPricing::flat(0.10, 0.40, 0.01);
    }
    if m.starts_with("gemini-flash") || m.contains("2.5-flash") || m.contains("2-5-flash") {
        return TextPricing::flat(0.30, 2.50, 0.03);
    }

    // --- OpenAI GPT family. Conservative defaults from published rates.
    if m.contains("gpt-5") {
        return TextPricing::flat(2.50, 10.00, 1.25);
    }
    if m.contains("gpt-4o-mini") {
        return TextPricing::flat(0.15, 0.60, 0.075);
    }
    if m.contains("gpt-4o") || m.contains("gpt-4.1") {
        return TextPricing::flat(2.50, 10.00, 1.25);
    }
    if m.contains("o3-mini") {
        return TextPricing::flat(1.10, 4.40, 0.55);
    }
    if m.contains("o3") {
        return TextPricing::flat(2.00, 8.00, 1.00);
    }

    // --- Anthropic Claude family.
    if m.contains("opus-4") || m.contains("opus-3.5") {
        return TextPricing::flat(15.00, 75.00, 1.50);
    }
    if m.contains("sonnet-4") || m.contains("sonnet-3.5") || m.contains("sonnet-3.7") {
        return TextPricing::flat(3.00, 15.00, 0.30);
    }
    if m.contains("haiku-4") || m.contains("haiku-3.5") {
        return TextPricing::flat(0.80, 4.00, 0.08);
    }

    // --- Fireworks (Kimi K2 Fire Pass is free for personal agentic use).
    if m.contains("kimi-k2p6-turbo") || m.contains("kimi-k2p6") {
        return TextPricing::flat(0.0, 0.0, 0.0);
    }

    // --- Z.ai GLM family.
    if m.contains("glm-5") {
        return TextPricing::flat(0.50, 1.50, 0.10);
    }

    // --- Local / unknown — bill at zero so the meter stays honest.
    TextPricing::flat(0.0, 0.0, 0.0)
}

/// Image-model pricing fallback when the DB is unavailable.
pub fn image_price_for_model(model: &str) -> ImagePricing {
    let m = model.to_ascii_lowercase();
    if m.contains("flash-image") || m.contains("nano-banana") {
        return ImagePricing { per_image_usd: 0.039 };
    }
    if m.contains("imagen") {
        return ImagePricing { per_image_usd: 0.04 };
    }
    if m.contains("glm-image") {
        return ImagePricing { per_image_usd: 0.03 };
    }
    ImagePricing { per_image_usd: 0.0 }
}

/// Wrapper that returns both text and image rates in one call — used by
/// the SQLite backend when costing a record before insert.
pub fn default_pricing_for_model(provider: &str, model: &str) -> PricingSeed {
    PricingSeed {
        provider: provider_key_static(provider),
        model: model_key_static(model),
        text: Some(text_price_for_model(model)),
        image: Some(image_price_for_model(model)),
    }
}

fn provider_key_static(p: &str) -> &'static str {
    match p.to_ascii_lowercase().as_str() {
        "gemini" => "gemini",
        "openai" => "openai",
        "anthropic" => "anthropic",
        "ollama" => "ollama",
        "claude-code" => "claude-code",
        "fireworks" => "fireworks",
        "zai" => "zai",
        _ => "other",
    }
}

fn model_key_static(_m: &str) -> &'static str {
    // The model string isn't `'static` from the caller, so this helper just
    // returns a placeholder — used only by the seed-row builder where the
    // string is already a literal. The DB row stores the real model name.
    ""
}

/// Baseline pricing seeded into the `pricing` table on first run. Each row
/// captures the public list price for a model `mogen-llm` talks to today.
/// Effective-dated: editing a row from the Studio inserts a new row with a
/// later `effective_from` and stamps `effective_to` on the previous row,
/// so historical [`CallRecord`]s keep billing at the rate they paid.
pub const SEED: &[PricingSeed] = &[
    // --- Gemini text.
    PricingSeed {
        provider: "gemini",
        model: "gemini-pro-latest",
        text: Some(TextPricing {
            input_per_mtok: 1.25,
            output_per_mtok: 10.00,
            cached_input_per_mtok: 0.125,
            input_per_mtok_long: 2.50,
            output_per_mtok_long: 15.00,
            cached_input_per_mtok_long: 0.25,
            long_context_threshold: DEFAULT_LONG_CONTEXT_THRESHOLD,
        }),
        image: None,
    },
    PricingSeed {
        provider: "gemini",
        model: "gemini-flash-latest",
        text: Some(TextPricing {
            input_per_mtok: 0.30,
            output_per_mtok: 2.50,
            cached_input_per_mtok: 0.03,
            input_per_mtok_long: 0.30,
            output_per_mtok_long: 2.50,
            cached_input_per_mtok_long: 0.03,
            long_context_threshold: 0,
        }),
        image: None,
    },
    PricingSeed {
        provider: "gemini",
        model: "gemini-3.1-pro-preview",
        text: Some(TextPricing {
            input_per_mtok: 2.00,
            output_per_mtok: 12.00,
            cached_input_per_mtok: 0.50,
            input_per_mtok_long: 4.00,
            output_per_mtok_long: 18.00,
            cached_input_per_mtok_long: 1.00,
            long_context_threshold: DEFAULT_LONG_CONTEXT_THRESHOLD,
        }),
        image: None,
    },
    PricingSeed {
        provider: "gemini",
        model: "gemini-3-flash-preview",
        text: Some(TextPricing {
            input_per_mtok: 0.50,
            output_per_mtok: 3.00,
            cached_input_per_mtok: 0.05,
            input_per_mtok_long: 0.50,
            output_per_mtok_long: 3.00,
            cached_input_per_mtok_long: 0.05,
            long_context_threshold: 0,
        }),
        image: None,
    },
    PricingSeed {
        provider: "gemini",
        model: "gemini-2.5-flash-lite",
        text: Some(TextPricing {
            input_per_mtok: 0.10,
            output_per_mtok: 0.40,
            cached_input_per_mtok: 0.01,
            input_per_mtok_long: 0.10,
            output_per_mtok_long: 0.40,
            cached_input_per_mtok_long: 0.01,
            long_context_threshold: 0,
        }),
        image: None,
    },
    // --- Gemini image.
    PricingSeed {
        provider: "gemini",
        model: "gemini-2.5-flash-image",
        text: None,
        image: Some(ImagePricing { per_image_usd: 0.039 }),
    },
    PricingSeed {
        provider: "gemini",
        model: "imagen-4.0-generate-001",
        text: None,
        image: Some(ImagePricing { per_image_usd: 0.04 }),
    },
    // --- OpenAI.
    PricingSeed {
        provider: "openai",
        model: "gpt-5",
        text: Some(TextPricing::flat(2.50, 10.00, 1.25)),
        image: None,
    },
    PricingSeed {
        provider: "openai",
        model: "gpt-4o-mini",
        text: Some(TextPricing::flat(0.15, 0.60, 0.075)),
        image: None,
    },
    PricingSeed {
        provider: "openai",
        model: "gpt-4o",
        text: Some(TextPricing::flat(2.50, 10.00, 1.25)),
        image: None,
    },
    PricingSeed {
        provider: "openai",
        model: "o3",
        text: Some(TextPricing::flat(2.00, 8.00, 1.00)),
        image: None,
    },
    // --- Anthropic.
    PricingSeed {
        provider: "anthropic",
        model: "claude-sonnet-4",
        text: Some(TextPricing::flat(3.00, 15.00, 0.30)),
        image: None,
    },
    PricingSeed {
        provider: "anthropic",
        model: "claude-opus-4",
        text: Some(TextPricing::flat(15.00, 75.00, 1.50)),
        image: None,
    },
    PricingSeed {
        provider: "anthropic",
        model: "claude-haiku-4-5",
        text: Some(TextPricing::flat(0.80, 4.00, 0.08)),
        image: None,
    },
    // --- Fireworks Fire Pass — zero per-token cost for personal use.
    PricingSeed {
        provider: "fireworks",
        model: "kimi-k2p6",
        text: Some(TextPricing::flat(0.0, 0.0, 0.0)),
        image: None,
    },
    PricingSeed {
        provider: "fireworks",
        model: "kimi-k2p6-turbo",
        text: Some(TextPricing::flat(0.0, 0.0, 0.0)),
        image: None,
    },
    // --- Z.ai GLM.
    PricingSeed {
        provider: "zai",
        model: "glm-5.1",
        text: Some(TextPricing::flat(0.50, 1.50, 0.10)),
        image: None,
    },
    PricingSeed {
        provider: "zai",
        model: "glm-image",
        text: None,
        image: Some(ImagePricing { per_image_usd: 0.03 }),
    },
    // --- Ollama (local). Always zero.
    PricingSeed {
        provider: "ollama",
        model: "llama3",
        text: Some(TextPricing::flat(0.0, 0.0, 0.0)),
        image: None,
    },
    // --- Claude Code (user's subscription pays — no per-call billing).
    PricingSeed {
        provider: "claude-code",
        model: "claude-sonnet-4",
        text: Some(TextPricing::flat(0.0, 0.0, 0.0)),
        image: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_pricing_matches_published_rates() {
        let p = text_price_for_model("gemini-pro-latest");
        assert!((p.input_per_mtok - 1.25).abs() < 1e-9);
        assert!((p.output_per_mtok - 10.00).abs() < 1e-9);
        assert!((p.cached_input_per_mtok - 0.125).abs() < 1e-9);
        assert!(p.is_tiered());
    }

    #[test]
    fn flash_pricing_is_flat() {
        let p = text_price_for_model("gemini-flash-latest");
        assert!(!p.is_tiered());
        assert!((p.input_per_mtok - 0.30).abs() < 1e-9);
    }

    #[test]
    fn cost_switches_to_long_tier_above_threshold() {
        let mut usage = Usage::default();
        usage.prompt_tokens = 250_000;
        usage.response_tokens = 10_000;
        let price = text_price_for_model("gemini-pro-latest");
        // 250k @ $2.50/M + 10k @ $15/M = 0.625 + 0.15 = 0.775
        let cost = compute_cost(&usage, price);
        assert!((cost - 0.775).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn cost_uses_cached_rate_for_cached_portion() {
        let mut usage = Usage::default();
        usage.prompt_tokens = 100_000;
        usage.cached_tokens = 50_000;
        let price = text_price_for_model("gemini-pro-latest");
        let cost = compute_cost(&usage, price);
        // 50k @ $1.25/M + 50k @ $0.125/M = 0.0625 + 0.00625 = 0.06875
        assert!((cost - 0.06875).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn image_price_for_nano_banana() {
        let p = image_price_for_model("gemini-2.5-flash-image");
        assert!((p.per_image_usd - 0.039).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_costs_nothing() {
        let p = text_price_for_model("homebrewed-llama");
        assert_eq!(p, TextPricing::flat(0.0, 0.0, 0.0));
    }

    #[test]
    fn seed_table_is_non_empty_and_covers_known_models() {
        assert!(SEED.iter().any(|r| r.model == "gemini-pro-latest"));
        assert!(SEED.iter().any(|r| r.model == "gemini-2.5-flash-image"));
        assert!(SEED.iter().any(|r| r.provider == "openai"));
    }
}
