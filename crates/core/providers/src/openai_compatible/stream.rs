//! The SSE decoder of `getTranStream` and the display projection of `wrapToolStream`
//! (`requests.ts:961-1307`).
//!
//! TS re-decodes the whole received buffer on every chunk and rebuilds its state from scratch, so each emitted value
//! is a cumulative snapshot and a failing line is dropped silently. [`SseDecoder`] keeps the state of the complete
//! lines and applies the unfinished last line to a copy, which yields the same snapshots without re-reading old lines.

use super::js::{self, J};
use super::response::{nullish, read, read_optional, remove_think_open, split_think, thoughts_block, to_text, truthy, Failed, ToolCallEntry, AssistantMessage};
use super::schema::extract_json;
use super::BuildError;
use risunest_ir::provider::LlmFlag;
use risunest_ir::{json, Budget, Error, Text};

/// What the decoder reads from the request.
#[derive(Debug)]
pub struct StreamContext {
    /// `modelInfo.flags`.
    pub flags: Vec<LlmFlag>,
    /// `arg.multiGen`.
    pub multi_gen: bool,
    /// `arg.extractJson` after `risuChatParser`, when `arg.extractJson && (db.jsonSchemaEnabled || arg.schema)`.
    pub extract_json: Option<Text>,
}

impl StreamContext {
    fn copy(&self, budget: &Budget) -> Result<Self, Error> {
        Ok(Self { flags: self.flags.clone(), multi_gen: self.multi_gen, extract_json: self.extract_json.as_ref().map(|text| text.copy(budget)).transpose()? })
    }
}

/// One tool call accumulated from `delta.tool_calls`. `id` and `name` hold the JSON value a delta carried (`null` until
/// one does), which can be any truthy JSON value.
#[derive(Debug)]
pub struct StreamToolCall {
    pub(crate) id: J,
    pub(crate) name: J,
    pub arguments: Text,
}

/// One value `getTranStream` enqueues: the texts by output index (`"0"` is the displayed text) and the tool calls.
#[derive(Debug)]
pub struct StreamValue {
    pub texts: Vec<(Text, Text)>,
    pub tool_calls: Vec<StreamToolCall>,
    /// Whether `__tool_calls` was present, which can be true with an empty list.
    pub has_tool_calls: bool,
    /// With `extractJson` set, `__tool_calls` went through `extractJSON` and `wrapToolStream` parses the result: this is
    /// the text `JSON.parse` receives when it is not empty (and therefore throws).
    pub unparsable_tool_calls: Option<Text>,
}

#[derive(Debug)]
struct ToolAcc { id: J, name: J, arguments: Text }

#[derive(Debug)]
struct State {
    readed: Vec<(Text, Text)>,
    tools: Option<Vec<(Text, ToolAcc)>>,
    reasoning: Text,
}

fn copy_tools(list: &[(Text, ToolAcc)], budget: &Budget) -> Result<Vec<(Text, ToolAcc)>, Error> {
    list.iter().map(|(key, tool)| Ok((key.copy(budget)?, ToolAcc {
        id: js::clone_j(&tool.id, budget)?, name: js::clone_j(&tool.name, budget)?, arguments: tool.arguments.copy(budget)?,
    }))).collect()
}

impl State {
    fn new(budget: &Budget) -> Result<Self, Error> { Ok(Self { readed: Vec::new(), tools: None, reasoning: js::text("", budget)? }) }
    fn copy(&self, budget: &Budget) -> Result<Self, Error> {
        Ok(Self {
            readed: self.readed.iter().map(|(key, value)| Ok((key.copy(budget)?, value.copy(budget)?))).collect::<Result<_, Error>>()?,
            tools: self.tools.as_ref().map(|list| copy_tools(list, budget)).transpose()?,
            reasoning: self.reasoning.copy(budget)?,
        })
    }
}

enum Line { Continue, Done(Result<StreamValue, Failed>) }

/// `appendStreamingFragment(current, incoming)` for an `incoming` value that is already known truthy.
fn append_fragment(current: &Text, incoming: &J, budget: &Budget) -> Result<Text, Failed> {
    match incoming {
        J::Str(text) => {
            if text.len_units() > current.len_units() && text.units().take(current.len_units()).eq(current.units()) { return Ok(text.copy(budget)?); }
            Ok(js::concat(current, text, budget)?)
        }
        J::Arr(items) => {
            if items.len() > current.len_units() { return Err(Failed::Thrown); }
            Ok(js::concat(current, &to_text(incoming, budget)?, budget)?)
        }
        other => Ok(js::concat(current, &to_text(other, budget)?, budget)?),
    }
}

/// Position of `key` in an object kept in JavaScript key order, adding an empty entry when it is missing.
fn object_key(entries: &mut Vec<(Text, Text)>, key: &Text, budget: &Budget) -> Result<usize, Error> {
    if let Some(at) = entries.iter().position(|(name, _)| name.equals(key)) { return Ok(at); }
    let mut position = entries.len();
    if let Some(index) = js::array_index(key) {
        for (at, (name, _)) in entries.iter().enumerate() {
            if js::array_index(name).is_none_or(|old| old > index) { position = at; break; }
        }
    }
    entries.insert(position, (key.copy(budget)?, js::text("", budget)?));
    Ok(position)
}

#[derive(Debug)]
pub struct SseDecoder {
    context: StreamContext,
    buffer: Vec<u8>,
    committed: usize,
    state: Option<State>,
    structured: bool,
    done: Option<StreamValue>,
}

impl SseDecoder {
    pub fn new(context: StreamContext, budget: &Budget) -> Result<Self, Error> {
        Ok(Self { context, buffer: Vec::new(), committed: 0, state: Some(State::new(budget)?), structured: false, done: None })
    }

    /// `transform(chunk, control)`: the value TS enqueues for this chunk, or `None` when nothing is enqueued (the
    /// chunk threw).
    pub fn push(&mut self, chunk: &[u8], budget: &Budget) -> Result<Option<StreamValue>, BuildError> {
        budget.work(chunk.len())?;
        self.buffer.extend_from_slice(chunk);
        if let Some(done) = &self.done { return Ok(copy_value(Some(done), budget)?); }
        // Complete lines change the state for good.
        while let Some(at) = self.buffer[self.committed..].iter().position(|&byte| byte == 10) {
            let end = self.committed + at;
            let line = String::from_utf8_lossy(&self.buffer[self.committed..end]).into_owned();
            self.committed = end + 1;
            let mut state = self.state.take().expect("state");
            let outcome = self.line(&line, &mut state, budget);
            self.state = Some(state);
            if let Line::Done(result) = outcome? {
                let value = result.map_err(BuildError::from)?;
                let copy = copy_value(Some(&value), budget)?;
                self.done = Some(value);
                return Ok(copy);
            }
        }
        // The unfinished last line is applied to a copy, as TS re-reads it from scratch on the next chunk.
        let tail = String::from_utf8_lossy(&self.buffer[self.committed..]).into_owned();
        let result = if tail.starts_with("data: ") {
            let mut state = self.state.as_ref().expect("state").copy(budget)?;
            match self.line(&tail, &mut state, budget)? {
                Line::Done(result) => result,
                Line::Continue => self.finish_value(&state, budget),
            }
        } else {
            let state = self.state.take().expect("state");
            let result = self.finish_value(&state, budget);
            self.state = Some(state);
            result
        };
        match result {
            Ok(value) => Ok(Some(value)),
            Err(Failed::Thrown) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    fn line(&mut self, line: &str, state: &mut State, budget: &Budget) -> Result<Line, BuildError> {
        let Some(raw) = line.strip_prefix("data: ") else { return Ok(Line::Continue) };
        budget.work(raw.len())?;
        if raw == "[DONE]" {
            return match self.finish_value(state, budget) {
                Ok(value) => Ok(Line::Done(Ok(value))),
                Err(Failed::Thrown) => Ok(Line::Continue),
                Err(Failed::Decline(name)) => Ok(Line::Done(Err(Failed::Decline(name)))),
                Err(Failed::Budget(error)) => Err(error.into()),
            };
        }
        match self.apply(raw, state, budget) {
            Ok(()) | Err(Failed::Thrown) => Ok(Line::Continue),
            Err(Failed::Decline(name)) => Err(BuildError::Unsupported(name)),
            Err(Failed::Budget(error)) => Err(error.into()),
        }
    }

    fn apply(&mut self, raw: &str, state: &mut State, budget: &Budget) -> Result<(), Failed> {
        let parsed = match json::parse(raw, budget) {
            Ok(value) => js::from_value(&value, budget)?,
            Err(Error::Syntax { .. }) => return Err(Failed::Thrown),
            Err(other) => return Err(other.into()),
        };
        let choices = read(Some(&parsed), "choices")?;
        let list: &[J] = match choices {
            Some(J::Arr(items)) => items,
            Some(J::Str(text)) if text.len_units() == 0 => &[],
            _ => return Err(Failed::Thrown),
        };
        for choice in list {
            let delta = read(Some(choice), "delta")?;
            let content = read(delta, "content")?;
            let chunk = if nullish(content) { read(Some(choice), "text")? } else { content };
            if truthy(chunk) {
                let chunk = chunk.expect("truthy");
                let key = if self.context.multi_gen {
                    let index = read(Some(choice), "index")?;
                    if nullish(index) { return Err(Failed::Thrown); }
                    let key = to_text(index.expect("present"), budget)?;
                    if js::reserved_object_key(&key) { return Err(Failed::Decline("stream-inherited-key")); }
                    key
                } else { js::text("0", budget)? };
                let at = object_key(&mut state.readed, &key, budget)?;
                let next = append_fragment(&state.readed[at].1, chunk, budget)?;
                state.readed[at].1 = next;
            }
            let calls = read_optional(read_optional(Some(choice), "delta")?, "tool_calls")?;
            if truthy(calls) {
                let calls = calls.expect("truthy");
                if state.tools.is_none() { state.tools = Some(Vec::new()); }
                // A string is iterated by character: each character reads `index`, `id` and `function` as `undefined`.
                let character = J::Str(js::text("", budget)?);
                let items: &[J] = match calls {
                    J::Arr(items) => items,
                    J::Str(_) => std::slice::from_ref(&character),
                    _ => return Err(Failed::Thrown),
                };
                // The delta list works on a parsed copy that replaces the stored one only when every entry went through.
                let mut working = copy_tools(state.tools.as_ref().expect("tools"), budget)?;
                for call in items {
                    let index = read(Some(call), "index")?;
                    let key = match index { None | Some(J::Null) => js::text("0", budget)?, Some(other) => to_text(other, budget)? };
                    if js::reserved_object_key(&key) { return Err(Failed::Decline("stream-inherited-key")); }
                    let id = read(Some(call), "id")?;
                    let at = match working.iter().position(|(name, _)| name.equals(&key)) {
                        Some(at) => at,
                        None => {
                            let mut position = working.len();
                            if let Some(number) = js::array_index(&key) {
                                for (at, (name, _)) in working.iter().enumerate() {
                                    if js::array_index(name).is_none_or(|old| old > number) { position = at; break; }
                                }
                            }
                            let start_id = match id { Some(id) if js::truthy(id) => js::json_round_trip(js::clone_j(id, budget)?), _ => J::Null };
                            working.insert(position, (key.copy(budget)?, ToolAcc { id: start_id, name: J::Null, arguments: js::text("", budget)? }));
                            position
                        }
                    };
                    if let Some(id) = id.filter(|id| js::truthy(id)) { working[at].1.id = js::json_round_trip(js::clone_j(id, budget)?); }
                    let function = read_optional(Some(call), "function")?;
                    if let Some(name) = read_optional(function, "name")?.filter(|name| js::truthy(name)) { working[at].1.name = js::json_round_trip(js::clone_j(name, budget)?); }
                    if let Some(arguments) = read_optional(function, "arguments")?.filter(|arguments| js::truthy(arguments)) {
                        working[at].1.arguments = append_fragment(&working[at].1.arguments, arguments, budget)?;
                    }
                }
                state.tools = Some(working);
            }
            let message_delta = read_optional(Some(choice), "delta")?;
            let reasoning = {
                let primary = read_optional(message_delta, "reasoning_content")?;
                if nullish(primary) { read_optional(message_delta, "reasoning")? } else { primary }
            };
            if truthy(reasoning) {
                self.structured = true;
                state.reasoning = append_fragment(&state.reasoning, reasoning.expect("truthy"), budget)?;
            }
        }
        Ok(())
    }

    /// The code after the line loop of `transform`, and the `[DONE]` branch that duplicates it. It leaves `state`
    /// alone: TS rebuilds `readed` on every chunk, so the think-tag split is local to one emission.
    fn finish_value(&self, state: &State, budget: &Budget) -> Result<StreamValue, Failed> {
        let mut reasoning = state.reasoning.copy(budget)?;
        let mut zero: Option<Text> = None;
        if self.context.flags.contains(&LlmFlag::DeepSeekThinkingOutput) && !self.structured {
            let Some((_, current)) = state.readed.iter().find(|(name, _)| name.equals_ascii(b"0")) else { return Err(Failed::Thrown) };
            if let Some((split, rest)) = split_think(current, budget)? {
                zero = Some(rest);
                reasoning = split;
            }
            if reasoning.len_units() != 0 { reasoning = remove_think_open(&reasoning, budget)?; }
        }
        let shown = |key: &Text, value: &Text| -> Result<Text, Error> {
            match (&zero, key.equals_ascii(b"0")) { (Some(rest), true) => rest.copy(budget), _ => value.copy(budget) }
        };
        let has_tools = state.tools.is_some();
        let tool_calls = || -> Result<Vec<StreamToolCall>, Failed> {
            state.tools.iter().flatten().map(|(_, tool)| Ok(StreamToolCall {
                id: js::clone_j(&tool.id, budget)?,
                name: js::clone_j(&tool.name, budget)?,
                arguments: tool.arguments.copy(budget)?,
            })).collect()
        };
        if let Some(format) = &self.context.extract_json {
            let mut texts = Vec::new();
            for (key, value) in &state.readed { texts.push((key.copy(budget)?, extract_json(&shown(key, value)?, format, budget)?)); }
            // `__tool_calls` goes through `extractJSON` like every other key, and `wrapToolStream` then parses what comes
            // back: an empty result drops the calls, anything else is not JSON.
            let mut unparsable = None;
            if has_tools {
                let source = tool_calls_json(state.tools.as_deref().unwrap_or(&[]), budget)?;
                let extracted = match extract_json(&source, format, budget) {
                    Ok(extracted) => extracted,
                    // The result is the nested object itself, which `JSON.parse` reads as `[object Object]`.
                    Err(BuildError::Unsupported("extract-json-nested-object")) => js::text("[object Object]", budget)?,
                    Err(other) => return Err(other.into()),
                };
                if extracted.len_units() != 0 { unparsable = Some(extracted); }
            }
            return Ok(StreamValue { texts, tool_calls: Vec::new(), has_tool_calls: has_tools, unparsable_tool_calls: unparsable });
        }
        if reasoning.len_units() != 0 {
            let zero_text = match state.readed.iter().find(|(name, _)| name.equals_ascii(b"0")) {
                Some((key, value)) => shown(key, value)?,
                None => js::text("", budget)?,
            };
            let text = thoughts_block(&reasoning, &zero_text, budget)?;
            return Ok(StreamValue { texts: vec![(js::text("0", budget)?, text)], tool_calls: tool_calls()?, has_tool_calls: has_tools, unparsable_tool_calls: None });
        }
        let mut texts = Vec::new();
        for (key, value) in &state.readed { texts.push((key.copy(budget)?, shown(key, value)?)); }
        Ok(StreamValue { texts, tool_calls: tool_calls()?, has_tool_calls: has_tools, unparsable_tool_calls: None })
    }
}

/// `JSON.stringify(toolCallsData)`: the tool calls keyed by index, in the shape `wrapToolStream` reads back.
fn tool_calls_json(list: &[(Text, ToolAcc)], budget: &Budget) -> Result<Text, Failed> {
    let mut object = js::new_object();
    for (key, tool) in list {
        let mut function = js::new_object();
        js::set_str(&mut function, "name", js::clone_j(&tool.name, budget)?, budget)?;
        js::set_str(&mut function, "arguments", J::Str(tool.arguments.copy(budget)?), budget)?;
        let mut entry = js::new_object();
        js::set_str(&mut entry, "id", js::clone_j(&tool.id, budget)?, budget)?;
        js::set_str(&mut entry, "type", js::str_value("function", budget)?, budget)?;
        js::set_str(&mut entry, "function", function, budget)?;
        js::set(&mut object, key.copy(budget)?, entry, budget)?;
    }
    let bytes = super::wire::stringify(&object, budget)?.ok_or(Error::Invariant("tool-calls-json"))?;
    Ok(js::text(std::str::from_utf8(&bytes).map_err(|_| Error::Invariant("tool-calls-utf8"))?, budget)?)
}

fn copy_value(value: Option<&StreamValue>, budget: &Budget) -> Result<Option<StreamValue>, Error> {
    let Some(value) = value else { return Ok(None) };
    Ok(Some(StreamValue {
        texts: value.texts.iter().map(|(key, text)| Ok((key.copy(budget)?, text.copy(budget)?))).collect::<Result<_, Error>>()?,
        tool_calls: value.tool_calls.iter().map(|call| Ok(StreamToolCall {
            id: js::clone_j(&call.id, budget)?,
            name: js::clone_j(&call.name, budget)?,
            arguments: call.arguments.copy(budget)?,
        })).collect::<Result<_, Error>>()?,
        has_tool_calls: value.has_tool_calls,
        unparsable_tool_calls: value.unparsable_tool_calls.as_ref().map(|text| text.copy(budget)).transpose()?,
    }))
}

/// The tool round a finished stream asks for (`wrapToolStream`, `requests.ts:1163-1296`).
#[derive(Debug)]
pub struct StreamToolRound {
    /// The text shown so far (`value['0']`), which the follow-up prefixes.
    pub content: Text,
    /// The assistant message pushed onto `messages`.
    pub assistant: AssistantMessage,
    /// The calls TS runs: those with a name and non-empty arguments.
    pub calls: Vec<ToolCallEntry>,
}

#[derive(Debug)]
pub enum StreamEnd {
    /// `controller.close()`.
    Complete,
    ToolRound(StreamToolRound),
    /// `wrapToolStream` threw while reading the tool calls, so the stream errors with `String(error)`.
    Errored(Text),
}

/// Options `wrapToolStream` reads from the database.
#[derive(Debug, Clone, Copy)]
pub struct SessionOptions {
    /// `db.simplifiedToolUse`.
    pub simplified_tool_use: bool,
    /// `db.deepseekThinkingType === 'enabled'`.
    pub deepseek_thinking_enabled: bool,
}

/// The stream the caller reads: decoder output projected to `{"0": prefix + text}` and the tool-round bookkeeping of
/// `wrapToolStream`.
#[derive(Debug)]
pub struct StreamSession {
    decoder: SseDecoder,
    options: SessionOptions,
    prefix: Text,
    last: Option<StreamValue>,
}

fn display(value: &StreamValue, budget: &Budget) -> Result<Text, Error> {
    match value.texts.iter().find(|(key, _)| key.equals_ascii(b"0")) {
        Some((_, text)) => text.copy(budget),
        None => js::text("", budget),
    }
}

/// `extractThoughts(text)`: removes every `<Thoughts>...</Thoughts>` block and joins the reasoning with a newline.
fn extract_thoughts(content: &Text, budget: &Budget) -> Result<(Text, Text), Error> {
    let all = js::units(content);
    let open = js::utf16("<Thoughts>");
    let close = js::utf16("</Thoughts>");
    let mut kept: Vec<u16> = Vec::new();
    let mut reasoning: Vec<u16> = Vec::new();
    let mut at = 0;
    while at < all.len() {
        if all[at..].starts_with(&open) {
            let mut start = at + open.len();
            if all.get(start) == Some(&10) { start += 1; }
            let mut found = None;
            for end in start..=all.len() {
                if all[end..].starts_with(&close) { found = Some((end, end + close.len())); break; }
                if all.get(end) == Some(&10) && all[end + 1..].starts_with(&close) { found = Some((end, end + 1 + close.len())); break; }
            }
            if let Some((capture_end, mut after)) = found {
                if !reasoning.is_empty() { reasoning.push(10); }
                reasoning.extend_from_slice(&all[start..capture_end]);
                while all.get(after) == Some(&10) { after += 1; }
                at = after;
                continue;
            }
        }
        kept.push(all[at]);
        at += 1;
    }
    Ok((js::from_units(&kept, budget)?, js::from_units(&reasoning, budget)?))
}

impl StreamSession {
    pub fn new(context: StreamContext, options: SessionOptions, budget: &Budget) -> Result<Self, Error> {
        Ok(Self { decoder: SseDecoder::new(context, budget)?, options, prefix: js::text("", budget)?, last: None })
    }

    /// One transport chunk. The result is the `{"0": text}` TS enqueues for the caller, if any.
    pub fn push(&mut self, chunk: &[u8], budget: &Budget) -> Result<Option<Text>, BuildError> {
        let Some(value) = self.decoder.push(chunk, budget)? else { return Ok(None) };
        let content = display(&value, budget)?;
        let shown = if self.prefix.len_units() != 0 { js::concat(&js::concat(&self.prefix, &js::text("\n\n", budget)?, budget)?, &content, budget)? } else { content };
        self.last = Some(value);
        Ok(Some(shown))
    }

    /// The reader reached its end: either the stream closes or a tool round follows.
    pub fn finish(&mut self, budget: &Budget) -> Result<StreamEnd, BuildError> {
        let Some(value) = self.last.as_ref() else { return Ok(StreamEnd::Complete) };
        if let Some(unparsable) = &value.unparsable_tool_calls {
            return Ok(StreamEnd::Errored(super::owner::json_parse_error(unparsable, budget)?));
        }
        let content = display(value, budget)?;
        if value.tool_calls.is_empty() { return Ok(StreamEnd::Complete); }
        let flags = &self.decoder.context.flags;
        let pass_reasoning = flags.contains(&LlmFlag::DeepSeekThinkingInput)
            || (flags.contains(&LlmFlag::DeepSeekThinkingToggle) && self.options.deepseek_thinking_enabled);
        let (assistant_content, reasoning) = if pass_reasoning {
            let (kept, reasoning) = extract_thoughts(&content, budget)?;
            (kept, Some(reasoning))
        } else { (content.copy(budget)?, None) };
        let mut message = js::new_object();
        js::set_str(&mut message, "role", js::str_value("assistant", budget)?, budget)?;
        let shown = if self.options.simplified_tool_use { js::text("", budget)? } else { assistant_content };
        js::set_str(&mut message, "content", J::Str(shown), budget)?;
        let mut calls = Vec::new();
        for call in &value.tool_calls {
            let mut function = js::new_object();
            js::set_str(&mut function, "name", js::clone_j(&call.name, budget)?, budget)?;
            js::set_str(&mut function, "arguments", J::Str(call.arguments.copy(budget)?), budget)?;
            let mut entry = js::new_object();
            js::set_str(&mut entry, "id", js::clone_j(&call.id, budget)?, budget)?;
            js::set_str(&mut entry, "type", js::str_value("function", budget)?, budget)?;
            js::set_str(&mut entry, "function", function, budget)?;
            calls.push(entry);
        }
        js::set_str(&mut message, "tool_calls", J::Arr(calls), budget)?;
        if let Some(reasoning) = reasoning.filter(|reasoning| reasoning.len_units() != 0) {
            js::set_str(&mut message, "reasoning_content", J::Str(reasoning), budget)?;
        }
        let mut entries = Vec::new();
        for call in &value.tool_calls {
            if !js::truthy(&call.name) || call.arguments.len_units() == 0 { continue; }
            entries.push(ToolCallEntry { id: js::clone_j(&call.id, budget)?, name: js::clone_j(&call.name, budget)?, arguments: call.arguments.copy(budget)? });
        }
        Ok(StreamEnd::ToolRound(StreamToolRound { content, assistant: AssistantMessage(message), calls: entries }))
    }

    /// A fresh decoder for the follow-up response and the `{"0": prefix}` value TS enqueues before reading it.
    /// `call_codes` are the encoded tool calls of `rememberToolUsage`, in call order.
    pub fn continue_after_tools(&mut self, round: &StreamToolRound, call_codes: &[Text], budget: &Budget) -> Result<Text, Error> {
        let context = self.decoder.context.copy(budget)?;
        self.decoder = SseDecoder::new(context, budget)?;
        let mut addition = js::text("", budget)?;
        if round.content.len_units() != 0 && !self.options.simplified_tool_use {
            addition.append(&round.content)?;
            addition.append(&js::text("\n\n", budget)?)?;
        }
        let codes: Vec<&Text> = call_codes.iter().collect();
        addition.append(&js::join(&codes, "\n\n", budget)?)?;
        self.prefix = js::concat(&self.prefix, &addition, budget)?;
        self.prefix.copy(budget)
    }
}

/// What the first response of a streaming request decides (`requests.ts:1010-1029`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamStart {
    /// A 200 response whose content type contains `text/event-stream`.
    Accept,
    /// Any other response: the request fails with the body text.
    Reject,
}

/// `da.status !== 200` and `da.headers.get('Content-Type').includes('text/event-stream')`. A response without a
/// content type throws in TS.
pub fn stream_start(status: u16, content_type: Option<&Text>, budget: &Budget) -> Result<StreamStart, BuildError> {
    if status != 200 { return Ok(StreamStart::Reject); }
    let Some(content_type) = content_type else {
        return Err(BuildError::Thrown(js::text("Cannot read properties of null (reading 'includes')", budget)?));
    };
    let wanted = js::utf16("text/event-stream");
    Ok(if js::find_units(&js::units(content_type), &wanted, 0).is_some() { StreamStart::Accept } else { StreamStart::Reject })
}
