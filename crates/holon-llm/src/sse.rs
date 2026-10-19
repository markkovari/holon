//! Server-sent events, as both dialects stream their replies.

/// Remove every complete SSE frame from `buf` and return their `data`.
///
/// A frame split across chunks stays in `buf` until its blank line arrives, so the caller appends each
/// chunk to the same buffer and calls this again. `\r\n` is read as `\n`. A frame with no `data:` line
/// (a comment, an `event:` alone) yields nothing.
pub fn take_frames(buf: &mut String) -> Vec<String> {
    if buf.contains('\r') {
        *buf = buf.replace("\r\n", "\n");
    }
    let mut out = Vec::new();
    while let Some(end) = buf.find("\n\n") {
        let frame: String = buf.drain(..end + 2).collect();
        let data: Vec<&str> = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect();
        if !data.is_empty() {
            out.push(data.join("\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(buf: &mut String, chunk: &str) -> Vec<String> {
        buf.push_str(chunk);
        take_frames(buf)
    }

    #[test]
    fn a_frame_split_across_chunks_waits_for_its_blank_line() {
        let mut buf = String::new();
        assert!(collect(&mut buf, "event: x\ndata: {\"a\"").is_empty());
        assert_eq!(collect(&mut buf, ":1}\n\ndata: [DONE]\r\n\r\n"), vec!["{\"a\":1}", "[DONE]"]);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_multi_line_data_field_is_joined_and_a_frame_with_none_is_skipped() {
        let mut buf = String::new();
        assert_eq!(
            collect(&mut buf, ": keep-alive\n\nevent: ping\n\ndata: a\ndata: b\n\n"),
            vec!["a\nb"]
        );
    }
}
