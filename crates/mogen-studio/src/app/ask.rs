//! "Ask MoGen" — context-menu Q&A about the user's DSL code.
//!
//! Right-clicking the editor surfaces an `Ask…` item that opens a modal where
//! the user can ask a free-form question about the selected snippet (or the
//! whole file if nothing is selected). The question is sent to the fast
//! Gemini model alongside the cached system instruction (full DSL grammar +
//! stdlib), so answers are grounded in the actual language rather than the
//! model's pre-training.
//!
//! The flow is intentionally read-only — Ask never rewrites the buffer. It's
//! the teaching counterpart to Modify / Animate.

use std::sync::Arc;

use eframe::egui;
use mogen_llm::{GenerateConfig, Provider, ThinkingLevel};

use super::types::AskInFlight;
use super::MogenStudioApp;

/// Snippet bytes captured from the editor at the moment the user clicked
/// `Ask…`, plus a short human label describing what was captured. Capturing
/// at click-time decouples the modal from later edits to the editor.
pub(super) fn capture_snippet(
    ui: &egui::Ui,
    editor_id: egui::Id,
    source: &str,
) -> (String, String) {
    let sel_range = egui::TextEdit::load_state(ui.ctx(), editor_id)
        .and_then(|s| s.cursor.char_range());
    let (lo, hi) = match sel_range {
        Some(range) => {
            let [lo, hi] = range.sorted();
            let lo_b = source
                .char_indices()
                .nth(lo.index)
                .map(|(b, _)| b)
                .unwrap_or(source.len());
            let hi_b = source
                .char_indices()
                .nth(hi.index)
                .map(|(b, _)| b)
                .unwrap_or(source.len());
            (lo_b, hi_b)
        }
        None => (0, 0),
    };
    if hi > lo {
        let snippet = source[lo..hi].to_string();
        let lines = snippet.lines().count().max(1);
        let label = format!(
            "Selected snippet ({lines} line{}, {} char{})",
            if lines == 1 { "" } else { "s" },
            snippet.chars().count(),
            if snippet.chars().count() == 1 { "" } else { "s" },
        );
        (snippet, label)
    } else {
        let snippet = source.to_string();
        let lines = snippet.lines().count().max(1);
        let label = if snippet.trim().is_empty() {
            "Entire file (empty)".to_string()
        } else {
            format!(
                "Entire file ({lines} line{})",
                if lines == 1 { "" } else { "s" },
            )
        };
        (snippet, label)
    }
}

impl MogenStudioApp {
    /// Stash the captured context, clear any prior answer, and raise the
    /// modal. Called from the editor context-menu handler.
    pub(super) fn open_ask_modal(&mut self, snippet: String, label: String) {
        self.ask_code_context = snippet;
        self.ask_context_label = label;
        self.ask_question_draft.clear();
        self.ask_answer = None;
        self.ask_focus_pending = true;
        self.show_ask = true;
    }

    /// Kick off the background Ask call. No-ops if another call is already in
    /// flight, the question is empty, or no API key is available — the modal
    /// surfaces those conditions inline.
    pub(super) fn start_ask(&mut self, ctx: egui::Context) {
        if self.ask_in_flight.is_some() {
            return;
        }
        let question = self.ask_question_draft.trim().to_string();
        if question.is_empty() {
            self.ask_answer = Some(Err("type a question first".into()));
            return;
        }
        let provider = self.settings.provider();
        let api_key = match self.resolve_api_key() {
            Some(k) => k,
            None => {
                self.ask_answer = Some(Err(format!(
                    "no {} API key — set one in Edit → Preferences…",
                    provider.label(),
                )));
                return;
            }
        };
        let model = self.settings.provider_fast_model();
        let claude_code_path = self.settings.claude_code_path();
        let sys_instr = self.cached_system_instruction();
        let code = self.ask_code_context.clone();
        let context_label = self.ask_context_label.clone();

        // Clear the previous answer once a fresh call is on the wire so the
        // modal doesn't briefly show stale text under a spinner.
        self.ask_answer = None;

        let (tx, rx) = std::sync::mpsc::channel();
        self.ask_in_flight = Some(AskInFlight { rx });

        std::thread::spawn(move || {
            let result = run_ask_question(
                question,
                code,
                context_label,
                provider,
                api_key,
                model,
                sys_instr,
                claude_code_path,
            );
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Drain the in-flight Ask channel, if any. Stores the result on
    /// `ask_answer` and clears the in-flight slot.
    pub(super) fn poll_ask(&mut self) {
        let Some(slot) = self.ask_in_flight.as_ref() else {
            return;
        };
        let result = match slot.rx.try_recv() {
            Ok(r) => r,
            Err(_) => return,
        };
        self.ask_in_flight = None;
        self.ask_answer = Some(result);
    }

    pub(super) fn any_ask_in_flight(&self) -> bool {
        self.ask_in_flight.is_some()
    }
}

/// Synchronous worker that calls the active provider's fast model with a
/// teaching prompt about the captured code. The system instruction (cached,
/// shared with the rest of the LLM paths) carries the full DSL grammar +
/// stdlib so answers stay grounded.
fn run_ask_question(
    question: String,
    code: String,
    context_label: String,
    provider: Provider,
    api_key: String,
    model: String,
    sys_instr: Arc<String>,
    claude_code_path: String,
) -> Result<String, String> {
    let client = super::util::build_provider_client(provider, api_key, &claude_code_path);

    // Tag the code so the model knows whether it's looking at a snippet or
    // the whole file. The "do not rewrite" guidance keeps replies pedagogical
    // — the user has Modify / Animate for actual edits.
    let user = if code.trim().is_empty() {
        format!(
            "A user is learning the MoGen DSL but has not written any code yet. \
             Answer their question pedagogically: explain the concepts, link \
             them to the relevant DSL constructs, and show a small example \
             when it would help. Use plain text with markdown allowed for \
             code fences and lists. Keep the answer focused and concise.\n\n\
             Question: {question}",
        )
    } else {
        format!(
            "A user is learning the MoGen DSL and has asked a question about \
             their code ({context_label}). Below is the code in a ```mog code \
             fence. Answer their question pedagogically: explain what the \
             relevant constructs do, point to specific lines or attributes \
             when useful, and show a small corrected or alternative snippet \
             if it clarifies the answer. Do NOT rewrite the user's whole file \
             — they asked a question, not for a refactor. Use plain text with \
             markdown allowed for code fences and lists. Keep it focused and \
             concise.\n\n\
             Code:\n```mog\n{code}\n```\n\n\
             Question: {question}",
        )
    };

    let mut cfg = GenerateConfig::new(user);
    cfg.model = model;
    cfg.system_instruction = Some((*sys_instr).clone());
    // Q&A is a low-reasoning task — Low budget keeps latency in line with
    // the "fast" label even when the model wants to think a bit.
    cfg.thinking_level = Some(ThinkingLevel::Low);
    // A touch of variance produces more natural prose than the structured
    // DSL default.
    cfg.temperature = Some(0.6);

    match client.generate(&cfg) {
        Ok(resp) => {
            let cleaned = resp.text.trim().to_string();
            if cleaned.is_empty() {
                Err(format!("empty response from {}", provider.label()))
            } else {
                Ok(cleaned)
            }
        }
        Err(e) => Err(format!("{e}")),
    }
}
