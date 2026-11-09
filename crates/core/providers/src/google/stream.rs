//! The event-stream reader of `requestGoogle` (`getTranStream` and the display part of `wrapToolStream`,
//! `google.ts:1000-1330`), including its replay of the whole buffer on every chunk: a line that cannot be read, or the
//! `[DONE]` line, drops the chunk's snapshot and every later one. A response that asks for tools or carries a thought
//! signature to save is declined.

use super::BuildError;
use crate::common::js::{self, J};
use crate::common::jsprop::{nullish, read, read_optional, truthy, Thrown};
use crate::common::jsprop::stringify;
use risunest_ir::{json, Budget, Error, Text};

/// `readed['__usageMetadata']` and `readed['__modelStatus']`: the `JSON.stringify` of the last values seen.
#[derive(Clone, Debug, Default)]
pub struct StreamMeta {
    pub usage_metadata: Option<String>,
    pub model_status: Option<String>,
}

#[derive(Clone, Default)]
struct State {
    content: Vec<u16>,
    thoughts: Vec<u16>,
    last_thought: Vec<u16>,
    meta: StreamMeta,
}

/// What reading one line does to the chunk.
enum Flow { Continue, Drop }

pub struct StreamSession {
    units: Vec<u16>,
    /// The index after the last `\n` that has been applied.
    committed: usize,
    state: State,
    /// A complete line failed: every later chunk fails in the same place.
    poisoned: bool,
    stream_thoughts: bool,
    save_signatures: bool,
    last_meta: Option<StreamMeta>,
}

impl StreamSession {
    /// `stream_thoughts` is `db.streamGeminiThoughts`; `save_signatures` is `arg.saveSignatures`, which declines a
    /// signature on a text part.
    pub fn new(stream_thoughts: bool, save_signatures: bool) -> Self {
        Self { units: Vec::new(), committed: 0, state: State::default(), poisoned: false, stream_thoughts, save_signatures, last_meta: None }
    }

    /// The `meta_gemini_stream` payload for the interceptors, once the body has ended.
    pub fn finish(self) -> Option<StreamMeta> { self.last_meta }

    fn snapshot(&self, state: &State, budget: &Budget) -> Result<Text, Error> {
        let mut out: Vec<u16> = Vec::new();
        let both = [state.thoughts.as_slice(), state.last_thought.as_slice()].concat();
        if self.stream_thoughts {
            if !state.thoughts.is_empty() {
                out.extend(js::utf16("<Thoughts>\n\n")); out.extend(&state.thoughts); out.extend(js::utf16("\n\n</Thoughts>\n\n"));
            }
            if !state.last_thought.is_empty() { out.extend(&state.last_thought); out.extend(js::utf16("\n\n")); }
        } else if !both.is_empty() {
            out.extend(js::utf16("<Thoughts>\n\n")); out.extend(&both); out.extend(js::utf16("\n\n</Thoughts>\n\n"));
        }
        out.extend(&state.content);
        Text::from_units(&out, budget)
    }

    fn apply(&self, state: &mut State, line: &[u16], budget: &Budget) -> Result<Flow, BuildError> {
        if !line.starts_with(&js::utf16("data: ")) { return Ok(Flow::Continue); }
        let trimmed = Text::from_units(&line[6..], budget)?.trim(budget)?;
        if trimmed.equals_ascii(b"[DONE]") { return Ok(Flow::Drop); }
        let source = String::from_utf16_lossy(&js::units(&trimmed));
        let parsed = match json::parse(&source, budget) {
            Ok(value) => value,
            Err(Error::Syntax { .. }) => return Ok(Flow::Drop),
            Err(other) => return Err(other.into()),
        };
        let data = js::from_value(&parsed, budget)?;
        // Any exception inside the `try` drops the chunk.
        let chain = (|| -> Result<Option<&J>, Thrown> {
            let candidates = read(Some(&data), "candidates")?;
            let first = read_optional(candidates, "0")?;
            let content = read_optional(first, "content")?;
            read_optional(content, "parts")
        })();
        let Ok(parts) = chain else { return Ok(Flow::Drop) };
        if truthy(parts) {
            let list: &[J] = match parts {
                Some(J::Arr(items)) => items,
                // `for (const part of "text")` visits characters, none of which has a member.
                Some(J::Str(_)) => &[],
                _ => return Ok(Flow::Drop),
            };
            for part in list {
                if nullish(Some(part)) { return Ok(Flow::Drop); }
                let Ok(text) = read(Some(part), "text") else { return Ok(Flow::Drop) };
                if truthy(text) {
                    let Some(J::Str(shown)) = text else { return Err(BuildError::Unsupported("part-text-not-text")) };
                    let thought = read(Some(part), "thought").map(|value| truthy(value)).unwrap_or(false);
                    let last = std::mem::take(&mut state.last_thought);
                    state.thoughts.extend(last);
                    if thought { state.last_thought = js::units(shown); } else { state.content.extend(js::units(shown)); }
                    if truthy(read(Some(part), "thoughtSignature").unwrap_or(None)) && self.save_signatures {
                        return Err(BuildError::Unsupported("signature-response"));
                    }
                }
                if truthy(read(Some(part), "functionCall").unwrap_or(None)) { return Err(BuildError::Unsupported("tool-round")); }
            }
        }
        let usage = read(Some(&data), "usageMetadata").unwrap_or(None);
        if truthy(usage) { state.meta.usage_metadata = Some(String::from_utf16_lossy(&js::units(&stringify(usage.expect("truthy"), budget)?))); }
        let status = read(Some(&data), "modelStatus").unwrap_or(None);
        if truthy(status) { state.meta.model_status = Some(String::from_utf16_lossy(&js::units(&stringify(status.expect("truthy"), budget)?))); }
        Ok(Flow::Continue)
    }

    /// One chunk of the body: `decoder.decode(value)` without `stream`, which replaces a character cut at the chunk edge
    /// and drops a leading byte order mark. `None` is a chunk TS enqueues nothing for.
    pub fn feed(&mut self, chunk: &[u8], budget: &Budget) -> Result<Option<Text>, BuildError> {
        let bytes = chunk.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(chunk);
        self.units.extend(String::from_utf8_lossy(bytes).encode_utf16());
        if self.poisoned { return Ok(None); }
        while let Some(offset) = self.units[self.committed..].iter().position(|&unit| unit == 10) {
            let end = self.committed + offset;
            let line = self.units[self.committed..end].to_vec();
            let mut state = std::mem::take(&mut self.state);
            let flow = self.apply(&mut state, &line, budget);
            self.state = state;
            self.committed = end + 1;
            match flow? { Flow::Continue => {} Flow::Drop => { self.poisoned = true; return Ok(None); } }
        }
        let mut state = self.state.clone();
        let tail = self.units[self.committed..].to_vec();
        if let Flow::Drop = self.apply(&mut state, &tail, budget)? { return Ok(None); }
        let snapshot = self.snapshot(&state, budget)?;
        if state.meta.usage_metadata.is_some() || state.meta.model_status.is_some() { self.last_meta = Some(state.meta.clone()); }
        Ok(Some(snapshot))
    }
}
