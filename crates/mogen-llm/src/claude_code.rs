//! Claude Code (`claude` CLI) provider.
//!
//! Wraps the user's installed `claude` binary in non-interactive print mode.
//! Authentication is delegated entirely to the local Claude Code install
//! (Pro/Max subscription login or whatever credentials `claude /login`
//! recorded), so this provider has no API key field.
//!
//! Wire shape:
//!
//! ```text
//! claude --print --output-format json --max-turns 1 [--model <id>]
//! ```
//!
//! The full prompt (system instruction + history + current user turn) is
//! flattened to a single text blob and piped to the child's stdin. The CLI
//! emits a JSON envelope on stdout from which we extract `result` (the
//! assistant text) and `usage` (token counts).
//!
//! `--max-turns 1` is used for text-only calls so the CLI cannot iterate
//! on tool calls — the model gets one shot to produce text. When the
//! caller passes a non-empty [`GenerateConfig::user_images`], we instead
//! write each image to a temp file and reference it as `@<absolute-path>`
//! inside the prompt; the spawned `claude` invocation then uses
//! `--max-turns 3` so the model can `Read` the file (turn 1) and follow
//! up before producing its final assistant text.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;
use thiserror::Error;

use crate::types::{GenerateConfig, GenerateResponse, Role, Usage};

/// Default heavy text model. `sonnet` is the alias the `claude` CLI accepts —
/// it resolves to the latest Sonnet your subscription has access to, which
/// matches the CLI's own interactive default.
pub const DEFAULT_MODEL: &str = "sonnet";

/// Default fast model used by the Studio Prompt Enhancer / Ask modal.
pub const DEFAULT_FAST_MODEL: &str = "haiku";

/// Default executable name. Resolved against `PATH`.
pub const DEFAULT_BINARY: &str = "claude";

#[derive(Debug, Error)]
pub enum ClaudeCodeError {
    #[error("failed to spawn `{path}`: {source}")]
    SpawnFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("`claude` exited with code {code}: {message}")]
    NonZeroExit { code: i32, message: String },
    #[error("empty response: `claude` produced no result text")]
    EmptyResponse,
    #[error("budget exceeded: {used} input+output tokens exceeds --budget-tokens={budget}")]
    BudgetExceeded { used: u32, budget: u32 },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

pub struct ClaudeCodeClient {
    /// Absolute path or PATH-resolvable name of the `claude` binary.
    path: String,
}

impl ClaudeCodeClient {
    /// Construct a client that invokes the `claude` binary on `PATH`.
    pub fn new() -> Self {
        Self { path: DEFAULT_BINARY.to_string() }
    }

    /// Construct a client that invokes a specific binary path. Empty input
    /// falls back to the default (`claude`).
    pub fn with_path(path: impl Into<String>) -> Self {
        let p = path.into();
        let trimmed = p.trim();
        Self {
            path: if trimmed.is_empty() { DEFAULT_BINARY.to_string() } else { trimmed.to_string() },
        }
    }

    /// Override the binary path on an existing client. Used by Studio when
    /// the user changes the setting at runtime.
    pub fn set_path(&mut self, path: impl Into<String>) {
        let p = path.into();
        let trimmed = p.trim();
        self.path = if trimmed.is_empty() {
            DEFAULT_BINARY.to_string()
        } else {
            trimmed.to_string()
        };
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, ClaudeCodeError> {
        // Materialise any attached images to disk first, so we can reference
        // them by absolute path inside the prompt. The model uses its
        // built-in `Read` tool to load each file as a vision content block —
        // that's how `claude --print` exposes vision input today (the CLI
        // has no `--image` flag).
        let image_paths = write_temp_images(&cfg.user_images)?;
        let prompt = build_prompt(cfg, &image_paths);

        // Text-only calls keep the historical fast-path: one turn, no tool
        // use. Vision calls need a small budget so the model can `Read`
        // each attached image (turn 1), optionally follow up (turn 2),
        // then deliver its assistant text (turn 3).
        let max_turns = if image_paths.is_empty() { "1" } else { "3" };

        let mut cmd = Command::new(&self.path);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("json")
            .arg("--max-turns")
            .arg(max_turns);
        if !cfg.model.trim().is_empty() {
            cmd.arg("--model").arg(cfg.model.trim());
        }

        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| ClaudeCodeError::SpawnFailed {
            path: self.path.clone(),
            source: e,
        })?;

        // Write the flattened prompt to stdin and close it so `claude` knows
        // input is complete. `take()` releases the handle so wait_with_output
        // can collect stdout/stderr without deadlocking.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes())?;
        }

        let out = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();

        // `claude --output-format json` writes its structured error envelope to
        // stdout (with `is_error: true`) even when it exits non-zero — model not
        // found, quota exhausted, auth failure all land there with stderr left
        // empty. Try to parse stdout first so we can surface that message
        // regardless of exit status; only fall through to a bare "exit code N"
        // when stdout doesn't look like the expected JSON.
        let parsed: Result<RawResponse, _> = serde_json::from_slice(&out.stdout);

        if !out.status.success() {
            let code = out.status.code().unwrap_or(-1);
            let message = match &parsed {
                Ok(r) if r.is_error.unwrap_or(false) => r
                    .result
                    .as_deref()
                    .map(str::to_string)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| stderr.clone()),
                _ => stderr,
            };
            return Err(ClaudeCodeError::NonZeroExit { code, message });
        }

        let parsed = parsed
            .map_err(|e| ClaudeCodeError::InvalidResponse(format!("{e}: stdout was not JSON")))?;

        if parsed.is_error.unwrap_or(false) {
            let msg = parsed
                .result
                .as_deref()
                .unwrap_or("`claude` reported is_error=true with no message")
                .to_string();
            return Err(ClaudeCodeError::InvalidResponse(msg));
        }

        let text = parsed.result.unwrap_or_default();
        if text.trim().is_empty() {
            return Err(ClaudeCodeError::EmptyResponse);
        }

        let usage = parsed
            .usage
            .map(|u| {
                let prompt_t = u.input_tokens.unwrap_or(0);
                let response_t = u.output_tokens.unwrap_or(0);
                let cached = u.cache_read_input_tokens.unwrap_or(0);
                Usage {
                    prompt_tokens: prompt_t,
                    response_tokens: response_t,
                    total_tokens: prompt_t + response_t,
                    cached_tokens: cached,
                }
            })
            .unwrap_or_default();

        if let Some(budget) = cfg.budget_tokens {
            if usage.total_tokens > budget {
                return Err(ClaudeCodeError::BudgetExceeded {
                    used: usage.total_tokens,
                    budget,
                });
            }
        }

        Ok(GenerateResponse { text, usage })
    }
}

impl Default for ClaudeCodeClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Flatten system instruction + history + user turn into a single prompt.
/// `claude -p` accepts one text input, so we reconstruct turn structure with
/// plain delimiters.
///
/// When `image_paths` is non-empty, the function prepends an
/// `[ATTACHED IMAGES]` block inside the `[REQUEST]` section that lists
/// each image with an `@<absolute-path>` reference and instructs the
/// model to use its built-in `Read` tool to load each file before
/// answering. That's how `claude --print` ingests vision input today —
/// the CLI has no dedicated `--image` flag.
fn build_prompt(cfg: &GenerateConfig, image_paths: &[PathBuf]) -> String {
    let mut out = String::with_capacity(cfg.user_prompt.len() + 256);

    if let Some(sys) = &cfg.system_instruction {
        if !sys.trim().is_empty() {
            out.push_str("[SYSTEM]\n");
            out.push_str(sys);
            out.push_str("\n\n");
        }
    }

    if !cfg.history.is_empty() {
        out.push_str("[CONVERSATION]\n");
        for turn in &cfg.history {
            let label = match turn.role {
                Role::User => "user",
                Role::Model => "assistant",
            };
            out.push_str(&format!("--- {label} ---\n{}\n\n", turn.text));
        }
    }

    out.push_str("[REQUEST]\n");
    if !image_paths.is_empty() {
        out.push_str("[ATTACHED IMAGES]\n");
        out.push_str(
            "Use your built-in Read tool on each of the following image \
             files before answering. Treat their contents as visual \
             reference for the request below.\n",
        );
        for p in image_paths {
            out.push_str(&format!("@{}\n", p.display()));
        }
        out.push('\n');
    }
    out.push_str(&cfg.user_prompt);
    if !cfg.user_prompt.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Pick a filesystem extension for an image based on its declared MIME
/// type. Falls back to `bin` when the MIME is unknown so we always
/// produce a writable path; the model's `Read` tool is content-sniffed
/// rather than extension-driven, so the fallback still works.
fn extension_for_mime(mime_type: &str) -> &'static str {
    match mime_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "bin",
    }
}

/// Materialise each [`crate::types::ImageInput`] to a deterministic file
/// under `<temp>/mogen-claude-images/img_<idx>.<ext>` so the spawned
/// `claude --print` invocation can `Read` it. Deterministic names mean a
/// re-run overwrites the prior file in place, so the directory does not
/// grow across the repair loop.
fn write_temp_images(
    images: &[crate::types::ImageInput],
) -> Result<Vec<PathBuf>, ClaudeCodeError> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let dir = std::env::temp_dir().join("mogen-claude-images");
    std::fs::create_dir_all(&dir)?;
    let mut paths = Vec::with_capacity(images.len());
    for (idx, img) in images.iter().enumerate() {
        let ext = extension_for_mime(&img.mime_type);
        let path = dir.join(format!("img_{idx}.{ext}"));
        std::fs::write(&path, &img.data)?;
        paths.push(path);
    }
    Ok(paths)
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    /// "result" subtype carries the final assistant text. Other subtypes
    /// (e.g. errors) leave it empty.
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Turn;

    #[test]
    fn build_prompt_emits_system_then_history_then_request() {
        let mut cfg = GenerateConfig::new("make a cube");
        cfg.system_instruction = Some("you write DSL".into());
        cfg.history.push(Turn { role: Role::User, text: "first try".into() });
        cfg.history.push(Turn { role: Role::Model, text: "scene { }".into() });
        let p = build_prompt(&cfg, &[]);
        let sys_idx = p.find("[SYSTEM]").unwrap();
        let conv_idx = p.find("[CONVERSATION]").unwrap();
        let req_idx = p.find("[REQUEST]").unwrap();
        assert!(sys_idx < conv_idx && conv_idx < req_idx);
        assert!(p.contains("you write DSL"));
        assert!(p.contains("--- user ---"));
        assert!(p.contains("--- assistant ---"));
        assert!(p.ends_with("make a cube\n"));
    }

    #[test]
    fn build_prompt_skips_empty_sections() {
        let cfg = GenerateConfig::new("hi");
        let p = build_prompt(&cfg, &[]);
        assert!(!p.contains("[SYSTEM]"));
        assert!(!p.contains("[CONVERSATION]"));
        assert!(p.starts_with("[REQUEST]"));
    }

    #[test]
    fn parse_response_extracts_result_and_usage() {
        let raw = br#"{"result":"scene {}","is_error":false,"usage":{"input_tokens":12,"output_tokens":8}}"#;
        let parsed: RawResponse = serde_json::from_slice(raw).unwrap();
        assert_eq!(parsed.result.as_deref(), Some("scene {}"));
        assert_eq!(parsed.is_error, Some(false));
        let u = parsed.usage.unwrap();
        assert_eq!(u.input_tokens, Some(12));
        assert_eq!(u.output_tokens, Some(8));
    }

    #[test]
    fn with_path_falls_back_to_default_when_blank() {
        let c = ClaudeCodeClient::with_path("   ");
        assert_eq!(c.path(), DEFAULT_BINARY);
    }

    /// Reproduces the "API error 1" symptom: `claude --output-format json`
    /// emits its structured error envelope to stdout (with `is_error: true`)
    /// even when it exits non-zero, and writes nothing to stderr. The client
    /// must surface the stdout-side message rather than the empty stderr.
    #[cfg(unix)]
    #[test]
    fn nonzero_exit_surfaces_stdout_error_envelope() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let stub = std::env::temp_dir().join(format!("mogen_cc_stub_{nanos}.sh"));
        let script = "#!/bin/sh\n\
                      cat <<'JSON'\n\
                      {\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\
                      \"api_error_status\":404,\"result\":\"There's an issue with the \
                      selected model (foo). It may not exist or you may not have access \
                      to it.\"}\n\
                      JSON\n\
                      exit 1\n";
        fs::write(&stub, script).expect("write stub");
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("chmod stub");

        let client = ClaudeCodeClient::with_path(stub.to_string_lossy().into_owned());
        let cfg = GenerateConfig::new("anything");
        let err = client.generate(&cfg).expect_err("stub exits non-zero");
        let _ = fs::remove_file(&stub);

        match err {
            ClaudeCodeError::NonZeroExit { code, message } => {
                assert_eq!(code, 1, "exit code should pass through");
                assert!(
                    message.contains("issue with the selected model"),
                    "message must come from the stdout JSON envelope, got: {message:?}",
                );
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
    }

    #[test]
    fn build_prompt_emits_attached_section_when_images_present() {
        let cfg = GenerateConfig::new("describe");
        let paths = vec![
            PathBuf::from("/tmp/mogen-claude-images/img_0.png"),
            PathBuf::from("/tmp/mogen-claude-images/img_1.jpg"),
        ];
        let p = build_prompt(&cfg, &paths);
        // Section must live inside the request block, before the user prompt.
        let attached = p.find("[ATTACHED IMAGES]").expect("attached header");
        let req = p.find("[REQUEST]").expect("request header");
        let prompt = p.find("describe").expect("user prompt");
        assert!(req < attached, "[ATTACHED IMAGES] must follow [REQUEST]");
        assert!(attached < prompt, "[ATTACHED IMAGES] must precede the prompt");
        assert!(p.contains("@/tmp/mogen-claude-images/img_0.png"));
        assert!(p.contains("@/tmp/mogen-claude-images/img_1.jpg"));
        assert!(
            p.contains("Read tool"),
            "prompt must instruct the model to use Read tool, got: {p}",
        );
    }

    #[test]
    fn build_prompt_omits_attached_section_when_no_images() {
        // Text-only path must stay byte-identical to today's output —
        // a regression to a non-empty `[ATTACHED IMAGES]` would burn tokens
        // and confuse the model on every text generation call.
        let cfg = GenerateConfig::new("hi");
        let p = build_prompt(&cfg, &[]);
        assert!(!p.contains("[ATTACHED IMAGES]"));
        assert!(p.starts_with("[REQUEST]"));
        assert!(p.ends_with("hi\n"));
    }

    #[test]
    fn extension_for_mime_picks_image_extension() {
        assert_eq!(extension_for_mime("image/png"), "png");
        assert_eq!(extension_for_mime("image/jpeg"), "jpg");
        assert_eq!(extension_for_mime("image/jpg"), "jpg");
        assert_eq!(extension_for_mime("image/webp"), "webp");
        assert_eq!(extension_for_mime("image/gif"), "gif");
        assert_eq!(extension_for_mime("image/bmp"), "bmp");
        // Unknown / blank MIME types fall back to a writable extension so the
        // path resolves regardless; the model's `Read` tool is content-sniffed.
        assert_eq!(extension_for_mime("application/octet-stream"), "bin");
        assert_eq!(extension_for_mime(""), "bin");
        assert_eq!(extension_for_mime("IMAGE/PNG"), "png");
    }

    #[test]
    fn write_temp_images_uses_extension_from_mime() {
        use crate::types::ImageInput;
        let imgs = vec![
            ImageInput { mime_type: "image/png".into(), data: vec![1, 2, 3] },
            ImageInput { mime_type: "image/jpeg".into(), data: vec![4, 5, 6] },
        ];
        let paths = write_temp_images(&imgs).expect("write succeeds");
        assert_eq!(paths.len(), 2);
        // Names are deterministic (idx-based) so re-runs overwrite in place.
        assert!(paths[0].file_name().unwrap().to_string_lossy().ends_with("img_0.png"));
        assert!(paths[1].file_name().unwrap().to_string_lossy().ends_with("img_1.jpg"));
        // The bytes must round-trip so the model's `Read` sees what we wrote.
        assert_eq!(std::fs::read(&paths[0]).unwrap(), vec![1, 2, 3]);
        assert_eq!(std::fs::read(&paths[1]).unwrap(), vec![4, 5, 6]);
        for p in &paths {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn write_temp_images_returns_empty_on_empty_input() {
        let paths = write_temp_images(&[]).expect("empty ok");
        assert!(paths.is_empty());
    }
}
