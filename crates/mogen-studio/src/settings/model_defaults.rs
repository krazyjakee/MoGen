//! Combobox labels / option lists for the thinking-budget and style
//! pickers. Pure presentation helpers keyed off `mogen_llm` enums, kept
//! out of `settings.rs` so the persisted-config surface stays readable.

use mogen_llm::{Style, ThinkingLevel, STYLES};

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
