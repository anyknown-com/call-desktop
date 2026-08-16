//! Minimal Server-Sent Events decoder (the subset OpenAI/Anthropic emit).

/// One SSE event. `event` is `None` when the stream did not send an `event:` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental decoder: feed bytes as they arrive, get complete events back.
/// Handles `\n` and `\r\n` line endings, multi-line `data:` (joined with `\n`),
/// comments (`:` lines) and events split across chunk boundaries.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let mut line = &line[..line.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if let Some(ev) = self.line(&String::from_utf8_lossy(line)) {
                out.push(ev);
            }
        }
        out
    }

    /// Flush a trailing event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        self.buf.clear();
        self.dispatch()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        let ev = SseEvent {
            event: self.event.take(),
            data: self.data.join("\n"),
        };
        self.data.clear();
        Some(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.map(String::from),
            data: data.to_string(),
        }
    }

    #[test]
    fn single_events_and_done() {
        let mut d = SseDecoder::new();
        let got = d.push(b"data: {\"a\":1}\n\ndata: [DONE]\n\n");
        assert_eq!(got, vec![ev(None, "{\"a\":1}"), ev(None, "[DONE]")]);
    }

    #[test]
    fn split_across_chunks_and_crlf() {
        let mut d = SseDecoder::new();
        assert!(d
            .push(b"event: content_block_delta\r\ndata: {\"te")
            .is_empty());
        assert!(d.push(b"xt\":\"hi\"}\r\n").is_empty());
        let got = d.push(b"\r\n");
        assert_eq!(
            got,
            vec![ev(Some("content_block_delta"), "{\"text\":\"hi\"}")]
        );
    }

    #[test]
    fn multi_line_data_and_comments() {
        let mut d = SseDecoder::new();
        let got = d.push(b": keep-alive\n\ndata: line1\ndata:line2\ndata\n\n");
        assert_eq!(got, vec![ev(None, "line1\nline2\n")]);
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut d = SseDecoder::new();
        assert!(d.push(b"data: tail\n").is_empty());
        assert_eq!(d.finish(), Some(ev(None, "tail")));
        assert_eq!(d.finish(), None);
    }
}
