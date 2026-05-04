use codespan_reporting::diagnostic::{Diagnostic as CdiDiag, Label};
use codespan_reporting::files::SimpleFile;
use codespan_reporting::term::termcolor::{ColorChoice, StandardStream};
use codespan_reporting::term::{self, Config};

use mogen_core::{Diagnostic, Severity};

/// Render diagnostics to stderr with source snippets and color.
pub fn render_human(filename: &str, source: &str, diags: &[Diagnostic]) {
    let file = SimpleFile::new(filename, source);
    let writer = StandardStream::stderr(ColorChoice::Auto);
    let config = Config::default();

    for d in diags {
        let mut cdi = match d.severity {
            Severity::Error => CdiDiag::error(),
            Severity::Warning => CdiDiag::warning(),
            Severity::Info => CdiDiag::note(),
        };
        cdi = cdi.with_message(format!("[{}] {}", d.code, d.message));
        if let Some(s) = d.span {
            cdi = cdi.with_labels(vec![Label::primary((), s.start..s.end)]);
        }
        let _ = term::emit(&mut writer.lock(), &config, &file, &cdi);
    }
}

/// One JSON object per diagnostic, each on its own line.
///
/// `filename` is the fallback used when a diagnostic doesn't carry its own
/// `file` field — preserves single-file CLI behaviour while letting the
/// multi-file wasm editor route per-file diagnostics to the right tab.
pub fn render_json(filename: &str, diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diags {
        let obj = serde_json::json!({
            "file": d.file.as_deref().unwrap_or(filename),
            "severity": d.severity,
            "code": d.code,
            "message": d.message,
            "span": d.span,
        });
        out.push_str(&obj.to_string());
        out.push('\n');
    }
    out
}
