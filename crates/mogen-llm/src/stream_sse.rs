//! Minimal blocking SSE reader for streaming LLM responses.
//!
//! Both Gemini's `:streamGenerateContent?alt=sse` and OpenAI's Chat
//! Completions with `stream: true` use the standard text/event-stream
//! framing: a sequence of `field: value` lines terminated by a blank
//! line. We only care about the `data:` payload (the per-frame JSON) so
//! this helper passes the assembled payload string to a caller-supplied
//! callback once per frame, filtering out comments, empty payloads, and
//! the OpenAI-style `[DONE]` terminator.
//!
//! Per the WHATWG SSE spec, multiple `data:` lines within a single
//! event are concatenated with `\n` and dispatched together on the
//! blank-line boundary. Today both Gemini and OpenAI emit single-line
//! `data:` frames so this rarely matters, but we follow the spec so a
//! future envelope change (or a new provider) doesn't silently corrupt
//! payloads.
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
    // Accumulator for `data:` lines within the current event. The spec
    // says to join them with `\n` and dispatch on the blank-line
    // boundary; we trim the trailing `\n` before handing the payload
    // off so single-line frames stay byte-identical to the wire form.
    let mut data_buf = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        let eof = n == 0;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            // Blank line (or EOF) → dispatch the accumulated event.
            if !data_buf.is_empty() {
                // Strip the trailing `\n` the spec appends after every
                // `data:` line; for single-line frames this leaves the
                // payload exactly as it appeared on the wire.
                if data_buf.ends_with('\n') {
                    data_buf.pop();
                }
                let payload = data_buf.as_str();
                if !payload.is_empty() && payload != "[DONE]" && !on_data(payload) {
                    return Ok(());
                }
                data_buf.clear();
            }
            if eof {
                break;
            }
            continue;
        }
        // Strip `data:` (with or without the leading space servers
        // usually include). Drop other field lines silently — we don't
        // care about `event:`, `id:`, retries, or `:` comments.
        let payload = match trimmed.strip_prefix("data:") {
            Some(rest) => rest.strip_prefix(' ').unwrap_or(rest),
            None => continue,
        };
        data_buf.push_str(payload);
        data_buf.push('\n');
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

    #[test]
    fn joins_multiple_data_lines_in_one_event_with_newlines() {
        // Per the SSE spec, two `data:` lines before the blank line are
        // one event whose payload is `line1\nline2`. Neither Gemini nor
        // OpenAI ships this today, but the reader has to honour it so a
        // future provider (or a JSON payload that contains a literal
        // newline) doesn't get silently truncated to the last line.
        let body = b"data: line1\ndata: line2\n\n";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["line1\nline2".to_string()]);
    }

    #[test]
    fn dispatches_final_event_without_trailing_blank_line() {
        // Streams sometimes close mid-event (server EOFs after the last
        // `data:` line). Anything we already buffered should still fire
        // so the caller doesn't lose the tail frame.
        let body = b"data: tail";
        let mut got = Vec::new();
        for_each_sse_data(Cursor::new(&body[..]), |p| {
            got.push(p.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["tail".to_string()]);
    }
}
