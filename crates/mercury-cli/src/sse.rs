//! A reader for `text/event-stream`, the format of a question's events: `event:`, `id:` and
//! `data:` lines, a blank line after each event, `:` for a comment. Lines end with CRLF, LF or
//! CR, and a chunk of the stream may end anywhere, even between the CR and the LF.

/// The longest event the reader keeps; a longer one is an error, not a growing buffer.
pub const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;

/// One event as the stream sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The `event:` name; `message` when the event had none.
    pub event: String,
    pub data: String,
    /// The stream's last event id when this event arrived, for `Last-Event-ID` on a reconnect.
    pub last_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SseError {
    #[error("an event is longer than {MAX_EVENT_BYTES} bytes")]
    TooLong,
    #[error("the stream isn't UTF-8")]
    NotUtf8,
}

#[derive(Debug, Default)]
pub struct Reader {
    line: Vec<u8>,
    /// The last byte was a CR, so an LF right after it ends no second line.
    after_cr: bool,
    first_line: bool,
    event: String,
    data: String,
    has_data: bool,
    size: usize,
    last_id: Option<String>,
}

impl Reader {
    pub fn new() -> Self {
        Self {
            first_line: true,
            ..Self::default()
        }
    }

    /// Takes the next bytes of the stream and gives back the events they complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, SseError> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\r' => {
                    self.after_cr = true;
                    self.end_line(&mut frames)?;
                }
                b'\n' => self.end_line(&mut frames)?,
                _ if self.size + self.line.len() >= MAX_EVENT_BYTES => return Err(SseError::TooLong),
                _ => self.line.push(byte),
            }
        }
        Ok(frames)
    }

    fn end_line(&mut self, frames: &mut Vec<Frame>) -> Result<(), SseError> {
        let mut bytes = std::mem::take(&mut self.line);
        if std::mem::take(&mut self.first_line) && bytes.starts_with(b"\xEF\xBB\xBF") {
            bytes.drain(..3);
        }
        let line = String::from_utf8(bytes).map_err(|_| SseError::NotUtf8)?;
        if line.is_empty() {
            self.dispatch(frames);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        self.size += line.len();
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_str(), ""),
        };
        match field {
            "event" => value.clone_into(&mut self.event),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.last_id = Some(value.to_owned()),
            // `retry` and unknown fields: the CLI keeps its own pace
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, frames: &mut Vec<Frame>) {
        let event = std::mem::take(&mut self.event);
        let mut data = std::mem::take(&mut self.data);
        self.size = 0;
        if !std::mem::take(&mut self.has_data) {
            return;
        }
        data.pop();
        frames.push(Frame {
            event: if event.is_empty() {
                "message".to_owned()
            } else {
                event
            },
            data,
            last_id: self.last_id.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(chunks: &[&[u8]]) -> Vec<Frame> {
        let mut reader = Reader::new();
        chunks
            .iter()
            .flat_map(|chunk| reader.feed(chunk).unwrap())
            .collect()
    }

    fn frame(event: &str, data: &str, last_id: Option<&str>) -> Frame {
        Frame {
            event: event.to_owned(),
            data: data.to_owned(),
            last_id: last_id.map(str::to_owned),
        }
    }

    #[test]
    fn an_event_is_its_name_its_data_and_its_id() {
        let got = frames(&[b"event: classified\nid: 4\ndata: {\"class\":\"progress\"}\n\n"]);
        assert_eq!(got, [frame("classified", "{\"class\":\"progress\"}", Some("4"))]);
    }

    #[test]
    fn every_line_ending_counts_and_a_chunk_may_split_anywhere() {
        let text = b"event: a\r\ndata: 1\r\n\r\nevent: b\rdata: 2\r\revent: c\ndata: 3\n\n";
        let whole = frames(&[text]);
        assert_eq!(
            whole,
            [
                frame("a", "1", None),
                frame("b", "2", None),
                frame("c", "3", None)
            ]
        );
        for split in 1..text.len() {
            let (head, tail) = text.split_at(split);
            assert_eq!(frames(&[head, tail]), whole, "split at {split}");
        }
        let bytewise: Vec<&[u8]> = text.chunks(1).collect();
        assert_eq!(frames(&bytewise), whole);
    }

    #[test]
    fn data_lines_join_and_comments_are_skipped() {
        let got = frames(&[b": keep-alive\ndata: one\ndata:two\ndata\n\n"]);
        assert_eq!(got, [frame("message", "one\ntwo\n", None)]);
    }

    #[test]
    fn the_last_id_carries_over_until_the_next_one() {
        let got =
            frames(&[b"id: 1\nevent: a\ndata: x\n\nevent: b\ndata: y\n\nid: 7\n\nevent: c\ndata: z\n\n"]);
        assert_eq!(
            got,
            [
                frame("a", "x", Some("1")),
                frame("b", "y", Some("1")),
                frame("c", "z", Some("7"))
            ]
        );
    }

    #[test]
    fn an_event_without_data_is_dropped_with_its_name() {
        let got = frames(&[b"event: lost\n\ndata: kept\n\n"]);
        assert_eq!(got, [frame("message", "kept", None)]);
    }

    #[test]
    fn a_byte_order_mark_opens_the_stream_only() {
        let got = frames(&[b"\xEF\xBB\xBFevent: a\ndata: 1\n\n"]);
        assert_eq!(got, [frame("a", "1", None)]);
    }

    #[test]
    fn an_unfinished_event_waits_for_its_blank_line() {
        let mut reader = Reader::new();
        assert!(reader.feed(b"event: a\ndata: 1\n").unwrap().is_empty());
        assert_eq!(reader.feed(b"\n").unwrap(), [frame("a", "1", None)]);
    }

    #[test]
    fn an_endless_event_or_bad_bytes_are_errors() {
        let mut reader = Reader::new();
        let line = vec![b'x'; MAX_EVENT_BYTES + 1];
        assert_eq!(reader.feed(&line), Err(SseError::TooLong));

        let mut reader = Reader::new();
        let many: Vec<u8> = b"data: 0123456789\n".repeat(MAX_EVENT_BYTES / 16 + 1);
        assert_eq!(reader.feed(&many), Err(SseError::TooLong));

        assert_eq!(Reader::new().feed(b"data: \xFF\n"), Err(SseError::NotUtf8));
    }
}
