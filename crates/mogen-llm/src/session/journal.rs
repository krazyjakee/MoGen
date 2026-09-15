//! Durable responses are saved before interpretation. Replaying the transcript
//! rebuilds private workspace state; completed requests are never billed twice.
use super::*;
use crate::{GenerateConfig, GenerateResponse, Usage};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub version: u32,
    pub request_digest: String,
    pub revision: String,
    pub phase: String,
    pub provider: String,
    pub model: String,
    pub response: String,
    pub usage: Usage,
    pub state: String,
    pub provenance: String,
    pub outcome: Option<Value>,
    #[serde(default)]
    pub render: Option<RenderedView>,
}
/// Bound retention without evicting responses required for resume.
pub const JOURNAL_BYTES: usize = 32 * 1024 * 1024;
pub(super) fn receive(
    project: &mut ModelingProject,
    cursor: &mut usize,
    workspace: &ModelingWorkspace,
    cfg: &GenerateConfig,
    provider: &str,
    call: &mut dyn FnMut(&GenerateConfig) -> Result<GenerateResponse>,
    checkpoint: &mut dyn FnMut(&ModelingProject),
) -> Result<(usize, GenerateResponse)> {
    let rev = workspace.revision()?;
    workspace.check_revision(&rev)?;
    let digest=identity(serde_json::to_string(&json!({"revision":rev,"phase":cfg.spend_context.operation,
        "provider":provider,"model":cfg.model,"settings":SessionRequestSettings::new(cfg,provider),"system":cfg.system_instruction,"prompt":cfg.user_prompt,
        "history":cfg.history.iter().map(|t|json!([t.role.chat_str(),t.text])).collect::<Vec<_>>(),
        "images":cfg.user_images.iter().map(|i|identity(&i.data)).collect::<Vec<_>>()}))?.as_bytes());
    let index = *cursor;
    if let Some(attempt) = project.attempts.get(index) {
        if attempt.request_digest != digest {
            bail!("Saved request differs from current source, dependencies, context or captures; start a new session");
        }
        if attempt.response.len() > MAX_RESPONSE_BYTES {
            bail!("Response exceeds 1 MiB limit; raw response retained for inspection");
        }
        *cursor += 1;
        return Ok((
            index,
            GenerateResponse {
                text: attempt.response.clone(),
                usage: attempt.usage.clone(),
            },
        ));
    }
    if project.attempts.len() >= 256
        || project
            .attempts
            .iter()
            .map(|a| a.response.len())
            .sum::<usize>()
            >= JOURNAL_BYTES
    {
        bail!("Response journal retention limit reached; export retained work and start a new session");
    }
    if let Some(c) = &cfg.session_control {
        c.check().map_err(anyhow::Error::msg)?;
    }
    project.stage = format!(
        "{}: awaiting provider response",
        cfg.spend_context.operation
    );
    project.sync_control(cfg);
    // Durable admission intent closes the crash gap before the adapter's gate.
    // A crash here reserves one possibly-billed call conservatively; receipt
    // replaces it with the actual meter, never adding the same usage twice.
    project.meter.calls = project.meter.calls.saturating_add(1);
    project.meter.unknown_cost = true;
    checkpoint(project);
    let mut receipt_cfg = cfg.clone();
    receipt_cfg.retain_stopped_response = true;
    let response = call(&receipt_cfg)?;
    project.attempts.push(Attempt {
        version: 1,
        request_digest: digest,
        revision: rev,
        phase: cfg.spend_context.operation.clone(),
        provider: provider.into(),
        model: cfg.model.clone(),
        response: response.text.clone(),
        usage: response.usage.clone(),
        state: "received".into(),
        provenance: String::new(),
        outcome: None,
        render: None,
    });
    if let Some(c) = &cfg.session_control {
        project.meter = c.meter();
        project.elapsed_seconds = c.elapsed().as_secs();
    }
    project.stage = format!("{}: response saved", cfg.spend_context.operation);
    checkpoint(project); // deliberately before parse, cancellation and stale checks
    *cursor += 1;
    if response.text.len() > MAX_RESPONSE_BYTES {
        project.attempts[index].state = "oversized".into();
        project.attempts[index].outcome = Some(json!({"error":"Response exceeds 1 MiB limit"}));
        checkpoint(project);
        bail!("Response exceeds 1 MiB limit; raw response retained for inspection");
    }
    Ok((index, response))
}
