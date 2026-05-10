//! Worker that asks the fast model to fill in the `meta(name, description,
//! tags)` block from the current DSL source. Mirrors the shape of
//! `run_prompt_enhance`: minimal system instruction, low thinking budget, JSON
//! response parsed manually because no provider in this crate exposes
//! structured-output mode yet.

use mogen_llm::{GenerateConfig, Provider, ThinkingLevel};

use super::llm::{build_provider_client, Credential};

/// Filled-in fields returned by [`run_meta_generate`]. Fields the model
/// declined to produce (or whose JSON shape was wrong) come back as defaults
/// so the caller can leave the existing meta value alone.
#[derive(Debug, Default, Clone)]
pub(in crate::app) struct MetaSuggestion {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// Single Flash-tier call that summarises `source` into `name`,
/// `description`, and a short tag list. Returns a [`MetaSuggestion`] on
/// success or a human-readable error string on failure.
pub(in crate::app) fn run_meta_generate(
    source: String,
    provider: Provider,
    credential: Credential,
    model: String,
    claude_code_path: String,
    zai_base_url: String,
) -> Result<MetaSuggestion, String> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err("nothing to summarise — load or generate a scene first".into());
    }
    let client = build_provider_client(provider, credential, &claude_code_path, &zai_base_url);

    let user = format!(
        "You are summarising a MoGen DSL scene file for a model registry.\n\
         Reply with ONE JSON object on a single line, no markdown fences, no \
         commentary, with EXACTLY these keys:\n\
         {{\"name\": \"snake_case_slug\", \"description\": \"one short sentence\", \
         \"tags\": [\"tag1\", \"tag2\", \"tag3\"]}}\n\n\
         Rules:\n\
         - `name`: a short snake_case identifier for the asset (e.g. \
         \"wooden_chair\", \"plate_armor_knight\"). Lowercase, ASCII, words \
         joined by underscores. No spaces.\n\
         - `description`: one concise sentence describing what the scene is. \
         No leading label, no period required.\n\
         - `tags`: 3 to 6 lowercase comma-separable keywords describing the \
         asset's category, material, style, or domain. Single words or short \
         hyphen-joined phrases.\n\
         - Do NOT invent details that aren't present in the DSL — infer only \
         from node names, primitives, materials, and the existing \
         `meta(prompt=...)` if any.\n\n\
         DSL:\n```\n{trimmed}\n```",
    );

    let mut cfg = GenerateConfig::new(user);
    cfg.model = model;
    cfg.thinking_level = Some(ThinkingLevel::Low);
    cfg.temperature = Some(0.4);

    let resp = client
        .generate(&cfg)
        .map_err(|e| format!("{e}"))?;

    parse_meta_json(&resp.text).ok_or_else(|| {
        format!(
            "could not parse meta JSON from {} response",
            provider.label()
        )
    })
}

/// Best-effort parser for the model's reply. Strips optional ```json fences
/// and locates the first balanced `{...}` block before delegating to
/// `serde_json`.
fn parse_meta_json(raw: &str) -> Option<MetaSuggestion> {
    let cleaned = raw.trim();
    let cleaned = cleaned
        .strip_prefix("```")
        .map(|s| s.trim_start_matches(char::is_alphanumeric).trim_start())
        .unwrap_or(cleaned)
        .trim_end_matches("```")
        .trim();
    let start = cleaned.find('{')?;
    let end = cleaned.rfind('}')?;
    if end <= start {
        return None;
    }
    let json = &cleaned[start..=end];
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = value.as_object()?;
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let description = obj
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let tags = obj
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if name.is_empty() && description.is_empty() && tags.is_empty() {
        return None;
    }
    Some(MetaSuggestion {
        name,
        description,
        tags,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json() {
        let s = r#"{"name":"chair","description":"a chair","tags":["wood","seat"]}"#;
        let m = parse_meta_json(s).unwrap();
        assert_eq!(m.name, "chair");
        assert_eq!(m.description, "a chair");
        assert_eq!(m.tags, vec!["wood".to_string(), "seat".to_string()]);
    }

    #[test]
    fn strips_code_fences() {
        let s = "```json\n{\"name\":\"x\",\"description\":\"\",\"tags\":[]}\n```";
        let m = parse_meta_json(s).unwrap();
        assert_eq!(m.name, "x");
    }

    #[test]
    fn ignores_leading_chatter() {
        let s = "Sure! Here you go:\n{\"name\":\"y\",\"description\":\"d\",\"tags\":[\"a\"]}";
        let m = parse_meta_json(s).unwrap();
        assert_eq!(m.name, "y");
        assert_eq!(m.tags, vec!["a".to_string()]);
    }

    #[test]
    fn rejects_empty_object() {
        assert!(parse_meta_json("{}").is_none());
    }

    #[test]
    fn lowercases_and_trims_tags() {
        let s = r#"{"name":"x","description":"","tags":["  Wood  ","Chair "]}"#;
        let m = parse_meta_json(s).unwrap();
        assert_eq!(m.tags, vec!["wood".to_string(), "chair".to_string()]);
    }
}
