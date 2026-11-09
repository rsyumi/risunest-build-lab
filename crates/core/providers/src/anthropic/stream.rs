//! The event-stream reader of `requestClaudeHTTP` (`anthropic.ts:870-1010`), including its replay of the last complete line
//! on every chunk and the `thinking` flag that survives that replay.

use super::BuildError;
use crate::common::js;
use risunest_ir::{json, Budget, Error, Text, Value};

/// Decodes the event stream. Feed it the body chunks in order; each [`StreamSession::feed`] gives the snapshot
/// `{ "0": text }` TS enqueues for that chunk, and [`StreamSession::finish`] the closing one.
pub struct StreamSession {
    text: Vec<u16>,
    /// `text` before the line processed last, which every chunk restores.
    previous_len: usize,
    parser_data: Vec<u16>,
    /// Offsets of the `\n` units in `parser_data`.
    newlines: Vec<usize>,
    /// Index into the lines, which starts at zero and is stepped back once per chunk.
    next_line: isize,
    thinking: bool,
}

/// The value a JavaScript `a?.b` read gives: own members of an object only.
fn member<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(entries) => entries.as_slice().iter().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value),
        _ => None,
    }
}

fn kind_of(value: Option<&Value>) -> Option<&Text> {
    match value { Some(Value::Text(text)) => Some(text), _ => None }
}

/// `String(value)` for a member that was appended with `+`; `undefined` and `null` mean `??` supplies `''`.
fn appended(value: Option<&Value>, nullish_is_empty: bool, budget: &Budget) -> Result<Vec<u16>, BuildError> {
    Ok(match value {
        None | Some(Value::Undefined | Value::Null) if nullish_is_empty => Vec::new(),
        None | Some(Value::Undefined) => js::utf16("undefined"),
        Some(Value::Null) => js::utf16("null"),
        Some(Value::Bool(flag)) => js::utf16(if *flag { "true" } else { "false" }),
        Some(number @ Value::Number(_)) => js::units(&number.to_text(budget)?),
        Some(Value::Text(text)) => js::units(text),
        Some(Value::Array(_) | Value::Object(_)) => return Err(BuildError::Unsupported("stream-value-not-text")),
    })
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => *number != 0.0 && !number.is_nan(),
        Value::Text(text) => text.len_units() != 0,
        Value::Array(_) | Value::Object(_) => true,
    }
}

impl Default for StreamSession {
    fn default() -> Self { Self::new() }
}

impl StreamSession {
    pub fn new() -> Self {
        Self { text: Vec::new(), previous_len: 0, parser_data: Vec::new(), newlines: Vec::new(), next_line: 0, thinking: false }
    }

    fn open_thoughts(&mut self) {
        if !self.thinking { self.text.extend(js::utf16("<Thoughts>\n")); self.thinking = true; }
    }

    /// `parseEvent`: a line that is not JSON, or throws, changes nothing.
    fn event(&mut self, source: &str, budget: &Budget) -> Result<(), BuildError> {
        let parsed = match json::parse(source, budget) {
            Ok(parsed) => parsed,
            Err(Error::Syntax { .. }) => return Ok(()),
            Err(other) => return Err(other.into()),
        };
        let kind = kind_of(member(&parsed, "type"));
        if kind.is_some_and(|kind| kind.equals_ascii(b"content_block_delta")) {
            let delta = member(&parsed, "delta");
            let delta_kind = delta.and_then(|delta| kind_of(member(delta, "type")));
            let is = |names: &[&str]| delta_kind.is_some_and(|kind| names.iter().any(|name| kind.equals_ascii(name.as_bytes())));
            if is(&["text", "text_delta"]) {
                if self.thinking { self.text.extend(js::utf16("</Thoughts>\n\n")); self.thinking = false; }
                let value = delta.and_then(|delta| member(delta, "text"));
                self.text.extend(appended(value, true, budget)?);
            }
            if is(&["thinking", "thinking_delta"]) {
                self.open_thoughts();
                let value = delta.and_then(|delta| member(delta, "thinking"));
                self.text.extend(appended(value, true, budget)?);
            }
            if is(&["redacted_thinking"]) {
                self.open_thoughts();
                self.text.extend(js::utf16("\n{{redacted_thinking}}\n"));
            }
        }
        if kind.is_some_and(|kind| kind.equals_ascii(b"error")) {
            let message = member(&parsed, "error").and_then(|error| member(error, "message"));
            if let Some(value) = message.filter(|value| truthy(value)) {
                // `errormsg.toLocaleLowerCase()` throws for anything but text, which drops the event.
                let Value::Text(text) = value else { return Ok(()) };
                // `db.antiServerOverloads` is read before the `const db` of `requestClaudeHTTP` is initialised, which throws
                // and drops the event, so the retry is never reached and the setting makes no difference.
                if super::response::overloaded(text) { return Ok(()); }
            }
            self.text.extend(js::utf16("Error:"));
            self.text.extend(appended(message, false, budget)?);
        }
        Ok(())
    }

    fn line(&self, index: usize) -> &[u16] {
        let start = if index == 0 { 0 } else { self.newlines[index - 1] + 1 };
        &self.parser_data[start..self.newlines[index]]
    }

    fn snapshot(&self, budget: &Budget) -> Result<Text, Error> { Text::from_units(&self.text, budget) }

    /// One chunk of the body: `decoder.decode(value)` without `stream`, which replaces a character cut at the chunk
    /// edge and drops a leading byte order mark.
    pub fn feed(&mut self, chunk: &[u8], budget: &Budget) -> Result<Text, BuildError> {
        let bytes = chunk.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(chunk);
        for unit in String::from_utf8_lossy(bytes).encode_utf16() {
            if unit == 10 { self.newlines.push(self.parser_data.len()); }
            self.parser_data.push(unit);
        }
        let complete = self.newlines.len() as isize;
        while self.next_line < complete {
            self.previous_len = self.text.len();
            if self.next_line >= 0 {
                let line = self.line(self.next_line as usize);
                if line.starts_with(&js::utf16("data: ")) {
                    let source = String::from_utf16_lossy(&line[6..]);
                    self.event(&source, budget)?;
                }
            }
            self.next_line += 1;
        }
        self.next_line -= 1;
        self.text.truncate(self.previous_len);
        Ok(self.snapshot(budget)?)
    }

    /// The end of the body: an open `<Thoughts>` block is closed and the text is enqueued once more.
    pub fn finish(&mut self, budget: &Budget) -> Result<Option<Text>, Error> {
        if !self.thinking { return Ok(None); }
        self.text.extend(js::utf16("</Thoughts>\n\n"));
        self.thinking = false;
        Ok(Some(self.snapshot(budget)?))
    }
}
