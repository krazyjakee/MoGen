//! Codex subscription provider, using the installed CLI's ChatGPT login.
//! See https://learn.chatgpt.com/docs/non-interactive-mode.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::provider::ProviderError;
use crate::types::{GenerateConfig, GenerateResponse, Usage};

pub const DEFAULT_BINARY: &str = "codex";
pub const DEFAULT_MODEL: &str = crate::openai::DEFAULT_MODEL;

pub struct CodexClient {
    path: String,
}

impl Default for CodexClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexClient {
    pub fn new() -> Self {
        Self::with_path(DEFAULT_BINARY)
    }

    pub fn with_path(path: impl Into<String>) -> Self {
        let path = path.into();
        let path = path.trim();
        Self {
            path: if path.is_empty() {
                DEFAULT_BINARY
            } else {
                path
            }
            .into(),
        }
    }

    pub fn generate(&self, cfg: &GenerateConfig) -> Result<GenerateResponse, ProviderError> {
        // Each request owns its files, including concurrent image/repair calls.
        // An empty working directory avoids loading the user's project instructions.
        let dir = tempfile::Builder::new()
            .prefix("mogen-codex-")
            .tempdir()
            .map_err(io_error)?;
        let mut cmd = Command::new(&self.path);
        cmd.current_dir(dir.path())
            .args([
                "exec",
                "--json",
                "--ephemeral",
                "--skip-git-repo-check",
                "--ignore-user-config",
                "--sandbox",
                "read-only",
                "-c",
                "approval_policy=\"never\"",
                "-c",
                "forced_login_method=\"chatgpt\"",
                "-c",
                "cli_auth_credentials_store=\"auto\"",
                "-c",
                "model_provider=\"openai\"",
                "-c",
                "features.shell_tool=false",
                "-c",
                "web_search=\"disabled\"",
            ])
            // A subscription selection must not silently become API billing.
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_API_KEY");
        let model = if cfg.model.trim().is_empty() {
            DEFAULT_MODEL
        } else {
            cfg.model.trim()
        };
        cmd.args(["--model", model]);
        if let Some(level) = cfg.thinking_level {
            cmd.args(["-c", &format!("model_reasoning_effort=\"{}\"", level.key())]);
        }
        for (i, image) in cfg.user_images.iter().enumerate() {
            let extension = match image.mime_type.to_ascii_lowercase().as_str() {
                "image/jpeg" | "image/jpg" => "jpg",
                "image/webp" => "webp",
                "image/gif" => "gif",
                _ => "png",
            };
            let path = dir.path().join(format!("image-{i}.{extension}"));
            std::fs::write(&path, &image.data).map_err(io_error)?;
            cmd.arg("--image").arg(path);
        }
        // `--` prevents the variadic image argument from consuming the stdin marker.
        cmd.args(["--", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| ProviderError::Transport(format!(
            "could not start Codex ({:?}): {e}. Install the Codex CLI and run `codex login` with ChatGPT.", self.path
        )))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let prompt = build_prompt(cfg);
        // Drain output while writing large prompts to avoid pipe-buffer deadlocks.
        let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let output = child.wait_with_output().map_err(io_error)?;
        let written = writer
            .join()
            .map_err(|_| ProviderError::Transport("Codex stdin writer failed".into()))?;
        let parsed = parse_events(&output.stdout, cfg.budget_tokens);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                parsed
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "no error details".into())
            };
            return Err(ProviderError::Transport(format!(
                "Codex exited with {}: {}. Check `codex login status` and your plan limits.",
                output.status, message
            )));
        }
        written.map_err(io_error)?;
        parsed
    }
}

fn io_error(error: std::io::Error) -> ProviderError {
    ProviderError::Transport(format!("Codex: {error}"))
}

fn build_prompt(cfg: &GenerateConfig) -> String {
    let mut prompt =
        String::from("Answer the request directly as text. Do not run tools or edit files.\n\n");
    if let Some(system) = &cfg.system_instruction {
        prompt.push_str(&format!("[SYSTEM]\n{system}\n\n"));
    }
    for turn in &cfg.history {
        prompt.push_str(&format!("[{}]\n{}\n\n", turn.role.chat_str(), turn.text));
    }
    prompt.push_str(&format!("[REQUEST]\n{}\n", cfg.user_prompt));
    prompt
}

fn parse_events(stdout: &[u8], budget: Option<u32>) -> Result<GenerateResponse, ProviderError> {
    let mut text = String::new();
    let mut usage = Usage::default();
    let mut completed = false;
    for line in stdout
        .split(|&b| b == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
    {
        let event: serde_json::Value = serde_json::from_slice(line).map_err(|e| {
            ProviderError::InvalidResponse(format!("Invalid Codex JSON event: {e}"))
        })?;
        match event["type"].as_str() {
            Some("item.completed") if event["item"]["type"] == "agent_message" => {
                // Commentary can precede the final answer; only the last message is the result.
                text = event["item"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
            }
            Some("turn.completed") => {
                completed = true;
                let u = &event["usage"];
                let count = |key: &str| u[key].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
                usage.prompt_tokens = count("input_tokens");
                usage.response_tokens = count("output_tokens");
                usage.cached_tokens = count("cached_input_tokens");
                usage.total_tokens = usage.prompt_tokens.saturating_add(usage.response_tokens);
            }
            Some("turn.failed") => {
                return Err(ProviderError::InvalidResponse(
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("Codex turn failed")
                        .into(),
                ));
            }
            _ => {} // Unknown events and recoverable reconnect errors are not final failures.
        }
    }
    if !completed {
        return Err(ProviderError::InvalidResponse(
            "Codex did not complete the turn".into(),
        ));
    }
    if text.trim().is_empty() {
        return Err(ProviderError::EmptyResponse);
    }
    if let Some(budget) = budget {
        if usage.total_tokens > budget {
            return Err(ProviderError::BudgetExceeded {
                used: usage.total_tokens,
                budget,
            });
        }
    }
    Ok(GenerateResponse { text, usage })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS: &str = concat!(
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Thinking...\"}}\n",
        "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"scene {}\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":12,\"cached_input_tokens\":4,\"output_tokens\":8}}\n"
    );

    #[test]
    fn extracts_final_message_usage_and_checks_budget() {
        let response = parse_events(EVENTS.as_bytes(), Some(20)).unwrap();
        assert_eq!(response.text, "scene {}");
        assert_eq!(response.usage.total_tokens, 20);
        assert_eq!(response.usage.cached_tokens, 4);
        assert!(matches!(
            parse_events(EVENTS.as_bytes(), Some(19)),
            Err(ProviderError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn rejects_failure_truncation_empty_and_malformed_output() {
        for events in ["", "not json", "{\"type\":\"turn.completed\"}", "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"partial\"}}"] {
            assert!(parse_events(events.as_bytes(), None).is_err());
        }
        let failure = format!(
            "{EVENTS}{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"quota exceeded\"}}}}\n"
        );
        assert!(parse_events(failure.as_bytes(), None)
            .unwrap_err()
            .to_string()
            .contains("quota exceeded"));
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_errors_keep_stderr_and_structured_failure_details() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("codex");
        for (body, expected) in [
            ("echo 'please log in' >&2", "please log in"),
            (
                r#"echo '{"type":"turn.failed","error":{"message":"quota exceeded"}}'"#,
                "quota exceeded",
            ),
        ] {
            std::fs::write(&script, format!("#!/bin/sh\n{body}\nexit 1\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            // Early exit while a large prompt is still being written must not
            // replace the useful auth error with a generic broken-pipe error.
            let cfg = GenerateConfig::new("x".repeat(100_000));
            let error = CodexClient::with_path(script.to_string_lossy())
                .generate(&cfg)
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn subprocess_receives_prompt_images_and_isolated_subscription_settings() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("codex stub");
        let capture = dir.path().join("capture");
        // Stub captures arguments, stdin, and the attached image before cleanup.
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$@" > '{}.args'
pwd > '{}.cwd'
cat > '{}.prompt'
for arg in "$@"; do
  case "$arg" in */image-0.png) cp "$arg" '{}.image';; esac
done
[ -z "$OPENAI_API_KEY" ] && [ -z "$CODEX_API_KEY" ] || exit 9
cat <<'EVENTS'
{EVENTS}
EVENTS
"#,
                capture.display(),
                capture.display(),
                capture.display(),
                capture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = GenerateConfig::new("make a cube");
        cfg.system_instruction = Some("write MoGen DSL".into());
        cfg.history.push(crate::types::Turn {
            role: crate::types::Role::Model,
            text: "previous scene".into(),
        });
        cfg.user_images.push(crate::types::ImageInput {
            mime_type: "image/png".into(),
            data: vec![1, 2, 3],
        });
        cfg.thinking_level = Some(crate::types::ThinkingLevel::High);
        let response = CodexClient::with_path(script.to_string_lossy())
            .generate(&cfg)
            .unwrap();
        assert_eq!(response.text, "scene {}");
        let args = std::fs::read_to_string(capture.with_extension("args")).unwrap();
        for expected in [
            "--ignore-user-config",
            "read-only",
            "forced_login_method=\"chatgpt\"",
            "model_reasoning_effort=\"high\"",
            "--image",
            "--\n-\n",
        ] {
            assert!(args.contains(expected), "missing {expected}: {args}");
        }
        let prompt = std::fs::read_to_string(capture.with_extension("prompt")).unwrap();
        for expected in ["write MoGen DSL", "previous scene", "make a cube"] {
            assert!(prompt.contains(expected));
        }
        assert_eq!(
            std::fs::read(capture.with_extension("image")).unwrap(),
            [1, 2, 3]
        );
        let cwd = std::fs::read_to_string(capture.with_extension("cwd")).unwrap();
        assert!(
            !std::path::Path::new(cwd.trim()).exists(),
            "temporary files should be cleaned up"
        );
    }
}
