//! SupaMemory (`src/ts/process/memory/supaMemory.ts`), and legacy Hypa as its `asHyper` mode, over a captured stored
//! record and injected tokenizer, retrieval and summary services. Errors return without a stored change, as the
//! caller throws them before persisting.
use crate::hanurai::{Retrieval, RetrievalOutput};
use crate::hypa_v2::key::{Key, KeyIndex};
use crate::hypa_v2::{SummaryOutput, SummaryService};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::{Effect, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{json, Budget, Error, OwnedVec, Text, TextBuilder, Value};
use std::borrow::Cow;
use std::mem::size_of;

/// Settings captured once before an invocation.
pub struct SupaMemoryConfig {
    /// `db.hypaMemory`: the legacy Hypa mode (`arg.asHyper`).
    pub as_hyper: bool,
    /// `removePunctuationHypa`.
    pub remove_punctuation: bool,
    /// `maxSupaChunkSize`.
    pub max_chunk_size: f64,
    /// `char.name`, which names assistant messages in a batch and ends the legacy Hypa query.
    pub char_name: Text,
    /// `getUserName()`, which names every other message in a batch.
    pub user_name: Text,
}

const PUNCTUATION: &[u8] = b".,/#!$%^&*;:{}=-_`~()";

/// One invocation. A completed or failed instance cannot run again: dependencies may already have done work.
pub struct SupaMemory<T, R, S> {
    config: SupaMemoryConfig,
    stored: Option<Value>,
    tokenizer: T,
    retrieval: R,
    summary: S,
    started: bool,
    suspended: Option<(Box<State>, Dependency)>,
}

#[derive(Clone, Copy)]
enum Dependency { Tokenizer, Retrieval, Summary }
enum Reply { Count(u32), Retrieval(RetrievalOutput), Summary(SummaryOutput) }

enum Phase {
    /// Drops the messages before the first `NewChat` (`:25-38`).
    Trim { end: usize },
    Stored,
    /// Drops messages until the stored ID (`:107-123`).
    Seek { id: Text },
    StoredCount,
    /// Counts from the trimmed messages to each record entry's message in turn; only the selected entry's count is kept
    /// (`:73-96`).
    Probe { entry: usize, position: usize, tokens: f64 },
    Retrieve,
    Prepared,
    Queried,
    PastCount,
    Fit,
    Outer,
    Walk,
    WalkCount,
    RanOutSummary,
    RanOutOld,
    RanOutNew,
    AfterWalk,
    ChunkSummary,
    ChunkCount,
    Merge,
    MergeOld,
    MergeNew,
    Append,
}

/// Legacy Hypa state.
struct Hyper {
    /// `HypaData`: the stored record's entries, empty without a `hypa:` record.
    record: OwnedVec<Value>,
    /// A `hypa:` record was read; `not_array` when it is not an array.
    parsed: bool,
    not_array: bool,
    /// The record entry the probe selects and the trimmed-message position of its message.
    selected: Option<(usize, usize)>,
    /// `hypaChunks`.
    chunks: OwnedVec<Text>,
    /// `hypaResult`.
    result: Text,
}

struct State {
    chats: Vec<PromptMessage>,
    /// Messages before `front` have been spliced away.
    front: usize,
    tokens: f64,
    max_context: f64,
    supa: Text,
    /// `lastId`: a memo string, or `undefined` from a memo-less message or record entry.
    last_id: Value,
    before: f64,
    max_chunk: f64,
    summarized: bool,
    chunk: f64,
    batch: Text,
    splice: usize,
    result: Option<Text>,
    result_tokens: f64,
    hyper: Option<Hyper>,
    phase: Phase,
}

enum Call { Count(usize), Text(Text), Message(PromptMessage), Prepare(Vec<Text>), Query(Text), Summarize(Text) }
enum Finish { Error(Text), Fit, Complete }
enum Next { Call(Call), Finish(Finish) }

fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }
fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }
fn decline(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
fn limit() -> StageError { StageError::Limit(Cow::Borrowed("owned-bytes")) }

fn lift<T>(step: Step<T>, reply: fn(T) -> Reply) -> Step<Reply> {
    match step { Step::NeedsHost(request) => Step::NeedsHost(request), Step::Done(value) => Step::Done(reply(value)) }
}

fn units(value: &Text, budget: &Budget) -> Result<OwnedVec<u16>, StageError> {
    budget.work(value.len_units())?;
    let mut units = OwnedVec::new(budget)?;
    for (index, unit) in value.units().enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        units.push(unit)?;
    }
    Ok(units)
}

fn concat(parts: &[&Text], budget: &Budget) -> Result<Text, StageError> {
    let mut result = text("", budget)?;
    for part in parts { result.append(part)?; }
    Ok(result)
}

/// `value.replace(/[\.,\/#!$%\^&\*;:{}=\-_`~()]/g, "")`.
fn strip_punctuation(value: &Text, budget: &Budget) -> Result<Text, StageError> {
    let mut result = TextBuilder::new(budget)?;
    for unit in units(value, budget)?.as_slice() {
        if !PUNCTUATION.iter().any(|&mark| u16::from(mark) == *unit) { result.unit(*unit)?; }
    }
    Ok(result.finish()?)
}

/// `value.replace(/\n+/g, "\n")`.
fn collapse_newlines(value: &Text, budget: &Budget) -> Result<Text, StageError> {
    let mut result = TextBuilder::new(budget)?;
    let mut newline = false;
    for &unit in units(value, budget)?.as_slice() {
        if unit == 10 && newline { continue; }
        newline = unit == 10;
        result.unit(unit)?;
    }
    Ok(result.finish()?)
}

/// `value.split("\n\n").filter((entry) => entry.length > 1)`.
fn entries(value: &Text, budget: &Budget) -> Result<OwnedVec<Text>, StageError> {
    let all = units(value, budget)?;
    let all = all.as_slice();
    let mut result = OwnedVec::new(budget)?;
    let push = |start: usize, end: usize, result: &mut OwnedVec<Text>| -> Result<(), StageError> {
        if end - start > 1 { result.push(Text::from_units(&all[start..end], budget)?)?; }
        Ok(())
    };
    let (mut start, mut at) = (0, 0);
    while at + 1 < all.len() {
        budget.work(1)?;
        if all[at] == 10 && all[at + 1] == 10 { push(start, at, &mut result)?; at += 2; start = at; } else { at += 1; }
    }
    push(start, all.len(), &mut result)?;
    Ok(result)
}

fn join(values: &[Text], separator: &str, budget: &Budget) -> Result<Text, StageError> {
    let separator = text(separator, budget)?;
    let mut joined = text("", budget)?;
    for (index, value) in values.iter().enumerate() {
        budget.work(1)?;
        if index != 0 { joined.append(&separator)?; }
        joined.append(value)?;
    }
    Ok(joined)
}

fn memo_value(message: &PromptMessage, budget: &Budget) -> Result<Value, StageError> {
    Ok(match &message.memo { Some(memo) => Value::Text(memo.copy(budget)?), None => { budget.work(1)?; Value::Undefined } })
}

fn message(role: Role, content: Text, memo: &str, budget: &Budget) -> Result<PromptMessage, StageError> {
    let mut message = PromptMessage::new(role, content);
    message.memo = Some(text(memo, budget)?);
    Ok(message)
}

fn property<'a>(entry: &'a Value, key: &str, budget: &Budget) -> Result<&'a Value, StageError> {
    Ok(entry.get(key.as_bytes(), budget)?.unwrap_or(&Value::Undefined))
}

/// `JSON.stringify(value, null, 2)` over a parsed record: the layout is built here and every leaf and key goes through
/// [`json::stringify`]. `None` where JS returns `undefined`.
fn pretty(value: &Value, depth: usize, budget: &Budget) -> Result<Option<Text>, StageError> {
    budget.work(1)?;
    let indent = |depth: usize| text(&format!("\n{}", "  ".repeat(depth)), budget);
    match value {
        Value::Array(items) => {
            if items.as_slice().is_empty() { return Ok(Some(text("[]", budget)?)); }
            let mut out = text("[", budget)?;
            for (index, item) in items.as_slice().iter().enumerate() {
                if index != 0 { out.append(&text(",", budget)?)?; }
                out.append(&indent(depth + 1)?)?;
                out.append(&match pretty(item, depth + 1, budget)? { Some(item) => item, None => text("null", budget)? })?;
            }
            out.append(&indent(depth)?)?;
            out.append(&text("]", budget)?)?;
            Ok(Some(out))
        }
        Value::Object(properties) => {
            let mut out = text("{", budget)?;
            let mut any = false;
            for (key, item) in properties.as_slice() {
                let Some(item) = pretty(item, depth + 1, budget)? else { continue };
                if any { out.append(&text(",", budget)?)?; }
                any = true;
                out.append(&indent(depth + 1)?)?;
                out.append(&json::stringify(&Value::Text(key.copy(budget)?), budget)?.ok_or_else(|| invalid("supa-memory-key"))?)?;
                out.append(&text(": ", budget)?)?;
                out.append(&item)?;
            }
            if !any { return Ok(Some(text("{}", budget)?)); }
            out.append(&indent(depth)?)?;
            out.append(&text("}", budget)?)?;
            Ok(Some(out))
        }
        leaf => Ok(json::stringify(leaf, budget)?),
    }
}

impl<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send> SupaMemory<T, R, S> {
    /// `stored` is the room's `supaMemoryData` as captured, `None` when absent.
    pub fn new(config: SupaMemoryConfig, stored: Option<Value>, tokenizer: T, retrieval: R, summary: S) -> Self {
        Self { config, stored, tokenizer, retrieval, summary, started: false, suspended: None }
    }

    fn admit(&mut self, input: MemoryInput, budget: &Budget) -> Result<Result<Box<State>, MemoryOutput>, StageError> {
        budget.check()?;
        let tokens = input.current_tokens + 10.0;
        if !(tokens > input.max_context_tokens) {
            return Ok(Err(MemoryOutput { chats: input.chats, current_tokens: tokens, effects: Vec::new(), error: None }));
        }
        // A record of another type is read through string methods TS does not have for it.
        if self.stored.as_ref().is_some_and(|stored| stored.truthy() && !matches!(stored, Value::Text(_))) { return Err(decline("supaMemoryData")); }
        let end = input.chats.iter().position(|chat| chat.memo.as_ref().is_some_and(|memo| memo.equals_ascii(b"NewChat"))).unwrap_or(0);
        let hyper = if self.config.as_hyper { Some(self.hyper(&input.chats[end..], budget)?) } else { None };
        Ok(Ok(Box::new(State {
            chats: input.chats, front: 0, tokens, max_context: input.max_context_tokens, supa: text("", budget)?,
            last_id: Value::Text(text("", budget)?), before: 0.0, max_chunk: 0.0, summarized: false, chunk: 0.0, batch: text("", budget)?,
            splice: 0, result: None, result_tokens: 0.0, hyper, phase: Phase::Trim { end },
        })))
    }

    /// Reads a `hypa:` record (`:45-71`) and selects its entry from the trimmed messages, declining every shape TS throws
    /// on before the first dependency call. A plain record is ignored (`:50-53`).
    fn hyper(&mut self, trimmed: &[PromptMessage], budget: &Budget) -> Result<Hyper, StageError> {
        let mut hyper = Hyper { record: OwnedVec::new(budget)?, parsed: false, not_array: false, selected: None,
            chunks: OwnedVec::new(budget)?, result: text("", budget)? };
        let Some(Value::Text(stored)) = self.stored.take() else { return Ok(hyper) };
        if stored.len_units() <= 4 { return Ok(hyper); }
        let all = units(&stored, budget)?;
        let all = all.as_slice();
        let split = all.iter().position(|&unit| unit == 10).unwrap_or(all.len());
        if split < 5 || !Text::from_units(&all[..5], budget)?.equals_ascii(b"hypa:") { return Ok(hyper); }
        let data = Text::from_units(all.get(split + 1..).unwrap_or(&[]), budget)?;
        // `JSON.parse(data.trim())` throws a `SyntaxError` after the NewChat messages were counted.
        let entries = match json::parse_json_text(&data.trim(budget)?, budget) {
            Ok(Value::Array(entries)) => entries,
            Ok(_) => { hyper.parsed = true; hyper.not_array = true; return Ok(hyper); }
            Err(Error::Syntax { .. }) => return Err(decline("supaMemoryData.json")),
            Err(error) => return Err(error.into()),
        };
        hyper.parsed = true;
        let mut memos = KeyIndex::new();
        for (position, chat) in trimmed.iter().enumerate() { memos.first(Key::memo(chat.memo.as_ref(), budget)?, position, budget)?; }
        for (index, entry) in entries.as_slice().iter().enumerate() {
            // `HypaData[j].id` throws on `null`; other non-objects read `undefined` and fail later.
            if !matches!(entry, Value::Object(_)) { return Err(decline("supaMemoryData.entry")); }
            let Some(key) = Key::of(property(entry, "id", budget)?, budget)? else { continue };
            if let Some(position) = memos.get(&key, budget)? { hyper.selected = Some((index, position)); break; }
        }
        if let Some((index, _)) = hyper.selected {
            let entry = &entries.as_slice()[index];
            if !matches!(property(entry, "supa", budget)?, Value::Text(_)) { return Err(decline("supaMemoryData.supa")); }
            match property(entry, "hypa", budget)? {
                Value::Array(chunks) if chunks.as_slice().iter().all(|chunk| matches!(chunk, Value::Text(_))) => {}
                _ => return Err(decline("supaMemoryData.hypa")),
            }
        }
        hyper.record = entries;
        Ok(hyper)
    }

    fn error(message: &str, budget: &Budget) -> Result<Next, StageError> { Ok(Next::Finish(Finish::Error(text(message, budget)?))) }

    /// The `addText` input (`:147-156`): unique chunks, without punctuation when configured, that the memory does not
    /// already contain.
    fn documents(&self, state: &State, budget: &Budget) -> Result<Vec<Text>, StageError> {
        let hyper = state.hyper.as_ref().ok_or_else(|| invalid("supa-memory-phase"))?;
        let memory = strip_punctuation(&state.supa, budget)?;
        let mut seen = KeyIndex::new();
        let mut documents = Vec::new();
        for (index, chunk) in hyper.chunks.as_slice().iter().enumerate() {
            if seen.first(Key::text(chunk, budget)?, index, budget)?.is_some() { continue; }
            let chunk = if self.config.remove_punctuation { strip_punctuation(chunk, budget)? } else { chunk.copy(budget)? };
            if memory.index_of(&strip_punctuation(&chunk, budget)?, budget)?.is_some() { continue; }
            documents.try_reserve(1).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
            documents.push(chunk);
        }
        Ok(documents)
    }

    /// `stringlizeChat(filteredChat.slice(0, 4), char.name, false)` (`src/ts/process/stringlize.ts:8-30`).
    fn query(&self, state: &State, budget: &Budget) -> Result<Text, StageError> {
        let mut parts = OwnedVec::new(budget)?;
        let chats = state.chats[state.front..].iter().filter(|chat| !matches!(chat.role, Role::System | Role::Function)).take(4);
        for chat in chats {
            budget.work(1)?;
            if chat.memo.as_ref().is_some_and(|memo| memo.len_units() >= 10 && memo.units().take(10).eq(b"inlayImage".iter().map(|&b| u16::from(b)))) { continue; }
            parts.push(match chat.name.as_ref().filter(|name| name.len_units() != 0) {
                Some(name) => concat(&[name, &text(": ", budget)?, &chat.content], budget)?,
                None => chat.content.copy(budget)?,
            })?;
        }
        let mut query = join(parts.as_slice(), "\n\n", budget)?;
        query.append(&concat(&[&text("\n\n", budget)?, &self.config.char_name, &text(":", budget)?], budget)?)?;
        Ok(query)
    }

    fn next(&mut self, state: &mut State, budget: &Budget) -> Result<Next, StageError> {
        loop {
            budget.work(1)?;
            match &mut state.phase {
                Phase::Trim { end } => {
                    if state.front < *end { return Ok(Next::Call(Call::Count(state.front))); }
                    state.phase = Phase::Stored;
                }
                Phase::Stored => {
                    if let Some(hyper) = &state.hyper {
                        if hyper.not_array { return Self::error("hypaMemory: hypaMemory isn't Array", budget); }
                        state.phase = if hyper.parsed { Phase::Probe { entry: 0, position: 0, tokens: state.tokens } } else { Phase::Retrieve };
                        continue;
                    }
                    state.phase = Phase::Fit;
                    let Some(Value::Text(stored)) = self.stored.take() else { continue };
                    if stored.len_units() <= 4 { continue; }
                    let all = units(&stored, budget)?;
                    let all = all.as_slice();
                    let split = all.iter().position(|&unit| unit == 10);
                    let id = Text::from_units(&all[..split.unwrap_or(all.len())], budget)?;
                    let data = Text::from_units(split.map_or(&[][..], |at| &all[at + 1..]), budget)?;
                    if id.len_units() >= 5 && id.prefix(5, budget)?.equals_ascii(b"hypa:") {
                        return Self::error("SupaMemory: Data saved in hypaMemory, loaded as SupaMemory.", budget);
                    }
                    state.supa = if self.config.remove_punctuation { strip_punctuation(&data, budget)? } else { data };
                    state.phase = Phase::Seek { id };
                }
                Phase::Seek { id } => {
                    let Some(chat) = state.chats.get(state.front) else { return Self::error("SupaMemory: chat ID not found", budget) };
                    budget.work(id.len_units())?;
                    if chat.memo.as_ref().is_some_and(|memo| memo.equals(id)) {
                        state.last_id = Value::Text(id.copy(budget)?);
                        state.phase = Phase::StoredCount;
                        return Ok(Next::Call(Call::Text(state.supa.copy(budget)?)));
                    }
                    return Ok(Next::Call(Call::Count(state.front)));
                }
                Phase::Probe { entry, position, tokens } => {
                    let hyper = state.hyper.as_mut().ok_or_else(|| invalid("supa-memory-phase"))?;
                    if let Some((selected, at)) = hyper.selected.filter(|(selected, at)| selected == entry && at == position) {
                        // The selected entry's memory and chunks; only chunks longer than one unit are kept (`:105-145`).
                        let record = &hyper.record.as_slice()[selected];
                        state.tokens = *tokens;
                        state.front += at;
                        state.last_id = match property(record, "id", budget)? { Value::Text(id) => Value::Text(id.copy(budget)?), _ => Value::Undefined };
                        let Value::Text(supa) = property(record, "supa", budget)? else { return Err(invalid("supa-memory-record")) };
                        state.supa = supa.copy(budget)?;
                        let Value::Array(chunks) = property(record, "hypa", budget)? else { return Err(invalid("supa-memory-record")) };
                        for chunk in chunks.as_slice() {
                            let Value::Text(chunk) = chunk else { return Err(invalid("supa-memory-record")) };
                            budget.work(1)?;
                            if chunk.len_units() > 1 { hyper.chunks.push(chunk.copy(budget)?)?; }
                        }
                        state.phase = Phase::Retrieve;
                        continue;
                    }
                    if *entry >= hyper.record.as_slice().len() { return Self::error("hypaMemory: chat ID not found", budget); }
                    if state.front + *position < state.chats.len() { return Ok(Next::Call(Call::Count(state.front + *position))); }
                    state.phase = Phase::Probe { entry: *entry + 1, position: 0, tokens: state.tokens };
                }
                Phase::Retrieve => {
                    state.phase = Phase::Fit;
                    if state.hyper.as_ref().is_none_or(|hyper| hyper.chunks.as_slice().is_empty()) { continue; }
                    let documents = self.documents(state, budget)?;
                    state.phase = Phase::Prepared;
                    return Ok(Next::Call(Call::Prepare(documents)));
                }
                Phase::Fit => {
                    if state.tokens < state.max_context { return Ok(Next::Finish(Finish::Fit)); }
                    state.phase = Phase::Outer;
                }
                Phase::Outer => {
                    if !(state.tokens > state.max_context) { return Ok(Next::Finish(Finish::Complete)); }
                    state.before = state.tokens;
                    state.max_chunk = (state.max_context / 3.0).floor();
                    if self.config.max_chunk_size < state.max_chunk { state.max_chunk = self.config.max_chunk_size; }
                    state.summarized = false;
                    state.chunk = 0.0;
                    state.batch = text("", budget)?;
                    state.splice = 0;
                    state.phase = Phase::Walk;
                }
                Phase::Walk => {
                    let position = state.front + state.splice;
                    if position < state.chats.len() {
                        state.phase = Phase::WalkCount;
                        return Ok(Next::Call(Call::Count(position)));
                    }
                    // Out of messages: start the batch over (`:291-321`).
                    state.tokens = state.before;
                    state.batch = text("", budget)?;
                    state.chunk = 0.0;
                    state.splice = 0;
                    if state.summarized {
                        if state.max_chunk < 500.0 { return Self::error("Not Enough Tokens to summarize in SupaMemory", budget); }
                        state.max_chunk *= 0.7;
                        continue;
                    }
                    state.phase = Phase::RanOutSummary;
                    return Ok(Next::Call(Call::Summarize(state.supa.copy(budget)?)));
                }
                Phase::AfterWalk => {
                    state.front += state.splice;
                    if state.batch.len_units() == 0 {
                        // An oversize first system or function message adds nothing, and TS retries it forever.
                        if state.tokens > state.max_context { return Err(invalid("supa-memory-no-progress")); }
                        state.phase = Phase::Outer;
                        continue;
                    }
                    let batch = std::mem::replace(&mut state.batch, text("", budget)?);
                    state.phase = Phase::ChunkSummary;
                    return Ok(Next::Call(Call::Summarize(batch)));
                }
                Phase::Append => {
                    let result = state.result.take().ok_or_else(|| invalid("supa-memory-result"))?;
                    let mut list = entries(&state.supa, budget)?;
                    list.push(collapse_newlines(&result, budget)?)?;
                    state.tokens += state.result_tokens;
                    state.supa = join(list.as_slice(), "\n\n", budget)?;
                    state.phase = Phase::Outer;
                }
                Phase::StoredCount | Phase::Prepared | Phase::Queried | Phase::PastCount | Phase::WalkCount | Phase::RanOutSummary
                | Phase::RanOutOld | Phase::RanOutNew | Phase::ChunkSummary | Phase::ChunkCount | Phase::Merge | Phase::MergeOld
                | Phase::MergeNew => return Err(invalid("supa-memory-phase")),
            }
        }
    }

    fn accept(&self, state: &mut State, reply: Reply, budget: &Budget) -> Result<Option<Next>, StageError> {
        budget.check()?;
        let config = &self.config;
        let summary = |output: SummaryOutput| -> Result<Result<Text, Next>, StageError> {
            match output {
                SummaryOutput::Success(result) => Ok(Ok(result)),
                SummaryOutput::Failed(error) => Ok(Err(Next::Finish(Finish::Error(error)))),
            }
        };
        match (std::mem::replace(&mut state.phase, Phase::Outer), reply) {
            (Phase::Trim { end }, Reply::Count(count)) => {
                state.tokens -= f64::from(count);
                state.front += 1;
                state.phase = Phase::Trim { end };
            }
            (Phase::Seek { id }, Reply::Count(count)) => {
                state.tokens -= f64::from(count);
                state.front += 1;
                state.phase = Phase::Seek { id };
            }
            (Phase::StoredCount, Reply::Count(count)) => { state.tokens += f64::from(count); state.phase = Phase::Fit; }
            (Phase::Probe { entry, position, tokens }, Reply::Count(count)) => {
                state.phase = Phase::Probe { entry, position: position + 1, tokens: tokens - f64::from(count) };
            }
            (Phase::Prepared, Reply::Retrieval(RetrievalOutput::Prepared)) => {
                state.phase = Phase::Queried;
                return Ok(Some(Next::Call(Call::Query(self.query(state, budget)?))));
            }
            (Phase::Queried, Reply::Retrieval(RetrievalOutput::Scored(found))) => {
                state.phase = Phase::Fit;
                if found.is_empty() { return Ok(None); }
                // `"past events: " + s.slice(0, 3)` joins the first three documents with commas (`:161-167`).
                let mut documents = OwnedVec::new(budget)?;
                for document in found.iter().take(3) {
                    documents.push(match &document.document { Some(document) => document.copy(budget)?, None => text("", budget)? })?;
                }
                let past = concat(&[&text("past events: ", budget)?, &join(documents.as_slice(), ",", budget)?], budget)?;
                let hyper = state.hyper.as_mut().ok_or_else(|| invalid("supa-memory-phase"))?;
                hyper.result = past.copy(budget)?;
                state.phase = Phase::PastCount;
                return Ok(Some(Next::Call(Call::Message(message(Role::Assistant, past, "hypaMemory", budget)?))));
            }
            (Phase::PastCount, Reply::Count(count)) => {
                state.tokens += f64::from(count);
                state.tokens += 10.0;
                state.phase = Phase::Fit;
            }
            (Phase::WalkCount, Reply::Count(count)) => {
                let count = f64::from(count);
                let chat = &state.chats[state.front + state.splice];
                let line = |batch: &mut Text| -> Result<(), StageError> {
                    batch.append(if matches!(chat.role, Role::Assistant) { &config.char_name } else { &config.user_name })?;
                    batch.append(&concat(&[&text(": ", budget)?, &chat.content, &text("\n\n", budget)?], budget)?)?;
                    Ok(())
                };
                if state.chunk + count > state.max_chunk {
                    // A first message over the chunk size is summarized alone (`:323-333`).
                    if state.batch.len_units() == 0 && !matches!(chat.role, Role::Function | Role::System) {
                        line(&mut state.batch)?;
                        state.splice += 1;
                        state.tokens -= count;
                        state.chunk += count;
                    }
                    state.last_id = memo_value(chat, budget)?;
                    state.phase = Phase::AfterWalk;
                } else {
                    line(&mut state.batch)?;
                    state.splice += 1;
                    state.tokens -= count;
                    state.chunk += count;
                    state.phase = Phase::Walk;
                }
            }
            (Phase::RanOutSummary, Reply::Summary(output)) => match summary(output)? {
                Ok(result) => {
                    state.result = Some(result);
                    state.phase = Phase::RanOutOld;
                    return Ok(Some(Next::Call(Call::Text(state.supa.copy(budget)?))));
                }
                Err(finish) => return Ok(Some(finish)),
            },
            (Phase::RanOutOld, Reply::Count(count)) => {
                state.tokens -= f64::from(count);
                let result = state.result.take().ok_or_else(|| invalid("supa-memory-result"))?;
                state.supa = concat(&[&result, &text("\n\n", budget)?], budget)?;
                state.phase = Phase::RanOutNew;
                return Ok(Some(Next::Call(Call::Text(state.supa.copy(budget)?))));
            }
            (Phase::RanOutNew, Reply::Count(count)) => {
                state.tokens += f64::from(count);
                state.summarized = true;
                state.phase = if state.tokens <= state.max_context { Phase::AfterWalk } else { Phase::Walk };
            }
            (Phase::ChunkSummary, Reply::Summary(output)) => match summary(output)? {
                Ok(result) => {
                    let counted = concat(&[&result, &text("\n\n", budget)?], budget)?;
                    state.result = Some(result);
                    state.phase = Phase::ChunkCount;
                    return Ok(Some(Next::Call(Call::Text(counted))));
                }
                Err(finish) => return Ok(Some(finish)),
            },
            (Phase::ChunkCount, Reply::Count(count)) => {
                state.result_tokens = f64::from(count);
                if let Some(hyper) = state.hyper.as_mut() {
                    hyper.chunks.push(collapse_newlines(state.result.as_ref().ok_or_else(|| invalid("supa-memory-result"))?, budget)?)?;
                }
                let threshold = if config.as_hyper { 3 } else { 4 };
                if entries(&state.supa, budget)?.as_slice().len() >= threshold {
                    state.phase = Phase::Merge;
                    return Ok(Some(Next::Call(Call::Summarize(state.supa.copy(budget)?))));
                }
                state.phase = Phase::Append;
            }
            (Phase::Merge, Reply::Summary(output)) => match summary(output)? {
                Ok(merged) => {
                    let old = std::mem::replace(&mut state.supa, merged);
                    state.phase = Phase::MergeOld;
                    return Ok(Some(Next::Call(Call::Text(old))));
                }
                Err(finish) => return Ok(Some(finish)),
            },
            (Phase::MergeOld, Reply::Count(count)) => {
                state.tokens -= f64::from(count);
                state.phase = Phase::MergeNew;
                return Ok(Some(Next::Call(Call::Text(state.supa.copy(budget)?))));
            }
            (Phase::MergeNew, Reply::Count(count)) => { state.tokens += f64::from(count); state.phase = Phase::Append; }
            _ => return Err(invalid("supa-memory-dependency-output")),
        }
        Ok(None)
    }

    /// The updated `hypa:` record (`:386-411`): the first entry is updated in place when its ID is the last ID, otherwise
    /// a new entry goes first.
    fn record(hyper: Hyper, last_id: &Value, supa: &Text, budget: &Budget) -> Result<Text, StageError> {
        let Hyper { mut record, chunks, .. } = hyper;
        let mut list = OwnedVec::new(budget)?;
        for chunk in chunks.as_slice() { list.push(Value::Text(chunk.copy(budget)?))?; }
        let same = match (record.as_slice().first().map(|entry| property(entry, "id", budget)).transpose()?, last_id) {
            (Some(Value::Undefined), Value::Undefined) => true,
            (Some(Value::Text(id)), Value::Text(last)) => { budget.work(id.len_units())?; id.equals(last) }
            _ => false,
        };
        if same {
            let entry = &mut record.as_mut_slice()[0];
            entry.define(text("hypa", budget)?, Value::Array(list), budget)?;
            entry.define(text("supa", budget)?, Value::Text(supa.copy(budget)?), budget)?;
        } else {
            let mut entry = Value::Object(OwnedVec::new(budget)?);
            let id = match last_id { Value::Text(id) => Value::Text(id.copy(budget)?), _ => Value::Undefined };
            entry.define(text("id", budget)?, id, budget)?;
            entry.define(text("hypa", budget)?, Value::Array(list), budget)?;
            entry.define(text("supa", budget)?, Value::Text(supa.copy(budget)?), budget)?;
            let mut unshifted = OwnedVec::new(budget)?;
            unshifted.push(entry)?;
            for value in record.as_mut_slice() { budget.work(1)?; unshifted.push(std::mem::replace(value, Value::Undefined))?; }
            record = unshifted;
        }
        let printed = pretty(&Value::Array(record), 0, budget)?.ok_or_else(|| invalid("supa-memory-record"))?;
        concat(&[&text("hypa:\n", budget)?, &printed], budget)
    }

    fn finish(&self, state: Box<State>, finish: Finish, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        budget.check()?;
        let State { chats, front, tokens, supa, last_id, hyper, .. } = *state;
        let length = chats.len();
        let capacity = length.saturating_sub(front).checked_add(2).ok_or_else(limit)?;
        // Outer containers are charged while this policy owns them; the returned output is outside the budget.
        let _charge = budget.reserve_owned(capacity.checked_mul(size_of::<PromptMessage>())
            .and_then(|n| n.checked_add(2 * size_of::<Effect>())).ok_or_else(limit)?)?;
        let mut output = Vec::new();
        output.try_reserve_exact(capacity).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
        let past = match &hyper { Some(hyper) => hyper.result.copy(budget)?, None => text("", budget)? };
        let (error, effects) = match finish {
            Finish::Error(error) => (Some(error), Vec::new()),
            Finish::Fit => {
                // `supaMemory + '\n\n' + hypaResult`, returned without a stored record (`:172-182`).
                output.push(message(Role::System, concat(&[&supa, &text("\n\n", budget)?, &past], budget)?, "supaMemory", budget)?);
                (None, Vec::new())
            }
            Finish::Complete => {
                let memory = match hyper {
                    Some(hyper) => Self::record(hyper, &last_id, &supa, budget)?,
                    None => {
                        let id = match &last_id { Value::Text(id) => id.copy(budget)?, _ => text("undefined", budget)? };
                        concat(&[&id, &text("\n", budget)?, &supa], budget)?
                    }
                };
                if past.len_units() != 0 { output.push(message(Role::System, past, "hypaMemory", budget)?); }
                output.push(message(Role::System, supa, "supaMemory", budget)?);
                let mut effects = Vec::new();
                effects.try_reserve_exact(2).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
                effects.push(Effect::ChatField { field: text("supaMemoryData", budget)?, value: Value::Text(memory) });
                // `sp.lastId ?? currentChat.lastMemory`: an undefined ID keeps the stored one.
                if let Value::Text(id) = last_id { effects.push(Effect::ChatField { field: text("lastMemory", budget)?, value: Value::Text(id) }); }
                (None, effects)
            }
        };
        for chat in chats.into_iter().skip(front) { budget.work(1)?; output.push(chat); }
        Ok(Step::Done(MemoryOutput { chats: output, current_tokens: tokens, effects, error }))
    }

    fn drive(&mut self, mut state: Box<State>, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        loop {
            let call = match self.next(&mut state, budget)? {
                Next::Finish(finish) => return self.finish(state, finish, budget),
                Next::Call(call) => call,
            };
            state = match self.call(state, call, budget)? { Ok(state) => state, Err(step) => return Ok(step) };
        }
    }

    fn call(&mut self, mut state: Box<State>, call: Call, budget: &Budget) -> Result<Result<Box<State>, Step<MemoryOutput>>, StageError> {
        let mut call = call;
        loop {
            budget.check()?;
            let (dependency, step) = match call {
                Call::Count(position) => (Dependency::Tokenizer, lift(self.tokenizer.count(TokenInput::Message(&state.chats[position]), budget)?, Reply::Count)),
                Call::Text(value) => (Dependency::Tokenizer, lift(self.tokenizer.count(TokenInput::Text(&value), budget)?, Reply::Count)),
                Call::Message(message) => (Dependency::Tokenizer, lift(self.tokenizer.count(TokenInput::Message(&message), budget)?, Reply::Count)),
                Call::Prepare(documents) => (Dependency::Retrieval, lift(self.retrieval.prepare(documents, budget)?, Reply::Retrieval)),
                Call::Query(query) => (Dependency::Retrieval, lift(self.retrieval.query(query, budget)?, Reply::Retrieval)),
                Call::Summarize(input) => (Dependency::Summary, lift(self.summary.summarize(input, budget)?, Reply::Summary)),
            };
            let reply = match step {
                Step::NeedsHost(request) => return Ok(Err(self.suspend(state, dependency, request))),
                Step::Done(reply) => reply,
            };
            match self.accept(&mut state, reply, budget)? {
                None => return Ok(Ok(state)),
                Some(Next::Call(next)) => call = next,
                Some(Next::Finish(finish)) => return Ok(Err(self.finish(state, finish, budget)?)),
            }
        }
    }

    fn suspend(&mut self, state: Box<State>, dependency: Dependency, request: HostRequest) -> Step<MemoryOutput> {
        self.suspended = Some((state, dependency));
        Step::NeedsHost(request)
    }
}

impl<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send> MemoryStage for SupaMemory<T, R, S> {
    fn run(&mut self, input: MemoryInput, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        if self.started { return Err(invalid("supa-memory-already-started")); }
        self.started = true;
        match self.admit(input, budget)? {
            Ok(state) => self.drive(state, budget),
            Err(output) => Ok(Step::Done(output)),
        }
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        let (mut state, dependency) = self.suspended.take().ok_or_else(|| invalid("supa-memory-not-suspended"))?;
        budget.check()?;
        let step = match dependency {
            Dependency::Tokenizer => lift(self.tokenizer.resume(reply, budget)?, Reply::Count),
            Dependency::Retrieval => lift(self.retrieval.resume(reply, budget)?, Reply::Retrieval),
            Dependency::Summary => lift(self.summary.resume(reply, budget)?, Reply::Summary),
        };
        let reply = match step {
            Step::NeedsHost(request) => return Ok(self.suspend(state, dependency, request)),
            Step::Done(reply) => reply,
        };
        match self.accept(&mut state, reply, budget)? {
            None => {}
            Some(Next::Call(call)) => { state = match self.call(state, call, budget)? { Ok(state) => state, Err(step) => return Ok(step) }; }
            Some(Next::Finish(finish)) => return self.finish(state, finish, budget),
        }
        self.drive(state, budget)
    }
}
