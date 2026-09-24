//! Just enough of a server-sent events parser for the relay's event stream:
//! collects `data:` lines into messages and skips everything else (comments,
//! `event:`, `id:`, `retry:`).

#[derive(Default)]
pub struct Parser {
    /// Bytes after the last complete line.
    partial: Vec<u8>,
    data: Option<String>,
}

impl Parser {
    /// Feed received bytes; returns the `data` of every message they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.partial.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some(end) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            if line.is_empty() {
                messages.extend(self.data.take());
            } else if let Some(value) = line.strip_prefix("data") {
                if let Some(value) = value.strip_prefix(':').or(value.is_empty().then_some("")) {
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    let data = self.data.get_or_insert_with(String::new);
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
            }
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_data_and_skips_the_rest() {
        let mut p = Parser::default();
        assert_eq!(p.feed(b": keep-alive\n\n"), Vec::<String>::new());
        assert_eq!(p.feed(b"event: x\ndata: {\"a\":1}\n\n"), ["{\"a\":1}"]);
        assert_eq!(
            p.feed(b"data:one\r\ndata: two\r\n\r\ndata: 3\n\n"),
            ["one\ntwo", "3"]
        );
        assert!(p.feed(b"database: no\n\n").is_empty());
    }

    #[test]
    fn handles_messages_split_across_chunks() {
        let mut p = Parser::default();
        assert!(p.feed(b"da").is_empty());
        assert!(p.feed(b"ta: hel").is_empty());
        assert!(p.feed(b"lo\n").is_empty());
        assert_eq!(p.feed(b"\ndata: next"), ["hello"]);
        assert_eq!(p.feed(b"\n\n"), ["next"]);
    }
}
