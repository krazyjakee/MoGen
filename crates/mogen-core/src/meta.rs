use serde::{Deserialize, Serialize};

/// Top-of-file metadata block extracted from an optional `meta(...)` node.
///
/// All fields are optional; the `meta(...)` node itself is optional. The DSL
/// layer extracts this from the AST before lowering and the lowering pass
/// stashes it on `SceneGraph` so tooling (Studio inspector, glTF extras)
/// can read it without re-parsing.
///
/// `mogen_version` is auto-stamped from `CARGO_PKG_VERSION` whenever the CLI
/// or Studio writes a `.mog` file, so it always reflects the toolchain that
/// last touched the source. `seed`, `thinking`, and `prompt` are written by
/// the LLM commands (`generate` / `modify` / `repair` / `animate`) so future
/// runs can reproduce the original call without re-supplying flags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mogen_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// RNG seed used for the LLM call that produced this file. Stored as a
    /// string in the DSL because nanosecond timestamps overflow `f32`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Per-file Gemini thinking budget label (`low`/`medium`/`high`/`xhigh`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Original natural-language prompt the file was generated from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Visual-style hint stamped by `mogen-llm` when the user picks one
    /// from the CLI flag or Studio dropdown. Free-form string in this
    /// layer (the validator and DSL extractor are deliberately lenient
    /// so hand-edited files with experimental keys still load); the
    /// LLM crate decides how to map it onto its `Style` enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}
