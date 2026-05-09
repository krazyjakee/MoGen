use eframe::egui;
use egui::text::{CCursor, CCursorRange};
use egui::{Key, KeyboardShortcut, Modifiers};

/// Wrap a `TextEdit` add call with the standard Cut/Copy/Paste/Delete/Select All
/// context menu. The `add` closure builds the widget and must call `.id(id)` on
/// the `TextEdit` so this helper can look the selection state up later.
///
/// Selection is snapshotted *before* the widget runs so right-click can restore
/// it — egui's pointer handling collapses the selection on any button press
/// (secondary included), which would otherwise leave the menu unable to see
/// what the user had highlighted.
///
/// Menu-driven mutations call `Response::mark_changed()` so callers that gate
/// dirty/recompile on `resp.changed()` get a uniform signal for typed edits
/// and cut/paste/delete alike.
pub(super) fn text_edit_with_menu(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    add: impl FnOnce(&mut egui::Ui, &mut String) -> egui::Response,
) -> egui::Response {
    let prior = egui::TextEdit::load_state(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range());

    let mut resp = add(ui, text);

    if resp.hovered() && ui.input(|i| i.pointer.secondary_pressed()) {
        if let Some(range) = prior {
            if range.primary.index != range.secondary.index {
                if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                    st.cursor.set_char_range(Some(range));
                    st.store(ui.ctx(), id);
                }
            }
        }
    }

    let mut menu_changed = false;
    resp.context_menu(|ui| {
        if show_context_menu(ui, id, text) {
            menu_changed = true;
        }
    });
    if menu_changed {
        resp.mark_changed();
    }
    resp
}

/// Cut/Copy/Paste/Delete/Select All for any `TextEdit` that has been assigned
/// `id`. Safe to call from inside a `Response::context_menu` closure. Returns
/// `true` when the source was mutated.
pub(super) fn show_context_menu(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
) -> bool {
    let sel_range = egui::TextEdit::load_state(ui.ctx(), id)
        .and_then(|s| s.cursor.char_range());
    let (sel_lo, sel_hi) = match sel_range {
        Some(range) => {
            let [lo, hi] = range.sorted();
            let lo_b = text
                .char_indices()
                .nth(lo.index)
                .map(|(b, _)| b)
                .unwrap_or(text.len());
            let hi_b = text
                .char_indices()
                .nth(hi.index)
                .map(|(b, _)| b)
                .unwrap_or(text.len());
            (lo_b, hi_b)
        }
        None => (0, 0),
    };
    let has_selection = sel_hi > sel_lo;

    let store_caret = |ui: &egui::Ui, new_byte: usize, source: &str| {
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
            let new_char = source[..new_byte.min(source.len())].chars().count();
            st.cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(new_char))));
            st.store(ui.ctx(), id);
        }
    };

    let mut changed = false;

    // Render shortcuts via egui's formatter so macOS users see ⌘ rather than
    // the literal "Ctrl+…" the menu used to hard-code.
    let cmd = Modifiers::COMMAND;
    let sc_cut = ui.ctx().format_shortcut(&KeyboardShortcut::new(cmd, Key::X));
    let sc_copy = ui.ctx().format_shortcut(&KeyboardShortcut::new(cmd, Key::C));
    let sc_paste = ui.ctx().format_shortcut(&KeyboardShortcut::new(cmd, Key::V));
    let sc_delete = ui
        .ctx()
        .format_shortcut(&KeyboardShortcut::new(Modifiers::NONE, Key::Delete));
    let sc_select_all = ui.ctx().format_shortcut(&KeyboardShortcut::new(cmd, Key::A));

    if ui
        .add_enabled(has_selection, egui::Button::new("Cut").shortcut_text(sc_cut))
        .clicked()
    {
        ui.ctx().copy_text(text[sel_lo..sel_hi].to_string());
        text.replace_range(sel_lo..sel_hi, "");
        store_caret(ui, sel_lo, text);
        changed = true;
        ui.close_menu();
    }

    if ui
        .add_enabled(
            has_selection,
            egui::Button::new("Copy").shortcut_text(sc_copy),
        )
        .clicked()
    {
        ui.ctx().copy_text(text[sel_lo..sel_hi].to_string());
        ui.close_menu();
    }

    if ui
        .add(egui::Button::new("Paste").shortcut_text(sc_paste))
        .clicked()
    {
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(t) if !t.is_empty() => {
                text.replace_range(sel_lo..sel_hi, &t);
                let new_byte = sel_lo + t.len();
                store_caret(ui, new_byte, text);
                changed = true;
            }
            Ok(_) => {}
            Err(e) => {
                // Surface clipboard failures so the user isn't left wondering
                // why nothing happened. Most common cause is a Wayland session
                // without a clipboard manager.
                eprintln!("paste from clipboard failed: {e}");
            }
        }
        ui.close_menu();
    }

    if ui
        .add_enabled(
            has_selection,
            egui::Button::new("Delete").shortcut_text(sc_delete),
        )
        .clicked()
    {
        text.replace_range(sel_lo..sel_hi, "");
        store_caret(ui, sel_lo, text);
        changed = true;
        ui.close_menu();
    }

    ui.separator();

    if ui
        .add(egui::Button::new("Select All").shortcut_text(sc_select_all))
        .clicked()
    {
        let total_chars = text.chars().count();
        if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
            st.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(0),
                CCursor::new(total_chars),
            )));
            st.store(ui.ctx(), id);
            ui.ctx().memory_mut(|m| m.request_focus(id));
        }
        ui.close_menu();
    }

    changed
}
