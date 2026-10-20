//! Reading JSON-RPC messages from Streamable HTTP response bodies, which may
//! be plain `application/json` or a `text/event-stream`.

use futures::StreamExt;
use serde_json::Value;

use crate::mcp::protocol;

/// Feeds every JSON-RPC message in `resp` to `on_message` until it returns
/// `true` (done) or the body ends.
pub(crate) async fn read_messages(
    resp: reqwest::Response,
    mut on_message: impl FnMut(Value) -> bool,
) -> anyhow::Result<()> {
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    if !is_sse {
        let body = resp.bytes().await?;
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|e| anyhow::anyhow!("invalid JSON in MCP response: {e}"))?;
        for msg in protocol::messages(value) {
            if on_message(msg) {
                break;
            }
        }
        return Ok(());
    }

    let mut parser = SseParser::default();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        for data in parser.feed(&chunk?)? {
            if dispatch(&data, &mut on_message) {
                return Ok(());
            }
        }
    }
    if let Some(data) = parser.finish() {
        dispatch(&data, &mut on_message);
    }
    Ok(())
}

fn dispatch(data: &str, on_message: &mut impl FnMut(Value) -> bool) -> bool {
    match serde_json::from_str::<Value>(data) {
        Ok(value) => protocol::messages(value).into_iter().any(on_message),
        Err(e) => {
            tracing::debug!("skipping non-JSON SSE event: {e}");
            false
        }
    }
}

/// Largest event (or unterminated line) a stream may build up before it is rejected.
pub(crate) const MAX_EVENT_BYTES: usize = 16 << 20;

/// Incremental parser for the `data:` payloads of a server-sent event stream.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    /// Bytes of `buf` already searched for a newline.
    scanned: usize,
    data: Vec<u8>,
}

impl SseParser {
    /// Consumes a chunk and returns the payloads of the events it completed.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<String>> {
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut start = 0;
        while let Some(rel) = self.buf[self.scanned..].iter().position(|b| *b == b'\n') {
            let end = self.scanned + rel;
            let mut line = &self.buf[start..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.is_empty() {
                events.extend(take_event(&mut self.data));
            } else {
                let (field, value) = match line.iter().position(|b| *b == b':') {
                    Some(i) => (&line[..i], line[i + 1..].strip_prefix(b" ").unwrap_or(&line[i + 1..])),
                    None => (line, &b""[..]),
                };
                if field == b"data" {
                    self.data.extend_from_slice(value);
                    self.data.push(b'\n');
                }
            }
            start = end + 1;
            self.scanned = start;
        }
        self.buf.drain(..start);
        self.scanned = self.buf.len();
        if self.buf.len() + self.data.len() > MAX_EVENT_BYTES {
            anyhow::bail!("server-sent event larger than {} MiB", MAX_EVENT_BYTES >> 20);
        }
        Ok(events)
    }

    /// Flushes an event left unterminated at end of stream.
    pub(crate) fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            if let Some(event) = self.feed(b"\n").ok().and_then(|mut events| events.pop()) {
                return Some(event);
            }
        }
        take_event(&mut self.data)
    }
}

fn take_event(data: &mut Vec<u8>) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let mut bytes = std::mem::take(data);
    bytes.pop();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::{SseParser, MAX_EVENT_BYTES};

    #[test]
    fn parses_events_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.feed(b": comment\r\nevent: message\r\nid: 1\r\nda").unwrap().is_empty());
        assert_eq!(p.feed(b"ta: {\"a\":1}\r\n\r\ndata:x\ndata: y\n\n").unwrap(), vec!["{\"a\":1}", "x\ny"]);
        assert!(p.feed(b"data: tail").unwrap().is_empty());
        assert_eq!(p.feed(b"\n").unwrap(), Vec::<String>::new());
        assert_eq!(p.finish().as_deref(), Some("tail"));
    }

    #[test]
    fn utf8_split_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.feed(b"data: \xd9").unwrap().is_empty());
        assert_eq!(p.feed(b"\x85\xd8\xb1\n\n").unwrap(), vec!["\u{645}\u{631}"]);
    }

    #[test]
    fn long_line_in_small_chunks_is_one_event() {
        let mut p = SseParser::default();
        let mut stream = b"data: ".to_vec();
        stream.extend(std::iter::repeat_n(b'x', 2 << 20));
        stream.extend_from_slice(b"\n\n");
        let mut events = Vec::new();
        for chunk in stream.chunks(1024) {
            events.extend(p.feed(chunk).unwrap());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].len(), 2 << 20);
        assert_eq!(p.finish(), None);
    }

    #[test]
    fn oversized_event_is_rejected() {
        let mut p = SseParser::default();
        p.feed(b"data: ").unwrap();
        let chunk = vec![b'x'; 1 << 20];
        let mut failed = false;
        for _ in 0..=(MAX_EVENT_BYTES >> 20) {
            if p.feed(&chunk).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::default();
        assert!(p.feed(b"data: {\"done\":true}").unwrap().is_empty());
        assert_eq!(p.finish().as_deref(), Some("{\"done\":true}"));
        assert_eq!(p.finish(), None);
    }
}
