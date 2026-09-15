//! Version 1 text protocol. Native adapters may transport the same JSON schema.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub findings: String,
    pub complete: bool,
    pub improved: bool,
    pub correction: String,
}
pub fn review_schema() -> Value {
    json!({"type":"object","additionalProperties":false,
        "required":["findings","complete","improved","correction"],
        "properties":{"findings":{"type":"string"},"complete":{"type":"boolean"},
        "improved":{"type":"boolean"},"correction":{"type":"string"}}})
}
pub fn review_instructions() -> String {
    format!("Review a 3D asset against the original brief and target references. Assess silhouette/proportions, required parts, joints/negative space, geometry finish, materials/UV scale and reference/style fidelity. Findings must describe observed defects and uncertainty. Compilation or confidence alone is not quality. Set improved false for regressions or uncertainty. Return only one JSON object conforming to review schema v1: {}. Typed example: {{\"findings\":\"Wheel arch has shading artifacts.\",\"complete\":false,\"improved\":false,\"correction\":\"Inspect body shading.\"}}", review_schema())
}
/// Only whole-document fences and a string-array findings value are lossless
/// compatibility forms. Missing judgments, string booleans and extras fail.
pub fn parse_review(raw: &str) -> Result<(Review, String)> {
    if raw.len() > MAX_RESPONSE_BYTES {
        bail!("Review exceeds 1 MiB protocol limit");
    }
    let text = document_text(raw, "json");
    let mut value: Value = serde_json::from_str(text)?;
    let mut provenance = if text == raw.trim() {
        "canonical"
    } else {
        "removed JSON fence"
    }
    .to_string();
    if let Some(Value::Array(items)) = value.get("findings") {
        let strings: Option<Vec<_>> = items.iter().map(Value::as_str).collect();
        let strings =
            strings.ok_or_else(|| anyhow::anyhow!("findings must contain only strings"))?;
        value["findings"] = Value::String(strings.join("\n"));
        provenance.push_str("; joined findings strings with newline");
    }
    Ok((serde_json::from_value(value)?, provenance))
}
/// A format repair may drop extraneous keys, but cannot create or change a
/// judgment. Unparseable/truncated evidence remains saved for human recovery.
pub fn validate_review_repair(original: &str, repaired: &Review) -> Result<()> {
    let mut value:Value=serde_json::from_str(document_text(original,"json"))
        .map_err(|_|anyhow::anyhow!("Original review is truncated/unparseable; judgments cannot be verified without a new review"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Original review must be an object"))?;
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "findings" | "complete" | "improved" | "correction"
        )
    });
    let (expected, _) = parse_review(&serde_json::to_string(&value)?)?;
    if serde_json::to_value(expected)? != serde_json::to_value(repaired)? {
        bail!("Format repair changed original observations or judgments");
    }
    Ok(())
}
pub fn document_text<'a>(raw: &'a str, language: &str) -> &'a str {
    let text = raw.trim();
    for prefix in [format!("```{language}\n"), "```\n".into()] {
        if let Some(inner) = text
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix("```"))
        {
            if !inner.contains("```") {
                return inner.trim();
            }
        }
    }
    text
}
pub fn parse_tool(raw: &str, request_revision: &str) -> Result<(super::ModelingTool, String)> {
    if raw.len() > MAX_RESPONSE_BYTES {
        bail!("Tool response exceeds 1 MiB protocol limit");
    }
    let json_text = document_text(raw, "json");
    match serde_json::from_str(json_text) {
        Ok(tool) => Ok((tool, "tool JSON".into())),
        Err(error) => {
            let source = document_text(raw, "mog");
            // Parsing the entire document (never extracting a code block from
            // prose) plus Apply's compilation/locks is the recovery boundary.
            if !source.starts_with('{')
                && mogen_dsl::parse(source).is_ok_and(|nodes| {
                    !nodes.is_empty()
                        && nodes
                            .iter()
                            .all(|n| mogen_validate::KNOWN_KINDS.contains(&n.kind.as_str()))
                })
            {
                Ok((
                    super::ModelingTool::Apply {
                        revision: request_revision.into(),
                        edits: source.into(),
                    },
                    "raw DSL staged as full-source Apply".into(),
                ))
            } else {
                Err(error.into())
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_contract() {
        let canonical =
            r#"{"findings":"a\nb","complete":false,"improved":true,"correction":"fix"}"#;
        assert_eq!(parse_review(canonical).unwrap().0.findings, "a\nb");
        let array = canonical.replace("\"a\\nb\"", "[\"a\",\"b\"]");
        assert_eq!(parse_review(&array).unwrap().0.findings, "a\nb");
        assert!(parse_review(&format!("```json\n{canonical}\n```")).is_ok());
        for bad in [
            canonical.replace("\"complete\":false,", ""),
            canonical.replace("false", "\"false\""),
            canonical.replace("\"correction\"", "\"extra\":1,\"correction\""),
            canonical[..30].into(),
        ] {
            assert!(parse_review(&bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn raw_document_only() {
        for source in ["scene { box \"a\" }", "```mog\nscene { box \"a\" }\n```"] {
            assert!(
                matches!(parse_tool(source,"original").unwrap().0, super::super::ModelingTool::Apply { revision, .. } if revision=="original")
            );
        }
        for bad in [
            "Here is the fix:\n```mog\nscene { box }\n```",
            "scene {",
            "{\"tool\":",
            "done",
        ] {
            assert!(parse_tool(bad, "r").is_err(), "{bad}");
        }
    }
}
