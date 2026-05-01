use std::path::{Path, PathBuf};

/// Walk upward from the CWD until we find the workspace root (the dir that
/// contains a `Cargo.toml`). Falls back to CWD when unfound. Used as the
/// default directory for the Open / Save-As file pickers so first-launch
/// dialogs start somewhere sensible.
pub(in crate::app) fn locate_project_root() -> PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cur = start.as_path();
    loop {
        if cur.join("Cargo.toml").is_file() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => return start,
        }
    }
}

pub(in crate::app) fn resolve_for_check(path: &Path, base: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base {
        Some(b) => b.join(path),
        None => path.to_path_buf(),
    }
}

/// Show "…/dir/filename.png", keeping the filename intact and ellipsizing
/// the directory prefix from the left if the whole thing is too long.
pub(in crate::app) fn ellipsize_path(path: &Path, max_chars: usize) -> String {
    let s = path.to_string_lossy();
    let n = s.chars().count();
    if n <= max_chars {
        return s.into_owned();
    }
    // Always keep the filename intact — that's the part the user actually
    // recognizes. Trim the prefix and prepend an ellipsis.
    let file_chars = path
        .file_name()
        .map(|f| f.to_string_lossy().chars().count())
        .unwrap_or(0);
    if file_chars + 1 >= max_chars {
        // Filename alone is too long; keep its tail.
        let tail: String = s.chars().rev().take(max_chars.saturating_sub(1)).collect();
        let tail: String = tail.chars().rev().collect();
        return format!("…{tail}");
    }
    let keep = max_chars.saturating_sub(file_chars + 1); // 1 for ellipsis
    let prefix_chars = n.saturating_sub(file_chars);
    let drop = prefix_chars.saturating_sub(keep);
    let visible: String = s.chars().skip(drop).collect();
    format!("…{visible}")
}

/// Trim a float to four decimals and drop trailing zeros so inspector-
/// committed values splice cleanly into the DSL. Matches
/// `viewer::format_scalar` — duplicated here because app doesn't import the
/// viewer internals.
pub(in crate::app) fn format_inspector_scalar(v: f32) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

pub(in crate::app) fn offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in src.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
