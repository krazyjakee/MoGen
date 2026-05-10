//! Minimal blocking SSE reader for streaming LLM responses.
//!
//! Both Gemini's `:streamGenerateContent?alt=sse` and OpenAI's Chat
//! Completions with `stream: true` use the standard text/event-stream
//! framing: a sequence of `field: value` lines terminated by a blank
//! line. We only care about the `data:` payload (the per-frame JSON) so
//! this helper passes the trimmed payload string to a caller-supplied
//! callback once per frame, filtering out comments, empty payloads, and
//! the OpenAI-style `[DONE]` terminator.
//!
//! The callback returns `bool`: `false` stops reading early — used by
//! callers that need to abort mid-stream (cancel button, budget tripped
//! in a streamed usage frame, etc.). Stops automatically on EOF.
//!
//! No tokio — `BufRead::read_line` on a `reqwest::blocking::Response`
//! is enough for the blocking client this crate already runs on.

use std::io::{BufRead, BufReader, Read};

/// Drive `on_data` once per SSE frame's `data:` payload. Returns when
/// the stream EOFs or the callback returns `false`. Bubbles up the
/// first read error.
pub fn for_each_sse_data<R, F>(reader: R, mut on_data: F) -> std::io::Result<()>
where
    R: Read,
    F: FnMut(&str) -> bool,
{
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            // Frame boundary — Gemini emits one frame per `data:` line so
            // we don't need to buffer multi-line frames.
            continue;
        }
        // Strip `data:` (with or without the leading space servers usually
        // include). Drop other field lines silently — we don't care about
        // `event:`, `id:`, retries, or `:` comments.
        let payload = match trimmed.strip_prefix("data:") {
            Some(rest) => rest.trim_start(),
            None => continue,
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if !on_data(payload) {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn yields_each_data_payload() {
        let body = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\n";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
    }

    #[test]
    fn skips_done_terminator_and_blank_data() {
        let body = b"data: hello\n\ndata:\n\ndata: [DONE]\n\n";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["hello".to_string()]);
    }

    #[test]
    fn ignores_non_data_lines() {
        let body = b"event: ping\n: comment\ndata: payload\n\n";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["payload".to_string()]);
    }

    #[test]
    fn callback_returning_false_stops_iteration() {
        let body = b"data: 1\n\ndata: 2\n\ndata: 3\n\n";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            p != "2" // stop after the 2nd frame
        })
        .unwrap();
        assert_eq!(got, vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn tolerates_data_with_no_leading_space() {
        let body = b"data:tight\n\n";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["tight".to_string()]);
    }
}
