use crate::persistent_store::{PersistentStore, StoreError};
use crate::native_log::CommandFailure;
use crate::persistent_store::script_access::{self as store_access, ExistenceFence, FrozenAttempt, MessageFence, ReadView, Resolution, Splice};
pub(crate) use crate::persistent_store::script_access::{RecoveryIssue, Target};
use risunest_records::edits::relative_index;
use serde::Serialize;
use serde_json::{Map, Value};
use std::{collections::{BTreeMap, BTreeSet, HashMap, VecDeque}, mem::size_of, sync::{Arc, Condvar, Mutex, MutexGuard, Weak}, time::{Duration, Instant}};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Error {
    Closed, Expired, OwnerInvalid, InvocationInvalid, WaitInvalid, PendingCommit, StaleTarget, StaleMessages,
    Conflict, HistoryCoercion, SegmentIntegrity, SegmentExpired, Limit(&'static str), Storage(String),
}

type Result<T> = std::result::Result<T, Error>;

fn storage_error(error: StoreError) -> Error {
    match &error {
        StoreError::RevisionConflict { .. } => Error::Conflict,
        StoreError::Validation { message } if message == "native-script:stale-messages" => Error::StaleMessages,
        StoreError::Validation { message } if matches!(message.as_str(), "native-script:stale-owner" | "native-script:stale-authority" | "native-script:missing-character" | "native-script:missing-conversation") => Error::StaleTarget,
        _ => Error::Storage(error.code().into_owned()),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> { mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) }

#[derive(Clone)]
pub(crate) struct OwnerPermit(Arc<Mutex<bool>>);

impl OwnerPermit {
    pub(crate) fn new() -> Self { Self(Arc::new(Mutex::new(true))) }
    pub(crate) fn revoke(&self) { *lock(&self.0) = false; }
    fn admit(&self) -> Result<MutexGuard<'_, bool>> {
        let guard = lock(&self.0);
        if !*guard { return Err(Error::OwnerInvalid); }
        Ok(guard)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Limits {
    pub active_scopes: usize,
    pub aggregate_bytes: usize,
    pub scope_bytes: usize,
    pub variable_bytes: usize,
    pub variable_count: usize,
    pub staged_bytes: usize,
    pub staged_rows: usize,
    pub read_page_rows: usize,
    pub read_page_bytes: usize,
    pub result_bytes: usize,
    pub waits: usize,
    pub receipts: usize,
    pub receipt_bytes: usize,
    pub commit_attempts: usize,
    pub lifetime: Duration,
}

impl Limits {
    fn validate(&self) -> Result<()> {
        if [self.active_scopes, self.aggregate_bytes, self.scope_bytes, self.variable_bytes, self.variable_count,
            self.staged_bytes, self.staged_rows, self.read_page_rows, self.read_page_bytes, self.result_bytes,
            self.waits, self.receipts, self.receipt_bytes, self.commit_attempts].contains(&0)
            || self.lifetime.is_zero() || self.read_page_rows > 4096 { return Err(Error::Limit("configuration")); }
        Ok(())
    }
}

#[derive(Default)]
struct Usage { bytes: usize, scopes: BTreeMap<String, Weak<Mutex<State>>> }

struct PoolInner {
    limits: Limits,
    clock: Arc<dyn Fn() -> Duration + Send + Sync>,
    usage: Mutex<Usage>,
    wake: Condvar,
}

#[derive(Clone)]
/// The service owner retains this pool through scope cleanup and unresolved write recovery.
pub(crate) struct AccessPool(Arc<PoolInner>, Arc<Mutex<BTreeMap<String, Arc<Mutex<State>>>>>);

impl AccessPool {
    pub(crate) fn new(limits: Limits) -> Result<Self> {
        let start = Instant::now();
        Self::with_clock(limits, Arc::new(move || start.elapsed()))
    }

    pub(crate) fn with_clock(limits: Limits, clock: Arc<dyn Fn() -> Duration + Send + Sync>) -> Result<Self> {
        limits.validate()?;
        let pool = Arc::new(PoolInner { limits, clock, usage: Mutex::new(Usage::default()), wake: Condvar::new() });
        let weak = Arc::downgrade(&pool);
        std::thread::Builder::new().name("script-access-expiry".into()).spawn(move || {
            while let Some(pool) = weak.upgrade() {
                if Arc::strong_count(&pool) == 1 { break; }
                pool.expire();
                let guard = lock(&pool.usage);
                let waited = pool.wake.wait_timeout(guard, Duration::from_millis(20));
                drop(waited);
            }
        }).map_err(|_| Error::Storage("expiry-worker".into()))?;
        Ok(Self(pool, Arc::new(Mutex::new(BTreeMap::new()))))
    }

    pub(crate) fn expire(&self) { self.0.expire(); }
    pub(crate) fn used_bytes(&self) -> usize { lock(&self.0.usage).bytes }

    pub(crate) fn recover_pending(&self, store: &mut PersistentStore) -> Vec<Result<SegmentOutcome>> {
        let pending: Vec<_> = lock(&self.1).iter().map(|(id, state)| (id.clone(), state.clone())).collect();
        pending.into_iter().map(|(id, state)| {
            let mut state = lock(&state);
            let result = drive(&mut state, store);
            if state.sealed.is_none() { lock(&self.1).remove(&id); }
            result
        }).collect()
    }
}

impl PoolInner {
    fn expire(&self) {
        let scopes: Vec<_> = {
            let mut usage = lock(&self.usage);
            usage.scopes.retain(|_, scope| scope.strong_count() != 0);
            usage.scopes.values().cloned().collect()
        };
        let now = (self.clock)();
        for scope in scopes.into_iter().filter_map(|scope| scope.upgrade()) {
            let mut state = lock(&scope);
            if now >= state.deadline { state.expire(); }
        }
    }

    fn resize(&self, old: usize, new: usize) -> Result<()> {
        let mut usage = lock(&self.usage);
        let next = usage.bytes.checked_sub(old).and_then(|bytes| bytes.checked_add(new)).ok_or(Error::Limit("bytes"))?;
        if next > self.limits.aggregate_bytes { return Err(Error::Limit("aggregate-bytes")); }
        usage.bytes = next;
        Ok(())
    }
}

struct Charge { pool: Arc<PoolInner>, scope: Arc<Mutex<usize>>, bytes: usize }
impl Charge {
    fn resize(&mut self, bytes: usize) -> Result<()> {
        let mut scope = lock(&self.scope);
        let next = scope.checked_sub(self.bytes).and_then(|current| current.checked_add(bytes)).ok_or(Error::Limit("scope-bytes"))?;
        if next > self.pool.limits.scope_bytes { return Err(Error::Limit("scope-bytes")); }
        self.pool.resize(self.bytes, bytes)?;
        *scope = next;
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Charge {
    fn drop(&mut self) { let _ = self.resize(0); self.pool.wake.notify_all(); }
}

pub(crate) struct ReadResult<T> { value: T, _charge: Charge }
impl<T> ReadResult<T> { pub(crate) fn value(&self) -> &T { &self.value } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum Exposure { Full, Window { start: i64, length: i64 } }

#[derive(Clone, Copy, Debug)]
pub(crate) enum NumberArgument { Value(f64), Invalid }

#[derive(Clone, Copy, Debug)]
pub(crate) enum HistorySelection { One(NumberArgument), Recent(NumberArgument), FullExposed }

pub(crate) enum HistoryRows { One(Option<Value>), Many(Vec<Value>) }

pub(crate) struct OutputReservation { _charge: Charge }

pub(crate) struct ChargedText { text: String, charge: Charge, limit: usize }

impl AsRef<str> for ChargedText { fn as_ref(&self) -> &str { &self.text } }

impl ChargedText {
    pub(crate) fn push(&mut self, text: &str) -> Result<()> {
        let length = self.text.len().checked_add(text.len()).filter(|length| *length <= self.limit)
            .ok_or(Error::Limit("encoded-history-bytes"))?;
        if length > self.text.capacity() {
            let capacity = length.max(self.text.capacity().saturating_mul(2).min(self.limit));
            self.charge.resize(size_of::<String>().saturating_add(capacity))?;
            self.text.try_reserve_exact(capacity - self.text.len()).map_err(|_| Error::Limit("encoded-history-allocation"))?;
            self.charge.resize(size_of::<String>().saturating_add(self.text.capacity()))?;
            if self.text.capacity() > self.limit { return Err(Error::Limit("encoded-history-bytes")); }
        }
        self.text.push_str(text);
        Ok(())
    }

    pub(crate) fn scratch(&self, bytes: usize) -> Result<OutputReservation> {
        let mut charge = Charge { pool: self.charge.pool.clone(), scope: self.charge.scope.clone(), bytes: 0 };
        charge.resize(bytes)?;
        Ok(OutputReservation { _charge: charge })
    }
}

impl<T> ReadResult<T> {
    pub(crate) fn text_writer(&self, limit: usize) -> Result<ChargedText> {
        let mut charge = Charge { pool: self._charge.pool.clone(), scope: self._charge.scope.clone(), bytes: 0 };
        charge.resize(size_of::<String>())?;
        Ok(ChargedText { text: String::new(), limit: limit.min(charge.pool.limits.result_bytes), charge })
    }
}


impl Exposure {
    fn range(self, total: i64) -> (i64, i64) {
        match self { Self::Full => (0, total), Self::Window { start, length } => (start.min(total), start.saturating_add(length).min(total)) }
    }
    fn position(self, total: i64, index: f64) -> Option<i64> {
        let (start, end) = self.range(total);
        let index = if index.is_nan() { 0.0 } else { index.trunc() };
        if !index.is_finite() || index < i64::MIN as f64 || index >= i64::MAX as f64 { return None; }
        let index = index as i64;
        let position = if index < 0 { end.checked_add(index)? } else { index };
        (position >= start && position < end).then_some(position)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InvocationPermit { scope: String, sequence: u64 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WaitPermit { invocation: InvocationPermit, sequence: u64 }
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentId { scope: String, sequence: u64 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum FinishMode { Variables, Messages, WithoutMessages, IfChanged }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentStatus { Committed(i64), NoChange, NotCommitted, Unresolved(RecoveryIssue) }

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentOutcome {
    pub id: SegmentId,
    pub digest: String,
    pub status: SegmentStatus,
    pub changed_keys: Vec<String>,
    pub messages_changed: bool,
    pub error: Option<Error>,
}

struct Completed { mode: FinishMode, outcome: SegmentOutcome }
struct Invocation { sequence: u64, aborted: bool, waits: BTreeSet<u64> }
/// Staged rows: `edits` replace stored rows by stored index; `layout`, once a row was removed, inserted or replaced, lists
/// the history's rows.
struct Staged { view: ReadView, edits: BTreeMap<i64, Value>, layout: Option<Vec<Piece>> }

/// Rows of a restructured history: the stored rows `start..start + count`, in stored order across the layout, or one row
/// the store does not hold.
#[derive(Clone)]
enum Piece { Stored { start: i64, count: i64 }, New(Value) }

impl Piece {
    fn len(&self) -> i64 { match self { Self::Stored { count, .. } => *count, Self::New(_) => 1 } }
}

/// A message `setFullChatMain` assigns: `role` and `data` as parsed, and `chatId` when it is a string.
pub(crate) struct FullMessage { pub role: String, pub data: String, pub chat_id: Option<String> }

impl Staged {
    fn count(&self) -> i64 { self.layout.as_deref().map_or(self.view.messages.count, layout_len) }
}

fn layout_len(layout: &[Piece]) -> i64 { layout.iter().map(Piece::len).sum() }

/// The piece holding row `at` and the row's offset in it.
fn locate(layout: &[Piece], at: i64) -> Option<(usize, i64)> {
    let mut position = 0;
    for (index, piece) in layout.iter().enumerate() {
        if at < position + piece.len() { return Some((index, at - position)); }
        position += piece.len();
    }
    None
}

/// The splices that turn the `stored` rows into `layout`, left to right.
fn splices(layout: &[Piece], stored: i64) -> Vec<Splice> {
    let mut splices = Vec::new();
    let (mut original, mut position) = (0, 0);
    let mut messages = Vec::new();
    for piece in layout.iter().map(Some).chain([None]) {
        let (start, count) = match piece {
            Some(Piece::New(value)) => { messages.push(value.clone()); continue }
            Some(Piece::Stored { start, count }) => (*start, *count),
            None => (stored, 0),
        };
        if start > original || !messages.is_empty() {
            let inserted = messages.len() as i64;
            splices.push(Splice { start: position, delete_count: start - original, messages: std::mem::take(&mut messages) });
            position += inserted;
        }
        position += count;
        original = start + count;
    }
    splices
}

/// Rows `start..start + count` as a script reads them: from `view`, through the staged rows when they are staged.
fn history_rows(view: &ReadView, staged: Option<&Staged>, target: &Target, start: i64, count: i64) -> Result<Vec<Value>> {
    let read = |start: i64, count: i64| -> Result<Vec<Value>> {
        let mut rows = view.read(target, start, count).map_err(storage_error)?;
        for (relative, row) in rows.iter_mut().enumerate() {
            if let Some(edited) = staged.and_then(|staged| staged.edits.get(&(start + relative as i64))) { *row = edited.clone(); }
        }
        Ok(rows)
    };
    let Some(layout) = staged.and_then(|staged| staged.layout.as_deref()) else { return read(start, count) };
    let (mut rows, mut position, end) = (Vec::new(), 0, start + count);
    for piece in layout {
        let (from, to) = (start.max(position), end.min(position + piece.len()));
        if from < to {
            match piece {
                Piece::Stored { start: stored, .. } => rows.extend(read(stored + from - position, to - from)?),
                Piece::New(row) => rows.push(row.clone()),
            }
        }
        position += piece.len();
        if position >= end { break; }
    }
    if rows.len() as i64 != count { return Err(Error::Limit("history-range")); }
    Ok(rows)
}

struct Sealed {
    id: SegmentId,
    mode: FinishMode,
    digest: String,
    variables: Vec<(String, String)>,
    edits: BTreeMap<i64, Value>,
    splices: Vec<Splice>,
    messages: Option<MessageFence>,
    primary_error: Option<Error>,
    attempt: Option<FrozenAttempt>,
    changed_keys: Vec<String>,
    messages_changed: bool,
}

struct State {
    id: String,
    target: Target,
    existence: ExistenceFence,
    owner: OwnerPermit,
    exposure: Exposure,
    deadline: Duration,
    charge: Charge,
    recovery: Weak<Mutex<BTreeMap<String, Arc<Mutex<State>>>>>,
    closed: Option<Error>,
    failure: Option<Error>,
    defaults: Option<Vec<(String, String)>>,
    variables: Vec<(String, String)>,
    first_read: Option<MessageFence>,
    staged: Option<Staged>,
    invocation_sequence: u64,
    wait_sequence: u64,
    invocation: Option<Invocation>,
    segment_sequence: u64,
    sealed: Option<Sealed>,
    completed: VecDeque<Completed>,
}

fn string_bytes(value: &String) -> usize { size_of::<String>().saturating_add(value.capacity()) }
fn value_bytes(value: &Value) -> usize {
    store_access::owned_value_bytes(value)
}
fn variables_bytes(values: &Vec<(String, String)>) -> usize {
    (values.capacity() - values.len()) * size_of::<(String, String)>()
        + values.iter().map(|(key, value)| string_bytes(key).saturating_add(string_bytes(value))).sum::<usize>()
}
fn edits_bytes(values: &BTreeMap<i64, Value>) -> usize { values.values().map(|value| value_bytes(value).saturating_add(8 * size_of::<usize>())).sum() }
fn layout_bytes(layout: &Vec<Piece>) -> usize {
    layout.capacity() * size_of::<Piece>() + layout.iter().map(|piece| match piece { Piece::New(row) => value_bytes(row), _ => 0 }).sum::<usize>()
}
fn splices_bytes(splices: &Vec<Splice>) -> usize {
    splices.capacity() * size_of::<Splice>() + splices.iter()
        .map(|splice| splice.messages.capacity() * size_of::<Value>() + splice.messages.iter().map(value_bytes).sum::<usize>()).sum::<usize>()
}
fn receipt_bytes(value: &Completed) -> usize {
    size_of::<Completed>() + value.outcome.digest.capacity() + value.outcome.id.scope.capacity()
        + value.outcome.changed_keys.iter().map(string_bytes).sum::<usize>()
}

impl State {
    fn bytes(&self) -> usize {
        let staged = self.staged.as_ref().map_or(0, |staged| size_of::<Staged>() + value_bytes(&staged.view.metadata)
            + staged.view.existence.owned_bytes() + staged.view.messages.owned_bytes() + edits_bytes(&staged.edits)
            + staged.layout.as_ref().map_or(0, layout_bytes));
        let sealed = self.sealed.as_ref().map_or(0, |sealed| variables_bytes(&sealed.variables) + edits_bytes(&sealed.edits)
            + splices_bytes(&sealed.splices)
            + sealed.id.scope.capacity() + sealed.digest.capacity() + sealed.changed_keys.iter().map(string_bytes).sum::<usize>()
            + sealed.messages.as_ref().map_or(0, MessageFence::owned_bytes)
            + sealed.attempt.as_ref().map_or(0, |attempt| attempt.retained_bytes));
        size_of::<Self>() + self.id.capacity() + self.target.character_id.capacity() + self.target.conversation_id.capacity()
            + self.target.library_identity.capacity() + self.existence.owned_bytes()
            + self.first_read.as_ref().map_or(0, MessageFence::owned_bytes) + variables_bytes(&self.variables)
            + self.defaults.as_ref().map_or(0, |defaults| variables_bytes(defaults)) + staged + sealed
            + self.completed.iter().map(receipt_bytes).sum::<usize>()
            + (self.completed.capacity() - self.completed.len()) * size_of::<Completed>()
            + self.invocation.as_ref().map_or(0, |invocation| invocation.waits.len().saturating_mul(64))
    }

    fn account(&mut self) -> Result<()> {
        let bytes = self.bytes();
        if bytes > self.charge.pool.limits.scope_bytes { return Err(Error::Limit("scope-bytes")); }
        self.charge.resize(bytes)
    }

    fn expire(&mut self) {
        if self.closed.is_none() {
            self.closed = Some(Error::Expired);
            self.invocation = None;
            self.staged = None;
            self.first_read = None;
            self.variables = Vec::new();
            self.defaults = None;
            let _ = self.account();
        }
    }

    fn check_open(&mut self) -> Result<()> {
        if (self.charge.pool.clock)() >= self.deadline { self.expire(); }
        if let Some(error) = &self.closed { return Err(error.clone()); }
        Ok(())
    }

    fn check_invocation(&mut self, permit: &InvocationPermit) -> Result<()> {
        self.check_open()?;
        if permit.scope != self.id || self.invocation.as_ref().map(|invocation| invocation.sequence) != Some(permit.sequence) {
            return Err(Error::InvocationInvalid);
        }
        if self.sealed.is_some() { return Err(Error::PendingCommit); }
        Ok(())
    }

    fn remember<T>(&mut self, result: Result<T>) -> Result<T> {
        if let Err(error @ (Error::OwnerInvalid | Error::StaleTarget | Error::StaleMessages)) = &result {
            self.failure.get_or_insert_with(|| error.clone());
        }
        result
    }

    fn live(&mut self, store: &PersistentStore) -> Result<ReadView> {
        let owner = self.owner.clone();
        let result = (|| {
            let _admission = owner.admit()?;
            let view = store.script_read_view(&self.target).map_err(storage_error)?;
            if view.existence != self.existence { return Err(Error::StaleTarget); }
            Ok(view)
        })();
        self.remember(result)
    }

    fn record_read(&mut self, view: &ReadView) -> Result<()> {
        if self.first_read.is_none() {
            self.first_read = Some(view.messages.clone());
            if let Err(error) = self.account() { self.first_read = None; return Err(error); }
        }
        Ok(())
    }

    fn stage(&mut self, store: &PersistentStore) -> Result<()> {
        if self.staged.is_some() { return Ok(()); }
        let view = self.live(store)?;
        if self.first_read.as_ref().is_some_and(|fence| fence != &view.messages) { return self.remember(Err(Error::StaleMessages)); }
        self.record_read(&view)?;
        self.staged = Some(Staged { view, edits: BTreeMap::new(), layout: None });
        if let Err(error) = self.account() { self.staged = None; return Err(error); }
        Ok(())
    }

    /// Puts staged edits and layout in place once they fit the limits.
    fn restage(&mut self, edits: BTreeMap<i64, Value>, layout: Option<Vec<Piece>>) -> Result<()> {
        let limits = &self.charge.pool.limits;
        let rows = edits.len().saturating_add(layout.as_ref().map_or(0, Vec::len));
        let bytes = edits_bytes(&edits).saturating_add(layout.as_ref().map_or(0, layout_bytes));
        if rows > limits.staged_rows || bytes > limits.staged_bytes { return Err(Error::Limit("staged-messages")); }
        let staged = self.staged.as_mut().unwrap();
        let previous = (std::mem::replace(&mut staged.edits, edits), std::mem::replace(&mut staged.layout, layout));
        if let Err(error) = self.account() {
            let staged = self.staged.as_mut().unwrap();
            (staged.edits, staged.layout) = previous;
            return Err(error);
        }
        Ok(())
    }

    fn output<T>(&self, value: T, bytes: usize) -> Result<ReadResult<T>> {
        if bytes > self.charge.pool.limits.result_bytes { return Err(Error::Limit("result-bytes")); }
        let mut charge = Charge { pool: self.charge.pool.clone(), scope: self.charge.scope.clone(), bytes: 0 };
        charge.resize(bytes)?;
        Ok(ReadResult { value, _charge: charge })
    }
}

pub(crate) struct ScriptStoreAccess { state: Arc<Mutex<State>> }

impl Drop for ScriptStoreAccess {
    fn drop(&mut self) {
        self.release();
        let state = lock(&self.state);
        if state.sealed.is_some() {
            if let Some(recovery) = state.recovery.upgrade() { lock(&recovery).insert(state.id.clone(), self.state.clone()); }
        }
    }
}

impl ScriptStoreAccess {
    pub(crate) fn open(store: &PersistentStore, target: Target, expected_revision: i64, owner: OwnerPermit,
        exposure: Exposure, pool: &AccessPool) -> Result<Self> {
        if matches!(exposure, Exposure::Window { start, length } if start < 0 || length < 0 || start.checked_add(length).is_none()) {
            return Err(Error::Limit("exposure"));
        }
        let _admission = owner.admit()?;
        let view = store.script_read_view(&target).map_err(storage_error)?;
        if view.revision != expected_revision { return Err(Error::Conflict); }
        let existence = view.existence.clone();
        view.close().map_err(storage_error)?;
        let id = uuid::Uuid::new_v4().to_string();
        let deadline = (pool.0.clock)().checked_add(pool.0.limits.lifetime).ok_or(Error::Limit("lifetime"))?;
        let state = Arc::new(Mutex::new(State {
            id: id.clone(), target, existence, owner: owner.clone(), exposure, deadline,
            charge: Charge { pool: pool.0.clone(), scope: Arc::new(Mutex::new(0)), bytes: 0 },
            recovery: Arc::downgrade(&pool.1), closed: None, failure: None,
            defaults: None, variables: Vec::new(), first_read: None, staged: None,
            invocation_sequence: 0, wait_sequence: 0, invocation: None, segment_sequence: 0,
            sealed: None, completed: VecDeque::new(),
        }));
        lock(&state).account()?;
        let mut usage = lock(&pool.0.usage);
        usage.scopes.retain(|_, scope| scope.strong_count() != 0);
        if usage.scopes.len() >= pool.0.limits.active_scopes { drop(usage); return Err(Error::Limit("active-scopes")); }
        usage.scopes.insert(id, Arc::downgrade(&state));
        drop(usage);
        pool.0.wake.notify_all();
        Ok(Self { state })
    }

    pub(crate) fn begin_invocation(&self, already_aborted: bool) -> Result<InvocationPermit> {
        let mut state = lock(&self.state);
        state.check_open()?;
        if already_aborted { return Err(Error::InvocationInvalid); }
        if state.invocation.is_some() { return Err(Error::InvocationInvalid); }
        if state.sealed.is_some() { return Err(Error::PendingCommit); }
        state.invocation_sequence = state.invocation_sequence.checked_add(1).ok_or(Error::Limit("invocations"))?;
        let sequence = state.invocation_sequence;
        state.invocation = Some(Invocation { sequence, aborted: false, waits: BTreeSet::new() });
        Ok(InvocationPermit { scope: state.id.clone(), sequence })
    }

    pub(crate) fn abort_invocation(&self, permit: &InvocationPermit) -> Result<()> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        state.invocation.as_mut().unwrap().aborted = true;
        Ok(())
    }

    pub(crate) fn end_invocation(&self, permit: &InvocationPermit) -> Result<bool> {
        let mut state = lock(&self.state);
        if permit.scope != state.id || state.invocation.as_ref().map(|invocation| invocation.sequence) != Some(permit.sequence) { return Err(Error::InvocationInvalid); }
        let aborted = state.invocation.take().unwrap().aborted;
        state.account()?;
        Ok(aborted)
    }

    pub(crate) fn begin_host_wait(&self, permit: &InvocationPermit) -> Result<WaitPermit> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        if state.invocation.as_ref().unwrap().waits.len() >= state.charge.pool.limits.waits { return Err(Error::Limit("waits")); }
        state.wait_sequence = state.wait_sequence.checked_add(1).ok_or(Error::Limit("waits"))?;
        let sequence = state.wait_sequence;
        state.invocation.as_mut().unwrap().waits.insert(sequence);
        if let Err(error) = state.account() { state.invocation.as_mut().unwrap().waits.remove(&sequence); return Err(error); }
        Ok(WaitPermit { invocation: permit.clone(), sequence })
    }

    pub(crate) fn accept_host_result(&self, permit: &WaitPermit) -> Result<InvocationPermit> {
        let mut state = lock(&self.state);
        state.check_invocation(&permit.invocation)?;
        if !state.invocation.as_mut().unwrap().waits.remove(&permit.sequence) { return Err(Error::WaitInvalid); }
        let owner = state.owner.clone();
        let result = owner.admit().map(|_| permit.invocation.clone());
        state.account()?;
        state.remember(result)
    }

    /// Non-string stored values stay JSON so each engine adapter supplies its own coercion.
    pub(crate) fn read_variable(&self, store: &PersistentStore, permit: &InvocationPermit, key: &str) -> Result<ReadResult<Value>> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        if let Some((_, value)) = state.variables.iter().find(|(candidate, _)| candidate == key) {
            let value = Value::String(value.clone());
            let bytes = value_bytes(&value);
            return state.output(value, bytes);
        }
        let view = state.live(store)?;
        if state.defaults.is_none() {
            let defaults = view.defaults(&state.target).map_err(storage_error)?;
            state.defaults = Some(defaults);
            if let Err(error) = state.account() { state.defaults = None; return Err(error); }
        }
        let value = store_access::stored_variable(&view.metadata, key).filter(|value| !value.is_null())
            .unwrap_or_else(|| Value::String(state.defaults.as_ref().unwrap().iter().find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone()).unwrap_or_else(|| "null".into())));
        view.close().map_err(storage_error)?;
        let bytes = value_bytes(&value);
        state.output(value, bytes)
    }

    pub(crate) fn set_variable(&self, store: &PersistentStore, permit: &InvocationPermit, key: String, value: String) -> Result<bool> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        let index = state.variables.iter().position(|(candidate, _)| candidate == &key);
        let unchanged = if let Some(index) = index { state.variables[index].1 == value } else {
            let view = state.live(store)?;
            let unchanged = store_access::stored_variable(&view.metadata, &key).as_ref() == Some(&Value::String(value.clone()));
            view.close().map_err(storage_error)?;
            unchanged
        };
        if unchanged { return Ok(false); }
        let mut next = state.variables.clone();
        if let Some(index) = index { next[index].1 = value; } else { next.push((key, value)); }
        if next.len() > state.charge.pool.limits.variable_count || variables_bytes(&next) > state.charge.pool.limits.variable_bytes { return Err(Error::Limit("variables")); }
        let previous = std::mem::replace(&mut state.variables, next);
        if let Err(error) = state.account() { state.variables = previous; return Err(error); }
        Ok(true)
    }

    pub(crate) fn history_length(&self, store: &PersistentStore, permit: &InvocationPermit) -> Result<i64> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        if let Some(staged) = &state.staged { return Ok(staged.count()); }
        let view = state.live(store)?;
        state.record_read(&view)?;
        let count = view.messages.count;
        view.close().map_err(storage_error)?;
        Ok(count)
    }

    pub(crate) fn read_message(&self, store: &PersistentStore, permit: &InvocationPermit, index: f64) -> Result<ReadResult<Option<Value>>> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        let live = if state.staged.is_none() { let view = state.live(store)?; state.record_read(&view)?; Some(view) } else { None };
        let view = live.as_ref().unwrap_or_else(|| &state.staged.as_ref().unwrap().view);
        let position = state.exposure.position(state.staged.as_ref().map_or(view.messages.count, Staged::count), index);
        let value = position.map(|index| history_rows(view, state.staged.as_ref(), &state.target, index, 1).map(|mut rows| rows.remove(0)))
            .transpose()?;
        let bytes = value.as_ref().map_or(size_of::<Option<Value>>(), value_bytes);
        if bytes > state.charge.pool.limits.read_page_bytes { return Err(Error::Limit("read-page-bytes")); }
        if let Some(live) = live { live.close().map_err(storage_error)?; }
        state.output(value, bytes)
    }

    pub(crate) fn read_history(&self, store: &PersistentStore, permit: &InvocationPermit,
        selection: HistorySelection) -> Result<ReadResult<HistoryRows>> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        let live = if state.staged.is_none() { let view = state.live(store)?; state.record_read(&view)?; Some(view) } else { None };
        let view = live.as_ref().unwrap_or_else(|| &state.staged.as_ref().unwrap().view);
        let result = (|| {
            let total = state.staged.as_ref().map_or(view.messages.count, Staged::count);
            let (first, last) = state.exposure.range(total);
            let (start, end, single) = match selection {
                HistorySelection::One(NumberArgument::Invalid) | HistorySelection::Recent(NumberArgument::Invalid) => return Err(Error::HistoryCoercion),
                HistorySelection::One(NumberArgument::Value(index)) => {
                    let position = state.exposure.position(total, index);
                    (position.unwrap_or(first), position.map_or(first, |position| position + 1), true)
                }
                HistorySelection::Recent(NumberArgument::Value(count)) => {
                    // Count already contains JS max(0, floor(count || 0)); NaN reaches slice as zero.
                    let start = if count.is_nan() || count >= (last - first) as f64 { first }
                        else if count <= 0.0 { last }
                        else { last - count as i64 };
                    (start, last, false)
                }
                HistorySelection::FullExposed => (first, last, false),
            };
            let mut rows = Vec::new();
            let mut bytes = size_of::<HistoryRows>();
            let mut charge = Charge { pool: state.charge.pool.clone(), scope: state.charge.scope.clone(), bytes: 0 };
            charge.resize(bytes)?;
            for offset in (start..end).step_by(state.charge.pool.limits.read_page_rows) {
                let size = (end - offset).min(state.charge.pool.limits.read_page_rows as i64);
                let page = history_rows(view, state.staged.as_ref(), &state.target, offset, size)?;
                let page_bytes = page.iter().map(value_bytes).sum::<usize>();
                if page_bytes > state.charge.pool.limits.read_page_bytes { return Err(Error::Limit("read-page-bytes")); }
                bytes = bytes.checked_add(page_bytes).ok_or(Error::Limit("result-bytes"))?;
                if bytes > state.charge.pool.limits.result_bytes { return Err(Error::Limit("result-bytes")); }
                charge.resize(bytes)?;
                rows.try_reserve_exact(page.len()).map_err(|_| Error::Limit("history-allocation"))?;
                rows.extend(page);
                let retained = bytes.saturating_add((rows.capacity() - rows.len()) * size_of::<Value>());
                if retained > state.charge.pool.limits.result_bytes { return Err(Error::Limit("result-bytes")); }
                charge.resize(retained)?;
            }
            let value = if single { HistoryRows::One(rows.pop()) } else { HistoryRows::Many(rows) };
            Ok(ReadResult { value, _charge: charge })
        })();
        if let Some(live) = live { live.close().map_err(storage_error)?; }
        result
    }

    pub(crate) fn read_messages(&self, store: &PersistentStore, permit: &InvocationPermit, start: i64, count: usize) -> Result<ReadResult<Vec<Value>>> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        let live = if state.staged.is_none() { let view = state.live(store)?; state.record_read(&view)?; Some(view) } else { None };
        let view = live.as_ref().unwrap_or_else(|| &state.staged.as_ref().unwrap().view);
        let (first, last) = state.exposure.range(state.staged.as_ref().map_or(view.messages.count, Staged::count));
        let count = i64::try_from(count).map_err(|_| Error::Limit("history-range"))?;
        let end = start.checked_add(count).ok_or(Error::Limit("history-range"))?;
        if start < first || end > last { return Err(Error::Limit("history-range")); }
        let mut result = Vec::new();
        let mut bytes = size_of::<Vec<Value>>();
        let mut charge = Charge { pool: state.charge.pool.clone(), scope: state.charge.scope.clone(), bytes: 0 };
        for offset in (start..end).step_by(state.charge.pool.limits.read_page_rows) {
            let size = (end - offset).min(state.charge.pool.limits.read_page_rows as i64);
            let page = history_rows(view, state.staged.as_ref(), &state.target, offset, size)?;
            let page_bytes = page.iter().map(value_bytes).sum::<usize>();
            if page_bytes > state.charge.pool.limits.read_page_bytes { return Err(Error::Limit("read-page-bytes")); }
            bytes = bytes.checked_add(page_bytes).ok_or(Error::Limit("result-bytes"))?;
            if bytes > state.charge.pool.limits.result_bytes { return Err(Error::Limit("result-bytes")); }
            charge.resize(bytes)?;
            result.extend(page);
            let retained = bytes + (result.capacity() - result.len()) * size_of::<Value>();
            if retained > state.charge.pool.limits.result_bytes { return Err(Error::Limit("result-bytes")); }
            charge.resize(retained)?;
        }
        if let Some(live) = live { live.close().map_err(storage_error)?; }
        if bytes > state.charge.pool.limits.result_bytes { return Err(Error::Limit("result-bytes")); }
        charge.resize(bytes + (result.capacity() - result.len()) * size_of::<Value>())?;
        Ok(ReadResult { value: result, _charge: charge })
    }

    pub(crate) fn set_message_data(&self, store: &PersistentStore, permit: &InvocationPermit, index: f64, data: String) -> Result<()> {
        self.edit_message(store, permit, index, "data", data)
    }

    pub(crate) fn set_message_role(&self, store: &PersistentStore, permit: &InvocationPermit, index: f64, role: &str) -> Result<()> {
        self.edit_message(store, permit, index, "role", if role == "user" { "user" } else { "char" }.into())
    }

    fn edit_message(&self, store: &PersistentStore, permit: &InvocationPermit, index: f64, field: &str, value: String) -> Result<()> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        state.stage(store)?;
        let staged = state.staged.as_ref().unwrap();
        let Some(index) = state.exposure.position(staged.count(), index) else { return Ok(()); };
        let (mut edits, mut layout) = (staged.edits.clone(), staged.layout.clone());
        let stored = match layout.as_mut() {
            None => Some(index),
            Some(layout) => match locate(layout, index) {
                Some((piece, offset)) => match &mut layout[piece] {
                    Piece::Stored { start, .. } => Some(*start + offset),
                    Piece::New(message) => {
                        store_access::object_mut(message).map_err(storage_error)?.insert(field.into(), Value::String(value.clone()));
                        None
                    }
                },
                None => return Ok(()),
            },
        };
        if let Some(index) = stored {
            let mut message = if let Some(value) = staged.edits.get(&index) { value.clone() } else {
                staged.view.read(&state.target, index, 1).map_err(storage_error)?.remove(0)
            };
            store_access::object_mut(&mut message).map_err(storage_error)?.insert(field.into(), Value::String(value));
            edits.insert(index, message);
        }
        state.restage(edits, layout)
    }

    /// `message.splice(index, 1)`.
    pub(crate) fn remove_message(&self, store: &PersistentStore, permit: &InvocationPermit, index: f64) -> Result<()> {
        self.restructure(store, permit, |_, edits, layout| {
            let Some((piece, offset)) = locate(layout, relative_index(index, layout_len(layout) as usize) as i64) else { return Ok(()) };
            let stored = match layout[piece] { Piece::Stored { start, count } => Some((start, count)), Piece::New(_) => None };
            let Some((start, count)) = stored else { layout.remove(piece); return Ok(()) };
            edits.remove(&(start + offset));
            let rest = [Piece::Stored { start, count: offset }, Piece::Stored { start: start + offset + 1, count: count - offset - 1 }];
            layout.splice(piece..=piece, rest.into_iter().filter(|piece| piece.len() > 0));
            Ok(())
        })
    }

    /// `message.splice(index, 0, message)`.
    pub(crate) fn insert_message(&self, store: &PersistentStore, permit: &InvocationPermit, index: f64, message: Value) -> Result<()> {
        self.restructure(store, permit, |_, _, layout| {
            match locate(layout, relative_index(index, layout_len(layout) as usize) as i64) {
                None => layout.push(Piece::New(message)),
                Some((piece, 0)) => layout.insert(piece, Piece::New(message)),
                Some((piece, offset)) => {
                    let Piece::Stored { start, count } = layout[piece] else { unreachable!("a new row is one row long") };
                    layout.splice(piece..=piece, [Piece::Stored { start, count: offset }, Piece::New(message),
                        Piece::Stored { start: start + offset, count: count - offset }]);
                }
            }
            Ok(())
        })
    }

    /// `setFullChatMain`'s assignment after its `chatId` merge: a stored id is kept for as many messages as the history
    /// holds it, and a message keeps its stored fields when its id names one message on both sides. Returns the rows.
    pub(crate) fn replace_messages(&self, store: &PersistentStore, permit: &InvocationPermit, messages: Vec<FullMessage>) -> Result<Vec<Value>> {
        self.restructure(store, permit, |state, edits, layout| {
            let staged = state.staged.as_ref().unwrap();
            let mut counts = HashMap::<&str, usize>::new();
            for id in messages.iter().filter_map(|message| message.chat_id.as_deref()) { *counts.entry(id).or_default() += 1; }
            // For each id the input names: how often the history holds it, and its message while it holds it once.
            let mut originals = HashMap::<&str, (usize, Option<Map<String, Value>>)>::new();
            let (total, page) = (staged.count(), state.charge.pool.limits.read_page_rows);
            for offset in (0..total).step_by(page) {
                for row in history_rows(&staged.view, Some(staged), &state.target, offset, (page as i64).min(total - offset))? {
                    let Value::Object(row) = row else { continue };
                    let Some(Value::String(id)) = row.get("chatId") else { continue };
                    let Some((&id, _)) = counts.get_key_value(id.as_str()).filter(|(id, _)| !id.is_empty()) else { continue };
                    originals.entry(id).and_modify(|entry| *entry = (entry.0 + 1, None)).or_insert((1, Some(row)));
                }
            }
            let mut kept = HashMap::<&str, usize>::new();
            let mut rows = Vec::new();
            for message in &messages {
                let held = message.chat_id.as_deref().and_then(|id| originals.get(id).map(|entry| (id, entry)))
                    .filter(|(id, (count, _))| kept.get(id).copied().unwrap_or(0) < *count);
                let (mut row, id) = match held {
                    Some((id, (_, original))) => {
                        *kept.entry(id).or_default() += 1;
                        (original.as_ref().filter(|_| counts[id] == 1).cloned().unwrap_or_default(), id.to_owned())
                    }
                    None => (Map::new(), uuid::Uuid::new_v4().to_string()),
                };
                row.insert("role".into(), Value::String(message.role.clone()));
                row.insert("data".into(), Value::String(message.data.clone()));
                row.insert("chatId".into(), Value::String(id));
                rows.push(Value::Object(row));
            }
            *layout = rows.iter().cloned().map(Piece::New).collect();
            edits.clear();
            Ok(rows)
        })
    }

    /// A change of the staged rows, made on copies of the edits and layout that replace them once they fit.
    fn restructure<T>(&self, store: &PersistentStore, permit: &InvocationPermit,
        change: impl FnOnce(&State, &mut BTreeMap<i64, Value>, &mut Vec<Piece>) -> Result<T>) -> Result<T> {
        let mut state = lock(&self.state);
        state.check_invocation(permit)?;
        // A windowed history (`toHistorySpliceIndex`) is not modeled.
        if state.exposure != Exposure::Full { return Err(Error::Limit("history-structure")); }
        state.stage(store)?;
        let staged = state.staged.as_ref().unwrap();
        let mut edits = staged.edits.clone();
        let count = staged.view.messages.count;
        let mut layout = staged.layout.clone().unwrap_or_else(|| if count > 0 { vec![Piece::Stored { start: 0, count }] } else { Vec::new() });
        let result = change(&*state, &mut edits, &mut layout)?;
        state.restage(edits, Some(layout))?;
        Ok(result)
    }

    pub(crate) fn next_segment(&self) -> Result<SegmentId> {
        let mut state = lock(&self.state);
        state.check_open()?;
        if state.sealed.is_some() { return Err(Error::PendingCommit); }
        Ok(SegmentId { scope: state.id.clone(), sequence: state.segment_sequence.checked_add(1).ok_or(Error::Limit("segments"))? })
    }

    pub(crate) fn flush_variables(&self, store: &mut PersistentStore, id: &SegmentId) -> Result<SegmentOutcome> {
        self.commit(store, id, FinishMode::Variables)
    }

    pub(crate) fn commit(&self, store: &mut PersistentStore, id: &SegmentId, mode: FinishMode) -> Result<SegmentOutcome> {
        let mut state = lock(&self.state);
        if id.scope != state.id { return Err(Error::SegmentIntegrity); }
        if let Some(completed) = state.completed.iter().find(|completed| &completed.outcome.id == id) {
            return if completed.mode == mode { Ok(completed.outcome.clone()) } else { Err(Error::SegmentIntegrity) };
        }
        if let Some(sealed) = &state.sealed {
            if &sealed.id != id || sealed.mode != mode { return Err(Error::SegmentIntegrity); }
        } else {
            state.check_open()?;
            if id.sequence <= state.segment_sequence { return Err(Error::SegmentExpired); }
            if state.segment_sequence.checked_add(1) != Some(id.sequence) { return Err(Error::SegmentIntegrity); }
            let variables = state.variables.clone();
            let (messages, edits, splices) = if mode == FinishMode::Variables || mode == FinishMode::WithoutMessages {
                (None, BTreeMap::new(), Vec::new())
            } else {
                state.staged.as_ref().map_or((None, BTreeMap::new(), Vec::new()), |staged| (Some(staged.view.messages.clone()), staged.edits.clone(),
                    staged.layout.as_deref().map_or_else(Vec::new, |layout| splices(layout, staged.view.messages.count))))
            };
            let primary_error = if mode == FinishMode::Variables { None } else { state.failure.clone() };
            let digest = risunest_sync_wire::hash(&serde_json::to_vec(&(mode, &variables, &messages, &edits, &splices)).map_err(|_| Error::SegmentIntegrity)?);
            state.sealed = Some(Sealed { id: id.clone(), mode, digest, variables, edits, splices, messages, primary_error,
                attempt: None, changed_keys: Vec::new(), messages_changed: false });
            if let Err(error) = state.account() { state.sealed = None; return Err(error); }
            state.variables = Vec::new();
            if mode != FinishMode::Variables { state.staged = None; state.first_read = None; state.failure = None; }
            state.segment_sequence = id.sequence;
            state.account()?;
        }
        drive(&mut state, store)
    }

    pub(crate) fn recover_segment(&self, store: &mut PersistentStore, id: &SegmentId) -> Result<SegmentOutcome> {
        let mut state = lock(&self.state);
        if id.scope != state.id { return Err(Error::SegmentIntegrity); }
        if let Some(completed) = state.completed.iter().find(|completed| &completed.outcome.id == id) { return Ok(completed.outcome.clone()); }
        if state.sealed.as_ref().is_none_or(|sealed| &sealed.id != id) { return Err(Error::SegmentExpired); }
        drive(&mut state, store)
    }

    pub(crate) fn release(&self) {
        let mut state = lock(&self.state);
        state.closed = Some(Error::Closed);
        state.invocation = None;
        state.staged = None;
        state.first_read = None;
        state.variables = Vec::new();
        state.defaults = None;
        let _ = state.account();
    }
}

fn finish(state: &mut State, status: SegmentStatus, error: Option<Error>) -> Result<SegmentOutcome> {
    let sealed = state.sealed.as_ref().ok_or(Error::SegmentExpired)?;
    let committed = matches!(status, SegmentStatus::Committed(_));
    let outcome = SegmentOutcome {
        id: sealed.id.clone(), digest: sealed.digest.clone(), status,
        changed_keys: if committed { sealed.changed_keys.clone() } else { Vec::new() },
        messages_changed: committed && sealed.messages_changed, error: sealed.primary_error.clone().or(error),
    };
    state.completed.push_back(Completed { mode: sealed.mode, outcome: outcome.clone() });
    while !state.completed.is_empty() && (state.completed.len() > state.charge.pool.limits.receipts
        || state.completed.iter().map(receipt_bytes).sum::<usize>()
            + (state.completed.capacity() - state.completed.len()) * size_of::<Completed>() > state.charge.pool.limits.receipt_bytes) {
        state.completed.pop_front();
    }
    if state.completed.is_empty() { state.completed = VecDeque::new(); }
    // Optional receipts must fit before the sealed recovery record is removed.
    if state.account().is_err() {
        state.completed = VecDeque::new();
        state.account()?;
    }
    state.sealed = None;
    state.account().expect("removing a sealed segment only releases charged memory");
    Ok(outcome)
}

fn unresolved(state: &State, issue: RecoveryIssue) -> Result<SegmentOutcome> {
    let sealed = state.sealed.as_ref().ok_or(Error::SegmentExpired)?;
    Ok(SegmentOutcome { id: sealed.id.clone(), digest: sealed.digest.clone(), status: SegmentStatus::Unresolved(issue),
        changed_keys: Vec::new(), messages_changed: false, error: sealed.primary_error.clone() })
}

fn drive(state: &mut State, store: &mut PersistentStore) -> Result<SegmentOutcome> {
    for _ in 0..state.charge.pool.limits.commit_attempts {
        if let Some(attempt) = state.sealed.as_ref().and_then(|sealed| sealed.attempt.as_ref()) {
            match store.script_resolve(attempt) {
                Ok(Resolution::Committed(revision)) => return finish(state, SegmentStatus::Committed(revision), None),
                Ok(Resolution::Unresolved(issue)) => return unresolved(state, issue),
                Err(_) => return unresolved(state, RecoveryIssue::Failed),
                Ok(Resolution::NotCommitted) => { state.sealed.as_mut().unwrap().attempt = None; }
            }
        }
        if let Err(error) = state.check_open() { return finish(state, SegmentStatus::NotCommitted, Some(error)); }
        let sealed = state.sealed.as_ref().ok_or(Error::SegmentExpired)?;
        if matches!(sealed.mode, FinishMode::Variables | FinishMode::IfChanged)
            && sealed.variables.is_empty() && sealed.messages.is_none() && sealed.edits.is_empty() && sealed.splices.is_empty()
            && sealed.primary_error.is_none() {
            return finish(state, SegmentStatus::NoChange, None);
        }
        let owner = state.owner.clone();
        let admission = match owner.admit() {
            Ok(admission) => admission,
            Err(error) => return finish(state, SegmentStatus::NotCommitted, Some(error)),
        };
        let sealed = state.sealed.as_mut().unwrap();
        if sealed.primary_error.is_some() { sealed.edits.clear(); sealed.splices.clear(); sealed.messages = None; }
        let prepared = store.script_prepare(&state.target, &state.existence, sealed.messages.as_ref(), &sealed.edits, &sealed.splices, &sealed.variables);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) if storage_error_code(&error) == Error::StaleMessages => {
                sealed.primary_error.get_or_insert(Error::StaleMessages);
                sealed.messages = None;
                sealed.edits.clear();
                sealed.splices.clear();
                match store.script_prepare(&state.target, &state.existence, None, &sealed.edits, &sealed.splices, &sealed.variables) {
                    Ok(prepared) => prepared,
                    Err(StoreError::RevisionConflict { .. }) => { drop(admission); continue; }
                    Err(error) => return finish(state, SegmentStatus::NotCommitted, Some(storage_error(error))),
                }
            }
            Err(StoreError::RevisionConflict { .. }) => { drop(admission); continue; }
            Err(error) => return finish(state, SegmentStatus::NotCommitted, Some(storage_error(error))),
        };
        if prepared.attempt.is_none() { return finish(state, SegmentStatus::NoChange, None); }
        sealed.changed_keys = prepared.changed_keys;
        sealed.messages_changed = prepared.messages_changed;
        sealed.attempt = prepared.attempt;
        if let Err(error) = state.account() {
            state.sealed.as_mut().unwrap().attempt = None;
            return finish(state, SegmentStatus::NotCommitted, Some(error));
        }
        let attempt = state.sealed.as_ref().unwrap().attempt.as_ref().unwrap();
        let result = store.script_issue(attempt);
        drop(admission);
        match result {
            Ok(Resolution::Committed(revision)) => return finish(state, SegmentStatus::Committed(revision), None),
            Ok(Resolution::Unresolved(issue)) => return unresolved(state, issue),
            Ok(Resolution::NotCommitted) | Err(StoreError::RevisionConflict { .. }) => {
                state.sealed.as_mut().unwrap().attempt = None;
            }
            Err(error) => match store.script_resolve(attempt) {
                Ok(Resolution::Committed(revision)) => return finish(state, SegmentStatus::Committed(revision), None),
                Ok(Resolution::NotCommitted) => return finish(state, SegmentStatus::NotCommitted, Some(storage_error(error))),
                Ok(Resolution::Unresolved(issue)) => return unresolved(state, issue),
                Err(_) => return unresolved(state, RecoveryIssue::Failed),
            },
        }
    }
    finish(state, SegmentStatus::NotCommitted, Some(Error::Conflict))
}

fn storage_error_code(error: &StoreError) -> Error {
    if matches!(error, StoreError::Validation { message } if message == "native-script:stale-messages") { Error::StaleMessages }
    else { Error::Storage(error.code().into_owned()) }
}
