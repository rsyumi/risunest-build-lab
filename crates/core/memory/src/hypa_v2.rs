//! HypaV2 summary memory (`src/ts/process/memory/hypav2.ts:334-668`) over a captured stored record and injected
//! tokenizer, retrieval and summary services. A failed summary batch is skipped and the scan continues after it, and
//! three consecutive failures end the run with an error, as upstream RisuAI does.
mod data;
pub(crate) mod key;

use crate::hanurai::{Retrieval, RetrievalOutput, ScoredDocument};
use data::{memo_value, Memory};
use key::{unique, Key, KeyIndex};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::num::number_to_text;
use risunest_ir::ordering::sort_descending;
use risunest_ir::stage::{Effect, HostReply, HostRequest, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::text::is_whitespace;
use risunest_ir::{Budget, Charge, OwnedVec, Text, Value};
use std::borrow::Cow;
use std::mem::size_of;

/// Settings captured once before an invocation, after settings normalization. Values keep JavaScript number
/// arithmetic; nothing is rounded or clamped.
pub struct HypaV2Config {
    /// `maxResponse`.
    pub max_response: f64,
    /// `hypaAllocatedTokens`.
    pub allocated_tokens: f64,
    /// `hypaChunkSize`.
    pub chunk_size: f64,
}

/// What the summary helper returned (`summary`, `hypav2.ts:38-143`).
pub enum SummaryOutput {
    Success(Text),
    /// `success: false` with its message, such as a caught summarizer rejection.
    Failed(Text),
}

/// Summarizes one batch text with the mode, prompt and model the service was built with. A returned
/// [`SummaryOutput::Failed`] skips the batch; an `Err` is a terminal dependency failure, cancellation or limit.
pub trait SummaryService {
    fn summarize(&mut self, input: Text, budget: &Budget) -> Result<Step<SummaryOutput>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<SummaryOutput>, StageError>;
}

const DOCUMENT_PREFIX: &str = "search_document: ";
/// `searchDocumentPrefix.length`: ranked documents lose this many UTF-16 units without a prefix check.
const DOCUMENT_PREFIX_UNITS: usize = 17;
const MAX_SUMMARY_FAILURES: u32 = 3;
const MEMORY_FIELD: &str = "hypaV2Data";

/// One HypaV2 invocation. A completed or failed instance cannot run again: dependencies may already have done work.
pub struct HypaV2<T, R, S> {
    config: HypaV2Config,
    stored: Option<Value>,
    tokenizer: T,
    retrieval: R,
    summary: S,
    started: bool,
    suspended: Option<(Box<State>, Dependency)>,
}

#[derive(Clone, Copy)]
enum Dependency { Tokenizer, Summary, Retrieval }

enum Reply { Count(u32), Summary(SummaryOutput), Retrieval(RetrievalOutput) }

enum Phase {
    /// Deducts the already summarized prefix `next..end`.
    Prefix { next: usize, end: usize },
    Loop,
    /// Collects a batch; `closed` once a candidate would exceed the chunk size.
    Scan { closed: bool },
    Recount,
    Summary,
    SummaryCount,
    Main,
    Prepare,
    Query,
    Detail,
    Wrapper,
}

struct AggregatedScore { document: Text, score: f64 }

struct State {
    chats: Vec<PromptMessage>,
    memory: Memory,
    tokens: f64,
    max_context: f64,
    idx: usize,
    failures: u32,
    batch: OwnedVec<usize>,
    batch_tokens: f64,
    /// The message being counted when the count must outlive a yield or becomes part of the result.
    candidate: Option<PromptMessage>,
    main_index: usize,
    main_prompt: Text,
    main_tokens: f64,
    distance: usize,
    scores: OwnedVec<Option<AggregatedScore>>,
    score_index: KeyIndex,
    ranked: OwnedVec<Option<Text>>,
    rank_index: usize,
    detail_prompt: Text,
    detail_tokens: f64,
    phase: Phase,
}

enum Call { Count(usize), Candidate, Summarize(Text), Prepare(Vec<Text>, Charge), Query(Text) }
enum Finish { Error(Text), Complete }
enum Next { Call(Call), Finish(Finish) }

fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }
fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }
fn system(content: Text) -> PromptMessage { PromptMessage::new(Role::System, content) }

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

fn whitespace_only(content: &Text, budget: &Budget) -> Result<bool, StageError> {
    budget.work(content.len_units())?;
    for (index, unit) in content.units().enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        if !is_whitespace(unit) { return Ok(false); }
    }
    Ok(true)
}

fn same_memo(left: &Option<Text>, right: &Option<Text>, budget: &Budget) -> Result<bool, StageError> {
    budget.work(1)?;
    Ok(match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => { budget.work(left.len_units().min(right.len_units()))?; left.equals(right) }
        _ => false,
    })
}

/// A JS canonical array index, which `Object.entries` lists before every other key.
fn array_index(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 { return None; }
    if units[0] == 48 { return (units.len() == 1).then_some(0); }
    let mut value: u32 = 0;
    for &unit in units {
        if !(48..=57).contains(&unit) { return None; }
        value = value.checked_mul(10)?.checked_add(u32::from(unit - 48))?;
    }
    (value < u32::MAX).then_some(value)
}

/// `split("\n\n").map(trim).filter(nonempty)` (`:538-541`).
fn summary_details(summary: &Text, budget: &Budget) -> Result<OwnedVec<Option<Text>>, StageError> {
    budget.work(summary.len_units())?;
    let mut units = OwnedVec::new(budget)?;
    for (index, unit) in summary.units().enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        units.push(unit)?;
    }
    let units = units.as_slice();
    let mut details = OwnedVec::new(budget)?;
    let push = |start: usize, end: usize, details: &mut OwnedVec<Option<Text>>| -> Result<(), StageError> {
        let piece = summary.slice(start, end, budget)?.trim(budget)?;
        if piece.len_units() != 0 { details.push(Some(piece))?; }
        Ok(())
    };
    let (mut start, mut at) = (0, 0);
    while at + 1 < units.len() {
        if units[at] == 10 && units[at + 1] == 10 {
            push(start, at, &mut details)?;
            at += 2;
            start = at;
        } else { at += 1; }
    }
    push(start, units.len(), &mut details)?;
    Ok(details)
}

impl<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send> HypaV2<T, R, S> {
    /// `stored` is the room's `hypaV2Data` as captured, `None` when absent.
    pub fn new(config: HypaV2Config, stored: Option<Value>, tokenizer: T, retrieval: R, summary: S) -> Self {
        Self { config, stored, tokenizer, retrieval, summary, started: false, suspended: None }
    }

    fn admit(&mut self, input: MemoryInput, budget: &Budget) -> Result<Box<State>, StageError> {
        budget.check()?;
        let mut memos = KeyIndex::new();
        for (position, chat) in input.chats.iter().enumerate() {
            memos.first(Key::memo(chat.memo.as_ref(), budget)?, position, budget)?;
        }
        let mut memory = Memory::decode(self.stored.take(), &input.chats, &memos, budget)?;
        memory.clean(&memos, budget)?;
        // Continue after the first chat whose memo is the final surviving main's lastChatMemo (`:380-395`).
        let resumed = match memory.last_chat_memo(budget)? { Some(key) => memos.get(&key, budget)?.map(|position| position + 1), None => None };
        drop(memos);
        let tokens = input.current_tokens - self.config.max_response + self.config.allocated_tokens;
        let end = resumed.unwrap_or(0);
        Ok(Box::new(State {
            chats: input.chats, memory, tokens, max_context: input.max_context_tokens, idx: end, failures: 0,
            batch: OwnedVec::new(budget)?, batch_tokens: 0.0, candidate: None,
            main_index: 0, main_prompt: text("", budget)?, main_tokens: 0.0,
            distance: 1, scores: OwnedVec::new(budget)?, score_index: KeyIndex::new(), ranked: OwnedVec::new(budget)?, rank_index: 0,
            detail_prompt: text("", budget)?, detail_tokens: 0.0,
            phase: Phase::Prefix { next: 0, end },
        }))
    }

    fn next(&self, state: &mut State, budget: &Budget) -> Result<Next, StageError> {
        loop {
            budget.work(1)?;
            let length = state.chats.len();
            match &mut state.phase {
                Phase::Prefix { next, end } => {
                    if *next < *end { return Ok(Next::Call(Call::Count(*next))); }
                    state.phase = Phase::Loop;
                }
                Phase::Loop => {
                    if state.tokens > state.max_context {
                        state.batch = OwnedVec::new(budget)?;
                        state.batch_tokens = 0.0;
                        state.phase = Phase::Scan { closed: false };
                    } else { state.phase = Phase::Main; }
                }
                Phase::Scan { closed } => {
                    // The last four messages are never summarized (`:414-417`).
                    if !*closed && state.batch_tokens < self.config.chunk_size && state.idx + 4 < length {
                        return Ok(Next::Call(Call::Count(state.idx)));
                    }
                    if state.batch.as_slice().is_empty() {
                        if state.idx + 4 >= length {
                            let tokens = number_to_text(state.tokens, budget)?;
                            let error = concat(&[&text("[HypaV2] Input tokens (", budget)?, &tokens, &text(") exceeds max context size (", budget)?,
                                &number_to_text(state.max_context, budget)?,
                                &text("), but can't summarize last 4 messages. Please increase max context size to at least ", budget)?,
                                &tokens, &text(".", budget)?], budget)?;
                            return Ok(Next::Finish(Finish::Error(error)));
                        }
                        state.phase = Phase::Recount;
                        return Ok(Next::Call(Call::Count(state.idx)));
                    }
                    let mut input = text("", budget)?;
                    let separator = text(": ", budget)?;
                    let newline = text("\n", budget)?;
                    for (index, &position) in state.batch.as_slice().iter().enumerate() {
                        budget.work(1)?;
                        if index != 0 { input.append(&newline)?; }
                        let chat = &state.chats[position];
                        input.append(&text(role(chat.role), budget)?)?;
                        input.append(&separator)?;
                        input.append(&chat.content)?;
                    }
                    state.phase = Phase::Summary;
                    return Ok(Next::Call(Call::Summarize(input)));
                }
                Phase::SummaryCount => return Ok(Next::Call(Call::Candidate)),
                Phase::Recount | Phase::Summary | Phase::Wrapper => return Err(invalid("hypa-v2-phase")),
                Phase::Main => {
                    if state.main_index < state.memory.mains.as_slice().len() {
                        let content = state.memory.main_text(state.main_index, budget)?.copy(budget)?;
                        state.candidate = Some(system(content));
                        return Ok(Next::Call(Call::Candidate));
                    }
                    state.phase = Phase::Prepare;
                }
                Phase::Prepare => {
                    let mut documents = OwnedVec::new(budget)?;
                    let prefix = text(DOCUMENT_PREFIX, budget)?;
                    for index in 0..state.memory.details.as_slice().len() {
                        let trimmed = state.memory.detail_text(index, budget)?.trim(budget)?;
                        if trimmed.len_units() != 0 { documents.push(Some(concat(&[&prefix, &trimmed], budget)?))?; }
                    }
                    // The outer Vec is charged while this policy owns it; the retrieval dependency owns it afterwards.
                    let count = documents.as_slice().len();
                    let charge = budget.reserve_owned(count.checked_mul(size_of::<Text>()).ok_or_else(|| StageError::Limit(Cow::Borrowed("owned-bytes")))?)?;
                    let mut list = Vec::new();
                    list.try_reserve_exact(count).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
                    for document in documents.as_mut_slice() { list.push(document.take().expect("each document moves once")); }
                    return Ok(Next::Call(Call::Prepare(list, charge)));
                }
                Phase::Query => {
                    // The original last three chats, newest first, including empty content (`:589-599`).
                    if state.distance <= 3 && state.distance <= length {
                        let query = concat(&[&text("search_query: ", budget)?, &state.chats[length - state.distance].content], budget)?;
                        return Ok(Next::Call(Call::Query(query)));
                    }
                    Self::rank(state, budget)?;
                    state.phase = Phase::Detail;
                }
                Phase::Detail => {
                    let remaining = self.config.allocated_tokens - state.main_tokens - state.detail_tokens;
                    if remaining > 0.0 && state.rank_index < state.ranked.as_slice().len() {
                        let ranked = state.ranked.as_mut_slice()[state.rank_index].take().expect("ranked detail");
                        state.rank_index += 1;
                        let content = ranked.slice(DOCUMENT_PREFIX_UNITS, ranked.len_units(), budget)?;
                        state.candidate = Some(system(content));
                        return Ok(Next::Call(Call::Candidate));
                    }
                    let content = concat(&[&text("<Past Events Summary>", budget)?, &state.main_prompt,
                        &text("</Past Events Summary>\n<Past Events Details>", budget)?, &state.detail_prompt,
                        &text("</Past Events Details>", budget)?], budget)?;
                    state.candidate = Some(system(content));
                    state.phase = Phase::Wrapper;
                    return Ok(Next::Call(Call::Candidate));
                }
            }
        }
    }

    /// `Object.entries(scoredResults).sort((a, b) => b[1] - a[1])`: index-like keys first, then first-seen order.
    fn rank(state: &mut State, budget: &Budget) -> Result<(), StageError> {
        let count = state.scores.as_slice().len();
        let mut indexed = OwnedVec::new(budget)?;
        let mut order = OwnedVec::new(budget)?;
        for (position, entry) in state.scores.as_slice().iter().enumerate() {
            let document = &entry.as_ref().expect("aggregated score").document;
            budget.work(document.len_units().min(10).saturating_add(1))?;
            let mut units = [0u16; 10];
            let length = document.len_units();
            let index = if length <= 10 {
                for (slot, unit) in units.iter_mut().zip(document.units()) { *slot = unit; }
                array_index(&units[..length])
            } else { None };
            match index { Some(index) => indexed.push((index, position))?, None => order.push(position)? }
        }
        if !indexed.as_slice().is_empty() {
            budget.work(indexed.as_slice().len().saturating_mul(usize::BITS as usize))?;
            indexed.as_mut_slice().sort_unstable_by_key(|(index, _)| *index);
            let mut all = OwnedVec::new(budget)?;
            for (_, position) in indexed.as_slice() { all.push(*position)?; }
            for position in order.as_slice() { all.push(*position)?; }
            order = all;
        }
        debug_assert_eq!(order.as_slice().len(), count);
        let scores = &state.scores;
        sort_descending(order.as_mut_slice(), &|position| scores.as_slice()[position].as_ref().expect("aggregated score").score, budget)?;
        state.score_index = KeyIndex::new();
        state.ranked = OwnedVec::new(budget)?;
        for &position in order.as_slice() {
            budget.work(1)?;
            let document = state.scores.as_mut_slice()[position].take().expect("each score ranks once").document;
            state.ranked.push(Some(document))?;
        }
        state.scores = OwnedVec::new(budget)?;
        Ok(())
    }

    fn aggregate(state: &mut State, results: Vec<ScoredDocument>, budget: &Budget) -> Result<(), StageError> {
        let distance = state.distance as f64;
        for result in results {
            let document = match result.document { Some(document) => document, None => text("undefined", budget)? };
            // Assigning a number to `__proto__` is ignored by the object.
            if document.equals_ascii(b"__proto__") { continue; }
            let weighted = result.score / distance;
            let key = Key::text(&document, budget)?;
            match state.score_index.get(&key, budget)? {
                Some(position) => {
                    let previous = &mut state.scores.as_mut_slice()[position].as_mut().expect("aggregated score").score;
                    // `(previous || 0) + score`: a zero or NaN total restarts from zero.
                    let base = if *previous != 0.0 && !previous.is_nan() { *previous } else { 0.0 };
                    *previous = base + weighted;
                }
                None => {
                    let position = state.scores.as_slice().len();
                    state.score_index.first(key, position, budget)?;
                    state.scores.push(Some(AggregatedScore { document, score: 0.0 + weighted }))?;
                }
            }
        }
        state.distance += 1;
        Ok(())
    }

    fn accept(&self, state: &mut State, reply: Reply, budget: &Budget) -> Result<Option<Finish>, StageError> {
        budget.check()?;
        match (std::mem::replace(&mut state.phase, Phase::Loop), reply) {
            (Phase::Prefix { next, end }, Reply::Count(count)) => {
                state.tokens -= f64::from(count);
                state.phase = Phase::Prefix { next: next + 1, end };
            }
            (Phase::Scan { .. }, Reply::Count(count)) => {
                let count = f64::from(count);
                let chat = &state.chats[state.idx];
                // Index zero and whitespace-only content are counted, then skipped without a deduction.
                let closed = if state.idx == 0 || whitespace_only(&chat.content, budget)? { state.idx += 1; false }
                else if state.batch_tokens + count > self.config.chunk_size { true }
                else {
                    state.batch.push(state.idx)?;
                    state.batch_tokens += count;
                    state.idx += 1;
                    false
                };
                state.phase = Phase::Scan { closed };
            }
            (Phase::Recount, Reply::Count(count)) => {
                let count = number_to_text(f64::from(count), budget)?;
                let error = concat(&[&text("[HypaV2] Message tokens (", budget)?, &count, &text(") exceeds chunk size (", budget)?,
                    &number_to_text(self.config.chunk_size, budget)?, &text("). Please increase chunk size to at least ", budget)?,
                    &count, &text(".", budget)?], budget)?;
                return Ok(Some(Finish::Error(error)));
            }
            (Phase::Summary, Reply::Summary(SummaryOutput::Failed(_))) => {
                // The failed batch stays skipped: idx has already advanced past it (`:495-508`).
                state.failures += 1;
                if state.failures >= MAX_SUMMARY_FAILURES {
                    return Ok(Some(Finish::Error(text("[HypaV2] Summarization failed multiple times. Aborting to prevent infinite loop.", budget)?)));
                }
            }
            (Phase::Summary, Reply::Summary(SummaryOutput::Success(summary))) => {
                state.failures = 0;
                state.candidate = Some(system(summary));
                state.phase = Phase::SummaryCount;
            }
            (Phase::SummaryCount, Reply::Count(_)) => {
                // The summary count is observable but never added to the total (`:512-515`).
                let summary = state.candidate.take().ok_or_else(|| invalid("hypa-v2-summary"))?.content;
                let details = summary_details(&summary, budget)?;
                let mut memos = OwnedVec::new(budget)?;
                for &position in state.batch.as_slice() { memos.push(memo_value(&state.chats[position], budget)?)?; }
                let last = *state.batch.as_slice().last().ok_or_else(|| invalid("hypa-v2-batch"))?;
                let last = memo_value(&state.chats[last], budget)?;
                let memos = unique(memos.as_mut_slice(), budget)?;
                state.memory.append(summary, memos, last, details, budget)?;
                state.tokens -= state.batch_tokens;
            }
            (Phase::Main, Reply::Count(count)) => {
                let count = f64::from(count);
                let candidate = state.candidate.take().ok_or_else(|| invalid("hypa-v2-main"))?;
                // The first main summary over half the allocation ends the section (`:564-572`).
                if state.main_tokens + count > self.config.allocated_tokens / 2.0 { state.phase = Phase::Prepare; }
                else {
                    state.main_prompt.append(&text("\n\n", budget)?)?;
                    state.main_prompt.append(&candidate.content)?;
                    state.main_tokens += count;
                    state.main_index += 1;
                    state.phase = Phase::Main;
                }
            }
            (Phase::Prepare, Reply::Retrieval(RetrievalOutput::Prepared)) => { state.phase = Phase::Query; }
            (Phase::Query, Reply::Retrieval(RetrievalOutput::Scored(results))) => {
                Self::aggregate(state, results, budget)?;
                state.phase = Phase::Query;
            }
            (Phase::Detail, Reply::Count(count)) => {
                let count = f64::from(count);
                let candidate = state.candidate.take().ok_or_else(|| invalid("hypa-v2-detail"))?;
                // The first detail over the remaining allocation ends the section (`:606-623`).
                if count > self.config.allocated_tokens - state.main_tokens - state.detail_tokens {
                    state.ranked = OwnedVec::new(budget)?;
                    state.rank_index = 0;
                } else {
                    state.detail_prompt.append(&candidate.content)?;
                    state.detail_prompt.append(&text("\n\n", budget)?)?;
                    state.detail_tokens += count;
                }
                state.phase = Phase::Detail;
            }
            (Phase::Wrapper, Reply::Count(count)) => {
                state.tokens += f64::from(count);
                return Ok(Some(Finish::Complete));
            }
            _ => return Err(invalid("hypa-v2-dependency-output")),
        }
        Ok(None)
    }

    fn finish(&self, state: Box<State>, finish: Finish, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        budget.check()?;
        let State { chats, memory, tokens, idx, candidate, .. } = *state;
        if let Finish::Error(error) = finish {
            // Errors return the original chats and publish no stored-memory change.
            return Ok(Step::Done(MemoryOutput { chats, current_tokens: tokens, effects: Vec::new(), error: Some(error) }));
        }
        let mut message = candidate.ok_or_else(|| invalid("hypa-v2-wrapper"))?;
        message.memo = Some(text("supaMemory", budget)?);
        let value = memory.encode(budget)?;
        let length = chats.len();
        let limit = || StageError::Limit(Cow::Borrowed("owned-bytes"));
        let capacity = length.saturating_sub(idx).checked_add(3).ok_or_else(limit)?;
        // Outer containers are charged while this policy owns them; the returned output is outside the budget.
        let _charge = budget.reserve_owned(capacity.checked_mul(size_of::<PromptMessage>())
            .and_then(|n| n.checked_add(size_of::<Effect>())).ok_or_else(limit)?)?;
        let mut output = Vec::new();
        output.try_reserve_exact(capacity).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
        let mut effects = Vec::new();
        effects.try_reserve_exact(1).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
        output.push(message);
        let mut held: [Option<PromptMessage>; 2] = [None, None];
        for (position, chat) in chats.into_iter().enumerate() {
            budget.work(1)?;
            if position >= idx { output.push(chat); }
            else if position + 2 >= length { held[position + 2 - length] = Some(chat); }
        }
        // Each of the original last two returns unless an output memo already equals its memo (`:642-647`).
        for chat in held.into_iter().flatten() {
            let mut present = false;
            for existing in &output {
                if same_memo(&existing.memo, &chat.memo, budget)? { present = true; break; }
            }
            if !present { output.push(chat); }
        }
        effects.push(Effect::ChatField { field: text(MEMORY_FIELD, budget)?, value });
        Ok(Step::Done(MemoryOutput { chats: output, current_tokens: tokens - self.config.allocated_tokens, effects, error: None }))
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
                    lift(self.tokenizer.count(TokenInput::Message(&state.chats[position]), budget)?, Reply::Count)),
                Call::Candidate => {
                    let candidate = state.candidate.as_ref().ok_or_else(|| invalid("hypa-v2-candidate"))?;
                    (Dependency::Tokenizer, lift(self.tokenizer.count(TokenInput::Message(candidate), budget)?, Reply::Count))
                }
                Call::Summarize(input) => (Dependency::Summary, lift(self.summary.summarize(input, budget)?, Reply::Summary)),
                Call::Prepare(documents, charge) => {
                    let step = self.retrieval.prepare(documents, budget);
                    drop(charge);
                    (Dependency::Retrieval, lift(step?, Reply::Retrieval))
                }
                Call::Query(query) => (Dependency::Retrieval, lift(self.retrieval.query(query, budget)?, Reply::Retrieval)),
            };
            match step {
                Step::NeedsHost(request) => return Ok(self.suspend(state, dependency, request)),
                Step::Done(reply) => {
                    if let Some(finish) = self.accept(&mut state, reply, budget)? { return self.finish(state, finish, budget); }
                }
            }
        }
    }

    fn suspend(&mut self, state: Box<State>, dependency: Dependency, request: HostRequest) -> Step<MemoryOutput> {
        self.suspended = Some((state, dependency));
        Step::NeedsHost(request)
    }
}

impl<T: TokenCounter + Send, R: Retrieval + Send, S: SummaryService + Send> MemoryStage for HypaV2<T, R, S> {
    fn run(&mut self, input: MemoryInput, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        if self.started { return Err(invalid("hypa-v2-already-started")); }
        self.started = true;
        let state = self.admit(input, budget)?;
        self.drive(state, budget)
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        let (mut state, dependency) = self.suspended.take().ok_or_else(|| invalid("hypa-v2-not-suspended"))?;
        budget.check()?;
        let step = match dependency {
            Dependency::Tokenizer => lift(self.tokenizer.resume(reply, budget)?, Reply::Count),
            Dependency::Summary => lift(self.summary.resume(reply, budget)?, Reply::Summary),
            Dependency::Retrieval => lift(self.retrieval.resume(reply, budget)?, Reply::Retrieval),
        };
        match step {
            Step::NeedsHost(request) => return Ok(self.suspend(state, dependency, request)),
            Step::Done(reply) => {
                if let Some(finish) = self.accept(&mut state, reply, budget)? { return self.finish(state, finish, budget); }
            }
        }
        self.drive(state, budget)
    }
}
