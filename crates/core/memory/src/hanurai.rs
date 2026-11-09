//! Hanurai document selection and history budgeting. Retrieval and token accounting remain injected native stages.

use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::num::number_to_text;
use risunest_ir::ordering::sort_descending;
use risunest_ir::stage::{HostReply, MemoryInput, MemoryOutput, MemoryStage, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{Budget, Charge, OwnedVec, Text};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::mem::size_of;

/// Captured once before an invocation. Numeric settings retain JavaScript number arithmetic.
pub struct HanuraiConfig {
    pub split: bool,
    pub reserve_tokens: f64,
    pub context_limit_error: Text,
}

pub struct ScoredDocument {
    /// Original content in dependency order; absent content remains distinct until object-key coercion.
    pub document: Option<Text>,
    pub score: f64,
}

pub enum RetrievalOutput {
    Prepared,
    Scored(Vec<ScoredDocument>),
}

/// A native document-preparation and retrieval dependency. Preparation preserves input order and duplicates;
/// scored queries return the prepared document texts in retrieval order, including duplicate results.
/// Cache/provider work is owned and completed by this dependency, never returned as deferred memory effects.
/// A policy test may replay observations here; such a replay is not an implementation of retrieval.
pub trait Retrieval {
    fn prepare(&mut self, documents: Vec<Text>, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError>;
    fn query(&mut self, query: Text, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError>;
    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<RetrievalOutput>, StageError>;
}

/// One invocation of Hanurai with captured configuration and native dependencies. A completed or failed instance
/// cannot be run again: retrying after a dependency has performed work must be an explicit owner decision.
pub struct Hanurai<T, R> {
    config: HanuraiConfig,
    tokenizer: T,
    retrieval: R,
    started: bool,
    suspended: Option<(State, Pending)>,
}

struct AggregatedScore { document: Text, score: f64 }

struct State {
    chats: VecDeque<PromptMessage>,
    queries: VecDeque<(usize, Text)>,
    scores: OwnedVec<Option<AggregatedScore>>,
    score_indices: HashMap<ScoreKey, usize>,
    index_charge: Option<Charge>,
    ranked: OwnedVec<Option<Text>>,
    rank_index: usize,
    selected: OwnedVec<Text>,
    tokens: f64,
    limit: f64,
    phase: Phase,
}

enum Phase { Prepare(Vec<Text>), Query, Trim, Select }
enum Pending { Prepare, Query(usize), Trim, Candidate(Text) }

impl<T: TokenCounter + Send, R: Retrieval + Send> Hanurai<T, R> {
    pub fn new(config: HanuraiConfig, tokenizer: T, retrieval: R) -> Self {
        Self { config, tokenizer, retrieval, started: false, suspended: None }
    }

    fn state(&self, input: MemoryInput, budget: &Budget) -> Result<State, StageError> {
        let mut documents = Vec::new();
        for chat in input.chats.iter().take(input.chats.len().saturating_sub(3)) {
            budget.work(1)?;
            if self.config.split {
                let units = copy_units(&chat.content, budget)?;
                let units = units.as_slice();
                let (mut start, mut at) = (0, 0);
                while at + 1 < units.len() {
                    if units[at] == 10 && units[at + 1] == 10 {
                        add_document(&chat.content.slice(start, at, budget)?, &mut documents, budget)?;
                        at += 2;
                        start = at;
                    } else { at += 1; }
                }
                add_document(&chat.content.slice(start, units.len(), budget)?, &mut documents, budget)?;
            } else {
                add_document(&chat.content, &mut documents, budget)?;
            }
        }
        let mut queries = VecDeque::new();
        for distance in 1..=3 {
            if let Some(chat) = input.chats.len().checked_sub(distance).and_then(|index| input.chats.get(index)) {
                if chat.content.len_units() != 0 {
                    let mut query = Text::from_str("search_query: ", budget)?;
                    query.append(&chat.content)?;
                    queries.push_back((distance, query));
                }
            }
        }
        Ok(State {
            chats: input.chats.into(), queries, scores: OwnedVec::new(budget)?, score_indices: HashMap::new(),
            index_charge: None, ranked: OwnedVec::new(budget)?, rank_index: 0, selected: OwnedVec::new(budget)?,
            tokens: input.current_tokens + self.config.reserve_tokens,
            limit: input.max_context_tokens, phase: Phase::Prepare(documents),
        })
    }

    fn accept_retrieval(state: &mut State, pending: Pending, output: RetrievalOutput, budget: &Budget) -> Result<(), StageError> {
        match (pending, output) {
            (Pending::Prepare, RetrievalOutput::Prepared) => { state.phase = Phase::Query; }
            (Pending::Query(distance), RetrievalOutput::Scored(results)) => {
                for result in results {
                    let document = match result.document { Some(text) => text, None => Text::from_str("undefined", budget)? };
                    // Copying, hashing, and comparing the key each scan its UTF-16 units.
                    budget.work(document.len_units().saturating_mul(3).saturating_add(1))?;
                    let key = ScoreKey(copy_units(&document, budget)?);
                    let weighted = result.score / distance as f64;
                    if let Some(&index) = state.score_indices.get(&key) {
                        let previous = &mut state.scores.as_mut_slice()[index].as_mut().expect("score index").score;
                        // The TS object update tests the previous number for truthiness, including NaN and zero.
                        if *previous != 0.0 && !previous.is_nan() { *previous += weighted; }
                        else { *previous = weighted; }
                    } else {
                        reserve_score_index(state, budget)?;
                        state.score_indices.insert(key, state.scores.as_slice().len());
                        state.scores.push(Some(AggregatedScore { document, score: weighted }))?;
                    }
                }
            }
            _ => return Err(StageError::Invalid(Cow::Borrowed("hanurai-retrieval-output"))),
        }
        Ok(())
    }

    fn accept_tokens(state: &mut State, pending: Pending, count: u32, budget: &Budget) -> Result<(), StageError> {
        match pending {
            Pending::Trim => { state.tokens -= f64::from(count); }
            Pending::Candidate(content) => {
                let added = f64::from(count) + 2.0;
                state.tokens += added;
                if state.tokens >= state.limit {
                    state.tokens -= added;
                    // An over-budget candidate ends selection, even if a later candidate would be cheaper.
                    state.ranked = OwnedVec::new(budget)?;
                    state.rank_index = 0;
                } else { state.selected.push(content)?; }
            }
            _ => return Err(StageError::Invalid(Cow::Borrowed("hanurai-token-output"))),
        }
        Ok(())
    }

    fn drive(&mut self, mut state: State, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        loop {
            budget.work(1)?;
            match &mut state.phase {
                Phase::Prepare(documents) => {
                    let step = self.retrieval.prepare(std::mem::take(documents), budget)?;
                    match step {
                        Step::NeedsHost(request) => {
                            self.suspended = Some((state, Pending::Prepare));
                            return Ok(Step::NeedsHost(request));
                        }
                        Step::Done(output) => Self::accept_retrieval(&mut state, Pending::Prepare, output, budget)?,
                    }
                }
                Phase::Query => {
                    if let Some((distance, query)) = state.queries.pop_front() {
                        match self.retrieval.query(query, budget)? {
                            Step::NeedsHost(request) => {
                                self.suspended = Some((state, Pending::Query(distance)));
                                return Ok(Step::NeedsHost(request));
                            }
                            Step::Done(output) => Self::accept_retrieval(&mut state, Pending::Query(distance), output, budget)?,
                        }
                    } else {
                        let mut order = OwnedVec::new(budget)?;
                        for index in 0..state.scores.as_slice().len() { order.push(index)?; }
                        sort_descending(order.as_mut_slice(), &|index| state.scores.as_slice()[index].as_ref().expect("score index").score, budget)?;
                        state.score_indices = HashMap::new();
                        state.index_charge = None;
                        for &index in order.as_slice() {
                            let document = state.scores.as_mut_slice()[index].take().expect("unique sorted index").document;
                            state.ranked.push(Some(document.slice(16.min(document.len_units()), document.len_units(), budget)?))?;
                        }
                        state.scores = OwnedVec::new(budget)?;
                        state.phase = Phase::Trim;
                    }
                }
                Phase::Trim => {
                    if state.tokens > state.limit {
                        let Some(chat) = state.chats.pop_front() else {
                            let mut error = self.config.context_limit_error.copy(budget)?;
                            error.append(&Text::from_str("\n\nRequired Tokens: ", budget)?)?;
                            error.append(&number_to_text(state.tokens, budget)?)?;
                            return Ok(Step::Done(output(state, Some(error))));
                        };
                        match self.tokenizer.count(TokenInput::Message(&chat), budget)? {
                            Step::NeedsHost(request) => {
                                self.suspended = Some((state, Pending::Trim));
                                return Ok(Step::NeedsHost(request));
                            }
                            Step::Done(count) => Self::accept_tokens(&mut state, Pending::Trim, count, budget)?,
                        }
                    } else {
                        state.tokens -= self.config.reserve_tokens;
                        state.phase = Phase::Select;
                    }
                }
                Phase::Select => {
                    if state.rank_index < state.ranked.as_slice().len() {
                        let content = state.ranked.as_mut_slice()[state.rank_index].take().expect("ranked candidate");
                        state.rank_index += 1;
                        let mut present = false;
                        for chat in &state.chats {
                            if equal_text(&chat.content, &content, budget)? { present = true; break; }
                        }
                        if present { continue; }
                        let candidate = memory_message(content.copy(budget)?, budget)?;
                        match self.tokenizer.count(TokenInput::Message(&candidate), budget)? {
                            Step::NeedsHost(request) => {
                                self.suspended = Some((state, Pending::Candidate(content)));
                                return Ok(Step::NeedsHost(request));
                            }
                            Step::Done(count) => Self::accept_tokens(&mut state, Pending::Candidate(content), count, budget)?,
                        }
                    } else {
                        let mut content = Text::from_str("", budget)?;
                        for (index, selected) in state.selected.as_slice().iter().enumerate() {
                            budget.work(1)?;
                            if index != 0 { content.append(&Text::from_str("\n\n", budget)?)?; }
                            content.append(selected)?;
                        }
                        state.chats.push_front(memory_message(content, budget)?);
                        return Ok(Step::Done(output(state, None)));
                    }
                }
            }
        }
    }
}

impl<T: TokenCounter + Send, R: Retrieval + Send> MemoryStage for Hanurai<T, R> {
    fn run(&mut self, input: MemoryInput, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        if self.started { return Err(StageError::Invalid(Cow::Borrowed("hanurai-already-started"))); }
        self.started = true;
        let state = self.state(input, budget)?;
        self.drive(state, budget)
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<MemoryOutput>, StageError> {
        let (mut state, pending) = self.suspended.take().ok_or(StageError::Invalid(Cow::Borrowed("hanurai-not-suspended")))?;
        budget.check()?;
        match pending {
            Pending::Prepare | Pending::Query(_) => match self.retrieval.resume(reply, budget)? {
                Step::NeedsHost(request) => {
                    self.suspended = Some((state, pending));
                    return Ok(Step::NeedsHost(request));
                }
                Step::Done(output) => Self::accept_retrieval(&mut state, pending, output, budget)?,
            },
            Pending::Trim | Pending::Candidate(_) => match self.tokenizer.resume(reply, budget)? {
                Step::NeedsHost(request) => {
                    self.suspended = Some((state, pending));
                    return Ok(Step::NeedsHost(request));
                }
                Step::Done(count) => Self::accept_tokens(&mut state, pending, count, budget)?,
            },
        }
        self.drive(state, budget)
    }
}

fn add_document(content: &Text, documents: &mut Vec<Text>, budget: &Budget) -> Result<(), StageError> {
    let trimmed = content.trim(budget)?;
    if trimmed.len_units() != 0 {
        let mut document = Text::from_str("search_document: ", budget)?;
        document.append(&trimmed)?;
        documents.push(document);
    }
    Ok(())
}

fn memory_message(content: Text, budget: &Budget) -> Result<PromptMessage, StageError> {
    let mut message = PromptMessage::new(Role::System, content);
    message.memo = Some(Text::from_str("supaMemory", budget)?);
    Ok(message)
}

fn output(state: State, error: Option<Text>) -> MemoryOutput {
    MemoryOutput { chats: state.chats.into(), current_tokens: state.tokens, effects: Vec::new(), error }
}


struct ScoreKey(OwnedVec<u16>);
impl Hash for ScoreKey {
    fn hash<H: Hasher>(&self, state: &mut H) { self.0.as_slice().hash(state); }
}
impl PartialEq for ScoreKey {
    fn eq(&self, other: &Self) -> bool { self.0.as_slice() == other.0.as_slice() }
}
impl Eq for ScoreKey {}

fn copy_units(text: &Text, budget: &Budget) -> Result<OwnedVec<u16>, StageError> {
    budget.work(text.len_units())?;
    let mut units = OwnedVec::new(budget)?;
    for unit in text.units() { units.push(unit)?; }
    Ok(units)
}

fn equal_text(left: &Text, right: &Text, budget: &Budget) -> Result<bool, StageError> {
    budget.work(1)?;
    if left.len_units() != right.len_units() { return Ok(false); }
    budget.work(left.len_units())?;
    for (index, (a, b)) in left.units().zip(right.units()).enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        if a != b { return Ok(false); }
    }
    Ok(true)
}

fn reserve_score_index(state: &mut State, budget: &Budget) -> Result<(), StageError> {
    if state.score_indices.len() < state.score_indices.capacity() { return Ok(()); }
    let limit = || StageError::Limit(Cow::Borrowed("owned-bytes"));
    // HashMap capacity excludes spare buckets and control bytes. Twice capacity plus one bucket bounds those
    // allocations, including the small-table layout; retain the old charge until growth finishes.
    let bytes_for = |capacity: usize| capacity.checked_add(1).and_then(|n| n.checked_mul(2))
        .and_then(|n| n.checked_mul(size_of::<(ScoreKey, usize)>() + 1)).and_then(|n| n.checked_add(16)).ok_or_else(limit);
    let capacity = state.score_indices.capacity().checked_mul(2).and_then(|n| n.checked_add(1)).ok_or_else(limit)?.max(3);
    let bytes = bytes_for(capacity)?;
    let mut charge = budget.reserve_owned(bytes)?;
    state.score_indices.try_reserve(1).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
    let actual = bytes_for(state.score_indices.capacity())?;
    if actual > bytes { charge = budget.reserve_owned(actual)?; }
    state.index_charge = Some(charge);
    Ok(())
}
