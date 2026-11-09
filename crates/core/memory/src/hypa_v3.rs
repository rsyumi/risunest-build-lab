//! HypaV3 summary memory, main implementation (`src/ts/process/memory/hypav3.ts:981-1658`), over a captured stored
//! record and injected tokenizer, retrieval, summary and `Math.random` services. Returned errors that carry memory
//! publish it; errors TS throws publish nothing.
mod data;
mod text;

pub use text::sub_model_summary;

use crate::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use crate::hypa_v2::key::{unique, Key, KeyIndex};
use crate::hypa_v2::{SummaryOutput, SummaryService};
use data::Memory;
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::num::number_to_text;
use risunest_ir::ordering::sort_descending;
use risunest_ir::rng::Rng;
use risunest_ir::stage::{Effect, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{Budget, Charge, OwnedVec, Text, Value};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use text::{sanitize, separator, split, whitespace_only};

/// The history the caller prepared (`HypaV3PreparedHistory`).
pub enum HypaV3PreparedHistory {
    /// A history window: orphan cleanup also counts the memos of the whole conversation.
    Window { effective_memos: Vec<Text> },
    /// A summary-aware history that already starts after `boundary_memo`.
    Boundary { boundary_memo: Text, effective_memos: Vec<Text>, history_start_index: usize },
}

/// The selected preset's settings, captured once before an invocation. Ratios keep JavaScript number arithmetic.
pub struct HypaV3Config {
    /// `maxResponse`.
    pub max_response: f64,
    pub memory_tokens_ratio: f64,
    pub extra_summarization_ratio: f64,
    pub max_chats_per_summary: f64,
    pub recent_memory_ratio: f64,
    pub similar_memory_ratio: f64,
    pub enable_similarity_correction: bool,
    pub preserve_orphaned_memory: bool,
    pub do_not_summarize_user_message: bool,
    pub summary_chunk_separator: Text,
    pub query_chat_count: f64,
    pub use_experimental_impl: bool,
    /// Whether `hypaModel` is a contextual embedding model (`isContextModel`), which groups chunks by summary.
    pub contextual_embedding: bool,
    pub prepared: Option<HypaV3PreparedHistory>,
}

const MEMORY_FIELD: &str = "hypaV3Data";
const OPEN: &str = "<Past Events Summary>\n";
const CLOSE: &str = "\n</Past Events Summary>";
const RRF_K: f64 = 60.0;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// One HypaV3 invocation. A completed or failed instance cannot run again: dependencies may already have done work.
pub struct HypaV3<T, R, S, G> {
    config: HypaV3Config,
    stored: Option<Value>,
    tokenizer: T,
    retrieval: R,
    summary: S,
    rng: G,
    started: bool,
    suspended: Option<(Box<State>, Dependency)>,
}

#[derive(Clone, Copy)]
enum Dependency { Tokenizer, Summary, Retrieval }

enum Reply { Count(u32), Summary(SummaryOutput), Retrieval(RetrievalOutput) }

enum Phase {
    /// Deducts the already summarized prefix `next..end`.
    Prefix { next: usize, end: usize },
    Empty,
    Loop,
    Batch { next: usize },
    Summary,
    Select,
    Important { next: usize },
    RecentStart,
    Recent,
    SimilarStart,
    Prepare,
    Queries,
    Correction,
    Query,
    Similar,
    RandomStart,
    Random,
    Final,
    FinalCount,
}

struct State {
    chats: Vec<PromptMessage>,
    memory: Memory,
    tokens: f64,
    max_context: f64,
    start: usize,
    empty_tokens: f64,
    memory_tokens: f64,
    reserve_empty: bool,
    summarizing: bool,
    target: f64,
    available: f64,
    batch: OwnedVec<usize>,
    batch_end: usize,
    batch_tokens: f64,
    candidate: Option<PromptMessage>,
    selected: OwnedVec<bool>,
    important: OwnedVec<usize>,
    recent: OwnedVec<usize>,
    similar: OwnedVec<usize>,
    random: OwnedVec<usize>,
    reserved_recent: f64,
    consumed_recent: f64,
    reserved_similar: f64,
    consumed_similar: f64,
    reserved_random: f64,
    consumed_random: f64,
    /// Summary indexes the current selection walks, and its position.
    order: OwnedVec<usize>,
    cursor: usize,
    /// Summary chunks: trimmed text and summary index.
    chunks: OwnedVec<(Text, usize)>,
    queries: OwnedVec<Text>,
    query_index: usize,
    /// `simpleCC` totals per chunk, and the chunks in the order the first list inserted them.
    totals: OwnedVec<f64>,
    inserted: OwnedVec<usize>,
    phase: Phase,
}

enum Call { Count(usize), Candidate, Summarize(Text), Prepare(Vec<Text>, Charge), Query(Text) }
enum Finish {
    /// An error with no stored change.
    Error(Text),
    /// An error returned with the working memory, which the caller persists.
    Memory(Text),
    Early,
    Complete(Value),
}
enum Next { Call(Call), Finish(Finish) }

fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }
fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }
fn decline(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
fn system(content: Text) -> PromptMessage { PromptMessage::new(Role::System, content) }
fn limit() -> StageError { StageError::Limit(Cow::Borrowed("owned-bytes")) }

fn lift<T>(step: Step<T>, reply: fn(T) -> Reply) -> Step<Reply> {
    match step { Step::NeedsHost(request) => Step::NeedsHost(request), Step::Done(value) => Step::Done(reply(value)) }
}

fn role(role: Role) -> &'static str {
    match role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" }
}

fn concat(parts: &[&Text], budget: &Budget) -> Result<Text, StageError> {
    let mut result = text("", budget)?;
    for part in parts { result.append(part)?; }
    Ok(result)
}

fn prefixed(prefix: &str, message: &Text, budget: &Budget) -> Result<Text, StageError> { concat(&[&text(prefix, budget)?, message], budget) }

fn safe_integer(value: f64, minimum: f64) -> bool { value.fract() == 0.0 && value >= minimum && value <= MAX_SAFE_INTEGER }

/// `role: sanitized content` per message, joined by newlines (`:1733-1735`).
fn messages(chats: &[PromptMessage], positions: &[usize], budget: &Budget) -> Result<Text, StageError> {
    let mut input = text("", budget)?;
    let separator = text(": ", budget)?;
    let newline = text("\n", budget)?;
    for (index, &position) in positions.iter().enumerate() {
        budget.work(1)?;
        if index != 0 { input.append(&newline)?; }
        let chat = &chats[position];
        input.append(&text(role(chat.role), budget)?)?;
        input.append(&separator)?;
        input.append(&sanitize(&chat.content, budget)?)?;
    }
    Ok(input)
}

fn is(memo: &Option<Text>, value: &str) -> bool { memo.as_ref().is_some_and(|memo| memo.equals_ascii(value.as_bytes())) }

/// `(previous || 0) + addend`: a zero or NaN total restarts from zero.
fn accumulate(previous: f64, addend: f64) -> f64 { (if previous != 0.0 && !previous.is_nan() { previous } else { 0.0 }) + addend }

impl<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send, G: Rng + Send> HypaV3<T, R, S, G> {
    /// `stored` is the room's `hypaV3Data` as captured, `None` when absent.
    pub fn new(config: HypaV3Config, stored: Option<Value>, tokenizer: T, retrieval: R, summary: S, rng: G) -> Self {
        Self { config, stored, tokenizer, retrieval, summary, rng, started: false, suspended: None }
    }

    /// Settings and stored-data checks before the first dependency call. `Err(output)` is a result decided here.
    fn admit(&mut self, input: MemoryInput, budget: &Budget) -> Result<Result<Box<State>, MemoryOutput>, StageError> {
        budget.check()?;
        let config = &self.config;
        if config.use_experimental_impl { return Err(decline("hypa-v3-experimental")); }
        if config.recent_memory_ratio + config.similar_memory_ratio > 1.0 {
            let error = text("[HypaV3] The sum of Recent Memory Ratio and Similar Memory Ratio is greater than 1.", budget)?;
            return Ok(Err(MemoryOutput { chats: input.chats, current_tokens: input.current_tokens, effects: Vec::new(), error: Some(error) }));
        }
        // TS never ends a batch scan with zero or fractional batch sizes, and reads past the history for negative or
        // fractional query counts.
        if !safe_integer(config.max_chats_per_summary, 1.0) { return Err(decline("hypa-v3-max-chats-per-summary")); }
        if !safe_integer(config.query_chat_count, 0.0) { return Err(decline("hypa-v3-query-chat-count")); }
        let similar = config.similar_memory_ratio > 0.0;
        if similar && config.contextual_embedding { return Err(decline("hypa-v3-contextual-embedding")); }
        if similar && separator(&config.summary_chunk_separator, budget)?.is_none() { return Err(decline("hypa-v3-summary-chunk-separator")); }
        let mut memory = Memory::decode(self.stored.take(), budget)?;
        let mut memos = KeyIndex::new();
        for (position, chat) in input.chats.iter().enumerate() {
            memos.first(Key::memo(chat.memo.as_ref(), budget)?, position, budget)?;
        }
        let effective: &[Text] = match &config.prepared {
            Some(HypaV3PreparedHistory::Window { effective_memos } | HypaV3PreparedHistory::Boundary { effective_memos, .. }) => effective_memos,
            None => &[],
        };
        if !config.preserve_orphaned_memory {
            let mut additional = KeyIndex::new();
            for memo in effective { additional.first(Key::text(memo, budget)?, 0, budget)?; }
            memory.clean(&memos, &additional, budget)?;
        }
        let tokens = input.current_tokens - config.max_response;
        let mut start = 0;
        let mut deducted = 0;
        if let Some(last) = memory.last_memo(budget)? {
            if let Some(HypaV3PreparedHistory::Boundary { boundary_memo, history_start_index, .. }) = &config.prepared {
                let current = match last {
                    Value::Text(last) => { budget.work(last.len_units())?; last.equals(boundary_memo) }
                    _ => false,
                };
                if !current {
                    let error = text("[HypaV3] Prepared history summary boundary became stale", budget)?;
                    return Ok(Err(MemoryOutput { chats: input.chats, current_tokens: tokens, effects: Vec::new(), error: Some(error) }));
                }
                start = *history_start_index;
                deducted = start.min(input.chats.len());
            } else {
                // `chat.memo === lastSummaryMemo`: only a string or `undefined` can equal a chat memo.
                let found = match last {
                    Value::Text(_) | Value::Undefined => memos.get(&Key::of(last, budget)?.expect("primitive memo"), budget)?,
                    _ => None,
                };
                if let Some(position) = found { start = position + 1; deducted = start; }
            }
        }
        drop(memos);
        Ok(Ok(Box::new(State {
            chats: input.chats, memory, tokens, max_context: input.max_context_tokens, start,
            empty_tokens: 0.0, memory_tokens: 0.0, reserve_empty: false, summarizing: false, target: 0.0, available: 0.0,
            batch: OwnedVec::new(budget)?, batch_end: 0, batch_tokens: 0.0, candidate: None,
            selected: OwnedVec::new(budget)?, important: OwnedVec::new(budget)?, recent: OwnedVec::new(budget)?,
            similar: OwnedVec::new(budget)?, random: OwnedVec::new(budget)?,
            reserved_recent: 0.0, consumed_recent: 0.0, reserved_similar: 0.0, consumed_similar: 0.0, reserved_random: 0.0, consumed_random: 0.0,
            order: OwnedVec::new(budget)?, cursor: 0, chunks: OwnedVec::new(budget)?, queries: OwnedVec::new(budget)?,
            query_index: 0, totals: OwnedVec::new(budget)?, inserted: OwnedVec::new(budget)?,
            phase: Phase::Prefix { next: 0, end: deducted },
        })))
    }

    fn candidate(state: &mut State, index: usize, budget: &Budget) -> Result<Next, StageError> {
        // Each candidate is counted with the separator that joins it into the memory prompt (`:1268-1271`).
        let content = concat(&[state.memory.text(index, budget)?, &text("\n\n", budget)?], budget)?;
        state.candidate = Some(system(content));
        Ok(Next::Call(Call::Candidate))
    }

    fn unused(state: &mut State, budget: &Budget) -> Result<(), StageError> {
        state.order = OwnedVec::new(budget)?;
        state.cursor = 0;
        for (index, &selected) in state.selected.as_slice().iter().enumerate() {
            budget.work(1)?;
            if !selected { state.order.push(index)?; }
        }
        Ok(())
    }

    fn select(state: &mut State, index: usize, list: fn(&mut State) -> &mut OwnedVec<usize>) -> Result<(), StageError> {
        state.selected.as_mut_slice()[index] = true;
        list(state).push(index)?;
        Ok(())
    }

    fn next(&mut self, state: &mut State, budget: &Budget) -> Result<Next, StageError> {
        let config = &self.config;
        let random_ratio = 1.0 - config.recent_memory_ratio - config.similar_memory_ratio;
        loop {
            budget.work(1)?;
            let length = state.chats.len();
            match &mut state.phase {
                Phase::Prefix { next, end } => {
                    if *next < *end { return Ok(Next::Call(Call::Count(*next))); }
                    state.candidate = Some(system(concat(&[&text(OPEN, budget)?, &text(CLOSE, budget)?], budget)?));
                    state.phase = Phase::Empty;
                    return Ok(Next::Call(Call::Candidate));
                }
                Phase::Loop => {
                    if !state.summarizing || state.tokens <= state.target { state.phase = Phase::Select; continue; }
                    let query = config.query_chat_count;
                    if length.saturating_sub(state.start) as f64 <= query {
                        if state.tokens <= state.max_context { state.phase = Phase::Select; continue; }
                        let error = concat(&[&text("[HypaV3] Cannot summarize further: input token count (", budget)?,
                            &number_to_text(state.tokens, budget)?, &text(") exceeds max context size (", budget)?,
                            &number_to_text(state.max_context, budget)?, &text("), but minimum ", budget)?,
                            &number_to_text(query, budget)?, &text(" messages required.", budget)?], budget)?;
                        return Ok(Next::Finish(Finish::Memory(error)));
                    }
                    state.batch_end = (state.start as f64 + config.max_chats_per_summary).min(length as f64 - query) as usize;
                    state.batch = OwnedVec::new(budget)?;
                    state.batch_tokens = 0.0;
                    state.phase = Phase::Batch { next: state.start };
                }
                Phase::Batch { next } => {
                    if *next < state.batch_end { return Ok(Next::Call(Call::Count(*next))); }
                    // Stop when the batch would take the total below the target, unless it is over the limit (`:1166-1176`).
                    if state.tokens <= state.max_context && state.tokens - state.batch_tokens < state.target {
                        state.phase = Phase::Select;
                        continue;
                    }
                    if state.batch.as_slice().is_empty() {
                        state.tokens -= state.batch_tokens;
                        state.start = state.batch_end;
                        state.phase = Phase::Loop;
                        continue;
                    }
                    let input = messages(&state.chats, state.batch.as_slice(), budget)?;
                    state.phase = Phase::Summary;
                    return Ok(Next::Call(Call::Summarize(input)));
                }
                Phase::Select => {
                    let count = state.memory.summaries.as_slice().len();
                    if count == 0 { return Ok(Next::Finish(Finish::Early)); }
                    for _ in 0..count { budget.work(1)?; state.selected.push(false)?; }
                    state.phase = Phase::Important { next: 0 };
                }
                Phase::Important { next } => {
                    let count = state.memory.summaries.as_slice().len();
                    let mut index = *next;
                    while index < count && !state.memory.important(index, budget)? { index += 1; }
                    if index < count {
                        state.phase = Phase::Important { next: index };
                        return Self::candidate(state, index, budget);
                    }
                    state.phase = Phase::RecentStart;
                }
                Phase::RecentStart => {
                    state.reserved_recent = (state.available * config.recent_memory_ratio).floor();
                    if config.recent_memory_ratio > 0.0 {
                        Self::unused(state, budget)?;
                        state.cursor = state.order.as_slice().len();
                        state.phase = Phase::Recent;
                    } else { state.phase = Phase::SimilarStart; }
                }
                Phase::Recent => {
                    if state.cursor > 0 {
                        let index = state.order.as_slice()[state.cursor - 1];
                        return Self::candidate(state, index, budget);
                    }
                    state.phase = Phase::SimilarStart;
                }
                Phase::SimilarStart => {
                    state.reserved_similar = (state.available * config.similar_memory_ratio).floor();
                    if !(config.similar_memory_ratio > 0.0) { state.phase = Phase::RandomStart; continue; }
                    if random_ratio <= 0.0 { state.reserved_similar += state.reserved_recent - state.consumed_recent; }
                    let separator = separator(&config.summary_chunk_separator, budget)?.ok_or_else(|| invalid("hypa-v3-separator"))?;
                    Self::unused(state, budget)?;
                    state.chunks = OwnedVec::new(budget)?;
                    for &index in state.order.as_slice() {
                        let pieces = split(state.memory.text(index, budget)?, &separator, budget)?;
                        for piece in pieces.as_slice() {
                            let trimmed = piece.trim(budget)?;
                            if trimmed.len_units() != 0 { state.chunks.push((trimmed, index))?; }
                        }
                    }
                    let count = state.chunks.as_slice().len();
                    // The outer Vec is charged while this policy owns it; the retrieval dependency owns it afterwards.
                    let charge = budget.reserve_owned(count.checked_mul(size_of::<Text>()).ok_or_else(limit)?)?;
                    let mut documents = Vec::new();
                    documents.try_reserve_exact(count).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
                    for (chunk, _) in state.chunks.as_slice() { documents.push(chunk.copy(budget)?); }
                    state.phase = Phase::Prepare;
                    return Ok(Next::Call(Call::Prepare(documents, charge)));
                }
                Phase::Queries => {
                    // `chats.slice(-queryChatCount)` with non-blank content; `slice(-0)` is the whole history.
                    let query = config.query_chat_count as usize;
                    let first = if query == 0 { 0 } else { length.saturating_sub(query) };
                    let mut recent = OwnedVec::new(budget)?;
                    for position in first..length {
                        if !whitespace_only(&state.chats[position].content, budget)? { recent.push(position)?; }
                    }
                    if recent.as_slice().is_empty() { state.phase = Phase::RandomStart; continue; }
                    state.queries = OwnedVec::new(budget)?;
                    for &position in recent.as_slice() { state.queries.push(state.chats[position].content.copy(budget)?)?; }
                    state.query_index = 0;
                    if config.enable_similarity_correction && recent.as_slice().len() > 1 {
                        state.phase = Phase::Correction;
                        return Ok(Next::Call(Call::Summarize(messages(&state.chats, recent.as_slice(), budget)?)));
                    }
                    state.phase = Phase::Query;
                }
                Phase::Query => {
                    if state.query_index < state.queries.as_slice().len() {
                        let query = state.queries.as_slice()[state.query_index].copy(budget)?;
                        return Ok(Next::Call(Call::Query(query)));
                    }
                    Self::rank(state, budget)?;
                    state.phase = Phase::Similar;
                }
                Phase::Similar => {
                    if state.cursor < state.order.as_slice().len() {
                        let index = state.order.as_slice()[state.cursor];
                        return Self::candidate(state, index, budget);
                    }
                    state.phase = Phase::RandomStart;
                }
                Phase::RandomStart => {
                    state.reserved_random = (state.available * random_ratio).floor();
                    if !(random_ratio > 0.0) { state.phase = Phase::Final; continue; }
                    let unused_recent = state.reserved_recent - state.consumed_recent;
                    let unused_similar = state.reserved_similar - state.consumed_similar;
                    state.reserved_random += unused_recent + unused_similar;
                    Self::unused(state, budget)?;
                    shuffle(state.order.as_mut_slice(), &mut self.rng, budget)?;
                    state.phase = Phase::Random;
                }
                Phase::Random => {
                    if state.cursor < state.order.as_slice().len() {
                        let index = state.order.as_slice()[state.cursor];
                        return Self::candidate(state, index, budget);
                    }
                    state.phase = Phase::Final;
                }
                Phase::Final => {
                    let mut content = text(OPEN, budget)?;
                    let separator = text("\n\n", budget)?;
                    let mut first = true;
                    for index in 0..state.selected.as_slice().len() {
                        budget.work(1)?;
                        if !state.selected.as_slice()[index] { continue; }
                        if !first { content.append(&separator)?; }
                        first = false;
                        content.append(state.memory.text(index, budget)?)?;
                    }
                    content.append(&text(CLOSE, budget)?)?;
                    state.candidate = Some(system(content));
                    state.phase = Phase::FinalCount;
                    return Ok(Next::Call(Call::Candidate));
                }
                Phase::Empty | Phase::Summary | Phase::Prepare | Phase::Correction | Phase::FinalCount => return Err(invalid("hypa-v3-phase")),
            }
        }
    }

    /// Per-chunk scores of one ranked list, looked up by chunk text in the retrieval order.
    fn scores(state: &State, results: &[ScoredDocument], budget: &Budget) -> Result<OwnedVec<f64>, StageError> {
        let mut positions = KeyIndex::new();
        for (position, result) in results.iter().enumerate() {
            if let Some(document) = &result.document { positions.first(Key::text(document, budget)?, position, budget)?; }
        }
        let mut scores = OwnedVec::new(budget)?;
        for (chunk, _) in state.chunks.as_slice() {
            let position = positions.get(&Key::text(chunk, budget)?, budget)?.ok_or_else(|| invalid("hypa-v3-retrieval-output"))?;
            scores.push(results[position].score)?;
        }
        Ok(scores)
    }

    /// `simpleCC` with weights `(i + 1) / (n(n + 1) / 2)`, accumulated as each list arrives (`:1883-1903`).
    fn combine(state: &mut State, results: Vec<ScoredDocument>, budget: &Budget) -> Result<(), StageError> {
        let scores = Self::scores(state, &results, budget)?;
        drop(results);
        let lists = state.queries.as_slice().len() as f64;
        let weight = (state.query_index as f64 + 1.0) / ((lists * (lists + 1.0)) / 2.0);
        if state.query_index == 0 {
            // `similaritySearchScoredEx` orders chunks by descending similarity; the first list fixes the map order.
            let mut order = OwnedVec::new(budget)?;
            for index in 0..scores.as_slice().len() { order.push(index)?; }
            sort_descending(order.as_mut_slice(), &|index| scores.as_slice()[index], budget)?;
            state.inserted = order;
            state.totals = OwnedVec::new(budget)?;
            for &score in scores.as_slice() { budget.work(1)?; state.totals.push(accumulate(0.0, score * weight))?; }
        } else {
            for (total, &score) in state.totals.as_mut_slice().iter_mut().zip(scores.as_slice()) {
                budget.work(1)?;
                *total = accumulate(*total, score * weight);
            }
        }
        state.query_index += 1;
        Ok(())
    }

    /// Ranks the combined chunks, then `childToParentRRF` with `k = 60` (`:1925-1944`), into `order`.
    fn rank(state: &mut State, budget: &Budget) -> Result<(), StageError> {
        let totals = &state.totals;
        sort_descending(state.inserted.as_mut_slice(), &|chunk| totals.as_slice()[chunk], budget)?;
        let count = state.memory.summaries.as_slice().len();
        let mut slots: OwnedVec<Option<usize>> = OwnedVec::new(budget)?;
        for _ in 0..count { budget.work(1)?; slots.push(None)?; }
        let mut parents = OwnedVec::new(budget)?;
        let mut scores = OwnedVec::new(budget)?;
        for (child, &chunk) in state.inserted.as_slice().iter().enumerate() {
            budget.work(1)?;
            let parent = state.chunks.as_slice()[chunk].1;
            let term = 1.0 / (RRF_K + (child as f64 + 1.0));
            match slots.as_slice()[parent] {
                Some(slot) => { let score = &mut scores.as_mut_slice()[slot]; *score = accumulate(*score, term); }
                None => {
                    slots.as_mut_slice()[parent] = Some(parents.as_slice().len());
                    parents.push(parent)?;
                    scores.push(accumulate(0.0, term))?;
                }
            }
        }
        let mut order = OwnedVec::new(budget)?;
        for slot in 0..parents.as_slice().len() { order.push(slot)?; }
        sort_descending(order.as_mut_slice(), &|slot| scores.as_slice()[slot], budget)?;
        state.order = OwnedVec::new(budget)?;
        for &slot in order.as_slice() { state.order.push(parents.as_slice()[slot])?; }
        state.cursor = 0;
        state.chunks = OwnedVec::new(budget)?;
        Ok(())
    }

    fn accept(&self, state: &mut State, reply: Reply, budget: &Budget) -> Result<Option<Finish>, StageError> {
        budget.check()?;
        let config = &self.config;
        match (std::mem::replace(&mut state.phase, Phase::Loop), reply) {
            (Phase::Prefix { next, end }, Reply::Count(count)) => {
                state.tokens -= f64::from(count);
                state.phase = Phase::Prefix { next: next + 1, end };
            }
            (Phase::Empty, Reply::Count(count)) => {
                state.candidate = None;
                let empty = f64::from(count);
                state.empty_tokens = empty;
                state.memory_tokens = (state.max_context * config.memory_tokens_ratio).floor();
                state.reserve_empty = state.memory.summaries.as_slice().is_empty() && state.tokens + empty <= state.max_context;
                state.available = if state.reserve_empty { 0.0 } else { state.memory_tokens - empty };
                state.tokens += if state.reserve_empty { empty } else { state.memory_tokens };
                state.summarizing = state.tokens > state.max_context;
                state.target = state.max_context * (1.0 - config.extra_summarization_ratio);
            }
            (Phase::Batch { next }, Reply::Count(count)) => {
                state.batch_tokens += f64::from(count);
                let chat = &state.chats[next];
                // Skipped messages are counted into the batch but not summarized (`:1129-1159`).
                let skipped = chat.name.as_ref().is_some_and(|name| name.equals_ascii(b"example_user") || name.equals_ascii(b"example_assistant"))
                    || is(&chat.memo, "NewChatExample") || is(&chat.memo, "NewChat") || whitespace_only(&chat.content, budget)?
                    || (config.do_not_summarize_user_message && matches!(chat.role, Role::User));
                if !skipped { state.batch.push(next)?; }
                state.phase = Phase::Batch { next: next + 1 };
            }
            (Phase::Summary, Reply::Summary(SummaryOutput::Success(summary))) => {
                let mut memos = OwnedVec::new(budget)?;
                for &position in state.batch.as_slice() {
                    memos.push(match &state.chats[position].memo { Some(memo) => Value::Text(memo.copy(budget)?), None => Value::Undefined })?;
                }
                let memos = unique(memos.as_mut_slice(), budget)?;
                state.memory.append(summary, memos, budget)?;
                state.tokens -= state.batch_tokens;
                state.start = state.batch_end;
            }
            (Phase::Summary | Phase::Correction, Reply::Summary(SummaryOutput::Failed(error))) => {
                return Ok(Some(Finish::Memory(prefixed("[HypaV3] Summarization failed: ", &error, budget)?)));
            }
            (Phase::Important { next }, Reply::Count(count)) => {
                let count = f64::from(count);
                // The first oversize important summary ends the section (`:1273-1275`).
                if count > state.available { state.phase = Phase::RecentStart; }
                else {
                    Self::select(state, next, |state| &mut state.important)?;
                    state.available -= count;
                    state.phase = Phase::Important { next: next + 1 };
                }
            }
            (Phase::Recent, Reply::Count(count)) => {
                let count = f64::from(count);
                if count + state.consumed_recent > state.reserved_recent { state.phase = Phase::SimilarStart; }
                else {
                    let index = state.order.as_slice()[state.cursor - 1];
                    Self::select(state, index, |state| &mut state.recent)?;
                    state.consumed_recent += count;
                    state.cursor -= 1;
                    state.phase = Phase::Recent;
                }
            }
            (Phase::Prepare, Reply::Retrieval(RetrievalOutput::Prepared)) => { state.phase = Phase::Queries; }
            (Phase::Correction, Reply::Summary(SummaryOutput::Success(summary))) => {
                state.queries.push(summary)?;
                state.phase = Phase::Query;
            }
            (Phase::Query, Reply::Retrieval(RetrievalOutput::Scored(results))) => {
                Self::combine(state, results, budget)?;
                state.phase = Phase::Query;
            }
            (Phase::Similar, Reply::Count(count)) => {
                let count = f64::from(count);
                if count + state.consumed_similar > state.reserved_similar { state.phase = Phase::RandomStart; }
                else {
                    let index = state.order.as_slice()[state.cursor];
                    Self::select(state, index, |state| &mut state.similar)?;
                    state.consumed_similar += count;
                    state.cursor += 1;
                    state.phase = Phase::Similar;
                }
            }
            (Phase::Random, Reply::Count(count)) => {
                let count = f64::from(count);
                // An oversize random summary is skipped and the walk continues (`:1531-1537`).
                if count + state.consumed_random <= state.reserved_random {
                    let index = state.order.as_slice()[state.cursor];
                    Self::select(state, index, |state| &mut state.random)?;
                    state.consumed_random += count;
                }
                state.cursor += 1;
                state.phase = Phase::Random;
            }
            (Phase::FinalCount, Reply::Count(count)) => {
                state.tokens -= if state.reserve_empty { state.empty_tokens } else { state.memory_tokens };
                state.tokens += f64::from(count);
                if state.tokens > state.max_context {
                    let error = concat(&[&text("[HypaV3] Unexpected error: input token count (", budget)?, &number_to_text(state.tokens, budget)?,
                        &text(") exceeds max context size (", budget)?, &number_to_text(state.max_context, budget)?, &text(")", budget)?], budget)?;
                    return Ok(Some(Finish::Error(error)));
                }
                let mut metrics = Value::Object(OwnedVec::new(budget)?);
                for (name, list) in [("lastImportantSummaries", &state.important), ("lastRecentSummaries", &state.recent),
                    ("lastSimilarSummaries", &state.similar), ("lastRandomSummaries", &state.random)] {
                    let mut indexes = OwnedVec::new(budget)?;
                    for &index in list.as_slice() { indexes.push(Value::Number(index as f64))?; }
                    metrics.define(text(name, budget)?, Value::Array(indexes), budget)?;
                }
                return Ok(Some(Finish::Complete(metrics)));
            }
            _ => return Err(invalid("hypa-v3-dependency-output")),
        }
        Ok(None)
    }

    /// A dependency error inside one of the policy's `try` blocks becomes a returned error with memory; a tokenizer
    /// failure elsewhere propagates as TS throws it, prefixed.
    fn caught(state: &State, dependency: Dependency, error: StageError, budget: &Budget) -> Result<Finish, StageError> {
        let StageError::Host(message) = error else { return Err(error) };
        let message = Text::from_units(&message, budget)?;
        let section = match (dependency, &state.phase) {
            (Dependency::Summary, _) => "[HypaV3] Summarization failed: Error: ",
            (Dependency::Retrieval, _) | (Dependency::Tokenizer, Phase::Similar) => "[HypaV3] Similarity search failed: Error: ",
            (Dependency::Tokenizer, _) => return Err(StageError::Host(prefixed("[HypaV3] ", &message, budget)?.units().collect())),
        };
        Ok(Finish::Memory(prefixed(section, &message, budget)?))
    }

    fn finish(&self, state: Box<State>, finish: Finish, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        budget.check()?;
        let State { chats, memory, tokens, start, candidate, .. } = *state;
        let (message, metrics, error) = match finish {
            Finish::Error(error) => return Ok(Step::Done(MemoryOutput { chats, current_tokens: tokens, effects: Vec::new(), error: Some(error) })),
            Finish::Memory(error) => {
                let value = memory.encode(None, budget)?;
                let effects = vec![Effect::ChatField { field: text(MEMORY_FIELD, budget)?, value }];
                return Ok(Step::Done(MemoryOutput { chats, current_tokens: tokens, effects, error: Some(error) }));
            }
            Finish::Early => (system(concat(&[&text(OPEN, budget)?, &text(CLOSE, budget)?], budget)?), None, None),
            Finish::Complete(metrics) => (candidate.ok_or_else(|| invalid("hypa-v3-memory"))?, Some(metrics), None),
        };
        let mut message = message;
        message.memo = Some(text("supaMemory", budget)?);
        let value = memory.encode(metrics, budget)?;
        let length = chats.len();
        let capacity = length.saturating_sub(start).checked_add(1).ok_or_else(limit)?;
        // Outer containers are charged while this policy owns them; the returned output is outside the budget.
        let _charge = budget.reserve_owned(capacity.checked_mul(size_of::<PromptMessage>())
            .and_then(|n| n.checked_add(size_of::<Effect>())).ok_or_else(limit)?)?;
        let mut output = Vec::new();
        output.try_reserve_exact(capacity).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
        output.push(message);
        for (position, chat) in chats.into_iter().enumerate() {
            budget.work(1)?;
            if position >= start { output.push(chat); }
        }
        let effects = vec![Effect::ChatField { field: text(MEMORY_FIELD, budget)?, value }];
        Ok(Step::Done(MemoryOutput { chats: output, current_tokens: tokens, effects, error }))
    }

    fn deliver(&mut self, mut state: Box<State>, dependency: Dependency, step: Result<Step<Reply>, StageError>, budget: &Budget)
        -> Result<Result<Box<State>, Step<MemoryOutput>>, StageError> {
        let step = match step {
            Ok(step) => step,
            Err(error) => { let finish = Self::caught(&state, dependency, error, budget)?; return Ok(Err(self.finish(state, finish, budget)?)); }
        };
        match step {
            Step::NeedsHost(request) => Ok(Err(self.suspend(state, dependency, request))),
            Step::Done(reply) => match self.accept(&mut state, reply, budget)? {
                Some(finish) => Ok(Err(self.finish(state, finish, budget)?)),
                None => Ok(Ok(state)),
            },
        }
    }

    fn drive(&mut self, mut state: Box<State>, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        loop {
            let call = match self.next(&mut state, budget)? {
                Next::Finish(finish) => return self.finish(state, finish, budget),
                Next::Call(call) => call,
            };
            budget.check()?;
            let (dependency, step) = match call {
                Call::Count(position) => (Dependency::Tokenizer,
                    self.tokenizer.count(TokenInput::Message(&state.chats[position]), budget).map(|step| lift(step, Reply::Count))),
                Call::Candidate => {
                    let candidate = state.candidate.as_ref().ok_or_else(|| invalid("hypa-v3-candidate"))?;
                    (Dependency::Tokenizer, self.tokenizer.count(TokenInput::Message(candidate), budget).map(|step| lift(step, Reply::Count)))
                }
                Call::Summarize(input) => (Dependency::Summary, self.summary.summarize(input, budget).map(|step| lift(step, Reply::Summary))),
                Call::Prepare(documents, charge) => {
                    let step = self.retrieval.prepare(documents, budget);
                    drop(charge);
                    (Dependency::Retrieval, step.map(|step| lift(step, Reply::Retrieval)))
                }
                Call::Query(query) => (Dependency::Retrieval, self.retrieval.query(query, budget).map(|step| lift(step, Reply::Retrieval))),
            };
            state = match self.deliver(state, dependency, step, budget)? { Ok(state) => state, Err(step) => return Ok(step) };
        }
    }

    fn suspend(&mut self, state: Box<State>, dependency: Dependency, request: HostRequest) -> Step<MemoryOutput> {
        self.suspended = Some((state, dependency));
        Step::NeedsHost(request)
    }
}

/// `unused.sort(() => Math.random() - 0.5)` (`:1524-1526`). V8 reads the comparator once per comparison; the stable
/// sort reads the key of its second operand first, so that read carries the draw and the other reads zero.
fn shuffle<G: Rng>(order: &mut [usize], rng: &mut G, budget: &Budget) -> Result<(), StageError> {
    let draw = Cell::new(true);
    let failure: RefCell<Option<StageError>> = RefCell::new(None);
    let rng = RefCell::new(rng);
    sort_descending(order, &|_| {
        if !draw.replace(!draw.get()) || failure.borrow().is_some() { return 0.0; }
        match rng.borrow_mut().random() {
            Ok(value) => value - 0.5,
            Err(error) => { *failure.borrow_mut() = Some(error.into()); 0.0 }
        }
    }, budget)?;
    match failure.into_inner() { Some(error) => Err(error), None => Ok(()) }
}

impl<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send, G: Rng + Send> MemoryStage for HypaV3<T, R, S, G> {
    fn run(&mut self, input: MemoryInput, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        if self.started { return Err(invalid("hypa-v3-already-started")); }
        self.started = true;
        match self.admit(input, budget)? {
            Ok(state) => self.drive(state, budget),
            Err(output) => Ok(Step::Done(output)),
        }
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        let (state, dependency) = self.suspended.take().ok_or_else(|| invalid("hypa-v3-not-suspended"))?;
        budget.check()?;
        let step = match dependency {
            Dependency::Tokenizer => self.tokenizer.resume(reply, budget).map(|step| lift(step, Reply::Count)),
            Dependency::Summary => self.summary.resume(reply, budget).map(|step| lift(step, Reply::Summary)),
            Dependency::Retrieval => self.retrieval.resume(reply, budget).map(|step| lift(step, Reply::Retrieval)),
        };
        match self.deliver(state, dependency, step, budget)? {
            Ok(state) => self.drive(state, budget),
            Err(step) => Ok(step),
        }
    }
}
