//! Pure helpers — URL parsing, image decoding, bbcode stripping, and
//! `MoghubError` formatting. No I/O, no reaching into `MogenStudioApp`.

use std::path::PathBuf;

use mogen_moghub_client::MoghubError;

/// Pull the slug out of a canonical `/m/<user>/<slug>` URL path returned
/// by `POST /api/models`. Returns `None` if the path doesn't have at
/// least three segments — covers a server change in the URL shape
/// without panicking.
pub(super) fn slug_from_url_path(url_path: &str) -> Option<String> {
    let segments: Vec<&str> = url_path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match segments.as_slice() {
        ["m", _user, slug, ..] => Some((*slug).to_string()),
        _ => None,
    }
}

/// Pick a unique temp path for one publish-dialog thumbnail render. Suffixed
/// with the pid + nanos so back-to-back open/cancel cycles don't trip over a
/// previous run's leftover file.
pub(super) fn publish_thumb_temp_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "mogen-publish-thumb-{}-{nanos}.png",
        std::process::id()
    ))
}

/// Decode arbitrary image bytes (PNG/JPG/WebP — whatever GitHub or our
/// own server returns) into an RGBA8 buffer + size suitable for
/// `ColorImage::from_rgba_unmultiplied`.
pub(super) fn decode_image(bytes: &[u8]) -> Result<([usize; 2], Vec<u8>), image::ImageError> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok((size, rgba.into_raw()))
}

/// Drop bbcode tags so a comment body renders cleanly in plain egui
/// text. Crude but enough for the in-app preview — the web client
/// renders the same bodies via a server-side bbcode parser.
pub(super) fn strip_bbcode(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            // Skip to the matching `]`. Anything else (no `]` ever, or a
            // newline first) we just emit verbatim so we don't lose
            // user content.
            let mut tag = String::new();
            let mut closed = false;
            for c2 in chars.by_ref() {
                if c2 == ']' {
                    closed = true;
                    break;
                }
                if c2 == '\n' {
                    out.push('[');
                    out.push_str(&tag);
                    out.push('\n');
                    closed = true;
                    break;
                }
                tag.push(c2);
            }
            if !closed {
                out.push('[');
                out.push_str(&tag);
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub(super) fn kind_badge(kind: &str) -> String {
    match kind {
        "scene" => "scene".to_string(),
        "model" => "model".to_string(),
        "module" => "module".to_string(),
        other => other.to_string(),
    }
}

pub(super) fn format_err(e: &MoghubError) -> String {
    match e {
        MoghubError::Network(s) => format!("couldn't reach MoGHub: {s}"),
        MoghubError::Status { code, body } => {
            if body.is_empty() {
                format!("server error {code}")
            } else {
                format!("server error {code}: {body}")
            }
        }
        MoghubError::Unauthorized => "sign-in required".to_string(),
        MoghubError::Decode(s) => format!("decode error: {s}"),
    }
}
