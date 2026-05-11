use std::collections::HashMap;

use eframe::egui;

/// Render the rename + delete chip at the bottom of a material's body.
/// Rename uses an in-place text field committed on focus loss (mirrors the
/// meta editor); Delete is a single click — the caller is responsible for
/// any confirmation flow. Mutates `drafts` directly (per-material rename
/// buffer) and writes `pending_delete` / `pending_rename` on commit.
pub(super) fn render(
    ui: &mut egui::Ui,
    idx: usize,
    mat: &mogen_core::Material,
    drafts: &mut HashMap<String, String>,
    pending_delete: &mut Option<String>,
    pending_rename: &mut Option<(String, String)>,
) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(egui::RichText::new("manage").weak());
    let mut rename_buf = drafts
        .entry(mat.name.clone())
        .or_insert_with(|| mat.name.clone())
        .clone();
    let rename_resp = ui.horizontal(|ui| {
        ui.label("Rename");
        ui.add(
            egui::TextEdit::singleline(&mut rename_buf)
                .desired_width(160.0)
                .id_salt(("mat_rename", idx)),
        )
    });
    if rename_resp.inner.changed() {
        drafts.insert(mat.name.clone(), rename_buf.clone());
    }
    if rename_resp.inner.lost_focus()
        && !rename_buf.trim().is_empty()
        && rename_buf != mat.name
    {
        *pending_rename = Some((mat.name.clone(), rename_buf.trim().to_string()));
    }

    ui.horizontal(|ui| {
        if ui
            .button(egui::RichText::new("🗑 Delete material"))
            .on_hover_text(
                "Remove the material declaration from the source. \
                 Nodes that reference it will fall back to default PBR \
                 until you reassign them.",
            )
            .clicked()
        {
            *pending_delete = Some(mat.name.clone());
        }
    });
}

/// Suggest a unique `material_<n>` name for a freshly-added material by
/// scanning the source for any existing `material "material_N" …` literal
/// and returning `material_<max+1>`. Defaults to `material_1` when no
/// numbered material is present.
pub(super) fn next_material_name(src: &str) -> String {
    let prefix = "material_";
    let mut max_n: u32 = 0;
    for line in src.lines() {
        let trimmed = line.trim_start();
        let after_kw = match trimmed.strip_prefix("material ") {
            Some(s) => s,
            None => continue,
        };
        let after_quote = match after_kw.trim_start().strip_prefix('"') {
            Some(s) => s,
            None => continue,
        };
        let end = match after_quote.find('"') {
            Some(e) => e,
            None => continue,
        };
        let name = &after_quote[..end];
        if let Some(rest) = name.strip_prefix(prefix) {
            if let Ok(n) = rest.parse::<u32>() {
                if n > max_n {
                    max_n = n;
                }
            }
        }
    }
    format!("{prefix}{}", max_n + 1)
}

/// Rewrite the quoted name literal inside the `material "name" (...)`
/// declaration covered by `span`. Bytes-level so we don't disturb the
/// surrounding formatting / comments. Returns the source unchanged if the
/// span doesn't contain a quoted name (defensive — `find_material_source_span`
/// only returns spans that do).
pub(super) fn rewrite_material_decl_name(
    src: &str,
    span: mogen_core::Span,
    new_name: &str,
) -> String {
    let bytes = src.as_bytes();
    let start = span.start.min(src.len());
    let end = span.end.min(src.len());
    let mut i = start;
    while i < end && bytes[i] != b'"' {
        i += 1;
    }
    if i >= end {
        return src.to_string();
    }
    let q_open = i;
    i += 1;
    while i < end && bytes[i] != b'"' {
        if bytes[i] == b'\\' && i + 1 < end {
            i += 2;
            continue;
        }
        i += 1;
    }
    if i >= end {
        return src.to_string();
    }
    let q_close = i;
    let mut out = String::with_capacity(src.len() + new_name.len());
    out.push_str(&src[..q_open + 1]);
    out.push_str(new_name);
    out.push_str(&src[q_close..]);
    out
}
