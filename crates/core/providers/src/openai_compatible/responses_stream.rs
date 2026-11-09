//! The event-stream reader of `requestOpenAIResponseAPI` (`getResponsesTranStream` and the display part of
//! `wrapResponsesToolStream`, `responses.ts:587-770`). A stream that ends with function calls is declined at its end,
//! when TS would run the tools.

use super::js::{self, J};
use super::responses_response::{extract_text, has_function_calls};
use super::BuildError;
use crate::common::jsprop::{read_optional, stringify, truthy, Prop};
use risunest_ir::utf8::Utf8Decoder;
use risunest_ir::{json, Budget, Error, Text};

/// `\s` of a JavaScript regular expression.
fn is_space(unit: u16) -> bool {
    matches!(unit, 9..=13 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

/// `buffer.split(/\r?\n\r?\n/)`.
fn split_events(buffer: &[u16]) -> Vec<Vec<u16>> {
    let at = |index: usize| buffer.get(index).copied();
    let mut pieces = Vec::new();
    let (mut start, mut index) = (0, 0);
    while index < buffer.len() {
        let mut end = index;
        if at(end) == Some(13) { end += 1; }
        if at(end) == Some(10) {
            end += 1;
            let mut after = end;
            if at(after) == Some(13) { after += 1; }
            if at(after) == Some(10) {
                pieces.push(buffer[start..index].to_vec());
                start = after + 1;
                index = start;
                continue;
            }
        }
        index += 1;
    }
    pieces.push(buffer[start..].to_vec());
    pieces
}

/// `rawEvent.split(/\r?\n/)`.
fn split_lines(event: &[u16]) -> Vec<&[u16]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, &unit) in event.iter().enumerate() {
        if unit == 10 {
            let end = if index > start && event[index - 1] == 13 { index - 1 } else { index };
            lines.push(&event[start..end]);
            start = index + 1;
        }
    }
    lines.push(&event[start..]);
    lines
}

fn is_blank(units: &[u16], budget: &Budget) -> Result<bool, Error> { Ok(js::from_units(units, budget)?.trim(budget)?.len_units() == 0) }

fn ends_with(text: &[u16], suffix: &[u16]) -> bool { text.ends_with(suffix) }

pub struct StreamSession {
    decoder: Utf8Decoder,
    buffer: Vec<u16>,
    text: Vec<u16>,
    reasoning: Vec<u16>,
    error: Vec<u16>,
    has_calls: bool,
    extract_json: Option<Text>,
    /// `lastValue` of the display wrapper: whether the last snapshot carried tool calls.
    last_had_calls: bool,
}

fn string_of<'a>(value: Prop<'a>) -> Result<Option<&'a Text>, BuildError> {
    match value {
        None | Some(J::Null | J::Undefined) => Ok(None),
        Some(J::Str(text)) => Ok(Some(text)),
        Some(_) => Err(BuildError::Unsupported("stream-value-not-text")),
    }
}

impl StreamSession {
    /// `extract_json` is `arg.extractJson` run through `risuChatParser`, when `arg.extractJson && (db.jsonSchemaEnabled ||
    /// arg.schema)` holds.
    pub fn new(extract_json: Option<Text>) -> Self {
        Self { decoder: Utf8Decoder::new(), buffer: Vec::new(), text: Vec::new(), reasoning: Vec::new(), error: Vec::new(), has_calls: false, extract_json, last_had_calls: false }
    }

    fn append_reasoning(&mut self, incoming: Prop<'_>) -> Result<(), BuildError> {
        if !truthy(incoming) { return Ok(()); }
        let Some(J::Str(incoming)) = incoming else { return Err(BuildError::Unsupported("stream-value-not-text")) };
        let incoming = js::units(incoming);
        if ends_with(&self.reasoning, &incoming) { return Ok(()); }
        if !self.reasoning.is_empty() { self.reasoning.extend(js::utf16("\n\n")); }
        self.reasoning.extend(incoming);
        Ok(())
    }

    /// `applyEvent`; an exception inside it is swallowed by the caller, with the changes made before it kept.
    fn apply_event(&mut self, event: &J, budget: &Budget) -> Result<(), BuildError> {
        let kind = match read_optional(Some(event), "type").unwrap_or(None) { Some(J::Str(text)) => js::units(text), _ => return Ok(()) };
        let is = |name: &str| kind == js::utf16(name);
        let prop = |key: &str| read_optional(Some(event), key).unwrap_or(None);
        if is("response.output_text.delta") || is("response.refusal.delta") {
            if let Some(delta) = string_of(prop("delta"))? { self.text.extend(js::units(delta)); }
        } else if is("response.output_text.done") && truthy(prop("text")) {
            let Some(J::Str(done)) = prop("text") else { return Err(BuildError::Unsupported("stream-value-not-text")) };
            let done = js::units(done);
            if !ends_with(&self.text, &done) { self.text = done; }
        } else if is("response.reasoning_summary_text.delta") || is("response.reasoning_text.delta") || is("response.reasoning.delta") {
            self.append_reasoning(prop("delta"))?;
        } else if (is("response.reasoning_summary_text.done") || is("response.reasoning_text.done")) && truthy(prop("text")) {
            let Some(J::Str(done)) = prop("text") else { return Err(BuildError::Unsupported("stream-value-not-text")) };
            let done = js::units(done);
            if !ends_with(&self.reasoning, &done) { self.reasoning = done; }
        } else if is("response.function_call_arguments.delta")
            || is("response.output_item.done") && matches!(prop("item"), Some(item) if matches!(read_optional(Some(item), "type").unwrap_or(None), Some(J::Str(kind)) if kind.equals_ascii(b"function_call")))
        {
            self.has_calls = true;
        } else if is("response.failed") || is("response.error") || is("error") {
            let shown = match prop("error") { Some(value) if !matches!(value, J::Null | J::Undefined) => value, _ => event };
            self.error = js::units(&stringify(shown, budget)?);
        } else if is("response.completed") {
            let response = prop("response");
            let final_text = extract_text(response, self.extract_json.as_ref(), budget)?;
            if final_text.len_units() != 0 {
                self.text = js::units(&final_text);
                if js::starts_with(&final_text, "<Thoughts>") { self.reasoning.clear(); }
            }
            if has_function_calls(response)? { self.has_calls = true; }
        }
        Ok(())
    }

    /// `emit`: the snapshot for the current state.
    fn emit(&mut self, budget: &Budget) -> Result<Text, BuildError> {
        let mut result = self.text.clone();
        if !self.reasoning.is_empty() {
            let mut shown = js::utf16("<Thoughts>\n\n");
            shown.extend(&self.reasoning);
            shown.extend(js::utf16("\n\n</Thoughts>\n"));
            shown.extend(&result);
            result = shown;
        }
        if let Some(format) = &self.extract_json {
            result = js::units(&crate::common::schema::extract_json(&js::from_units(&result, budget)?, format, budget)?);
        }
        self.last_had_calls = self.has_calls;
        let shown = if self.error.is_empty() { result } else { self.error.clone() };
        Ok(js::from_units(&shown, budget)?)
    }

    fn process(&mut self, last: bool, budget: &Budget) -> Result<(Vec<Text>, bool), BuildError> {
        let mut events = split_events(&self.buffer);
        self.buffer = events.pop().unwrap_or_default();
        if last && !is_blank(&self.buffer, budget)? {
            events.push(std::mem::take(&mut self.buffer));
        }
        let mut snapshots = Vec::new();
        let mut emitted = false;
        for event in events {
            let mut data: Vec<u16> = Vec::new();
            let mut found = false;
            for line in split_lines(&event) {
                if !line.starts_with(&js::utf16("data:")) { continue; }
                let mut rest = &line[5..];
                if rest.first().is_some_and(|&unit| is_space(unit)) { rest = &rest[1..]; }
                if found { data.push(10); }
                data.extend_from_slice(rest);
                found = true;
            }
            if !found { continue; }
            if data != js::utf16("[DONE]") {
                let source = String::from_utf16_lossy(&data);
                match json::parse(&source, budget) {
                    Ok(parsed) => {
                        let event = js::from_value(&parsed, budget)?;
                        match self.apply_event(&event, budget) {
                            Ok(()) | Err(BuildError::Thrown(_)) => {}
                            Err(other) => return Err(other),
                        }
                    }
                    Err(Error::Syntax { .. }) => {}
                    Err(other) => return Err(other.into()),
                }
            }
            snapshots.push(self.emit(budget)?);
            emitted = true;
        }
        Ok((snapshots, emitted))
    }

    /// One chunk of the body; the snapshots TS enqueues for it.
    pub fn feed(&mut self, chunk: &[u8], budget: &Budget) -> Result<Vec<Text>, BuildError> {
        let decoded = self.decoder.decode(chunk, false, budget)?;
        self.buffer.extend(decoded.units());
        Ok(self.process(false, budget)?.0)
    }

    /// The end of the body (`flush`): the remaining text is read as one event, and a stream that gave no snapshot at all
    /// gives one. A stream whose last snapshot carried function calls ends with a decline, since TS runs the tools then.
    pub fn finish(&mut self, budget: &Budget) -> Result<StreamEnd, BuildError> {
        let decoded = self.decoder.decode(&[], true, budget)?;
        self.buffer.extend(decoded.units());
        let (mut snapshots, emitted) = self.process(true, budget)?;
        if !emitted { snapshots.push(self.emit(budget)?); }
        Ok(StreamEnd { snapshots, decline: self.last_had_calls.then_some("tool-round") })
    }
}

/// What the end of the body gives.
#[derive(Debug)]
pub struct StreamEnd {
    pub snapshots: Vec<Text>,
    /// Set when the stream asks for tools, which the native path leaves to the TS one.
    pub decline: Option<&'static str>,
}
