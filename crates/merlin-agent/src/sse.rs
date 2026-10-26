//! Server-sent events (the WHATWG `text/event-stream` format) as the model APIs send them: `event:`
//! and `data:` fields, events separated by a blank line. Comments (`:`) and `id:` / `retry:` are
//! ignored, multi-line `data:` is joined with `\n`.

/// One event: its type (`message` when the stream doesn't name one) and data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub event: String,
    pub data: String,
}

/// Feed lines (without their line terminator); complete events come out.
#[derive(Default)]
pub struct Parser {
    event: Option<String>,
    data: Option<String>,
}

/// Cap on one event's data; a runaway stream is an error, not an allocation without end.
pub const MAX_EVENT_BYTES: usize = 32 << 20;

impl Parser {
    /// Process one line. Returns a finished event at a blank line, or an error when an event grows
    /// past [`MAX_EVENT_BYTES`].
    pub fn line(&mut self, line: &str) -> Result<Option<Event>, String> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            return Ok(self.finish());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                let data = self.data.get_or_insert_with(String::new);
                if data.len().saturating_add(value.len()) > MAX_EVENT_BYTES {
                    return Err(format!("an event is larger than {MAX_EVENT_BYTES} bytes"));
                }
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            _ => {}
        }
        Ok(None)
    }

    /// Flush an event left open when the stream ends without a final blank line.
    pub fn finish(&mut self) -> Option<Event> {
        let event = self.event.take();
        let data = self.data.take()?;
        Some(Event { event: event.unwrap_or_else(|| "message".into()), data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<Event> {
        let mut p = Parser::default();
        let mut out: Vec<Event> = text.split('\n').filter_map(|l| p.line(l).unwrap()).collect();
        out.extend(p.finish());
        out
    }

    #[test]
    fn named_events_and_data() {
        let ev = parse("event: message_start\ndata: {\"a\":1}\n\nevent: ping\ndata: {}\n\n");
        assert_eq!(ev, vec![Event { event: "message_start".into(), data: "{\"a\":1}".into() }, Event { event: "ping".into(), data: "{}".into() }]);
    }

    #[test]
    fn unnamed_multiline_crlf_comments() {
        let ev = parse(": keep-alive\r\ndata: one\r\ndata:two\r\n\r\ndata: [DONE]");
        assert_eq!(ev, vec![Event { event: "message".into(), data: "one\ntwo".into() }, Event { event: "message".into(), data: "[DONE]".into() }]);
    }

    #[test]
    fn blank_lines_without_data_emit_nothing() {
        assert!(parse("\n\n\nevent: x\n\n").is_empty());
    }
}
