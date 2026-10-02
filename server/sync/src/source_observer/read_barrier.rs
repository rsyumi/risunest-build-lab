use super::*;
use std::{
    future::Future,
    sync::{Condvar, Weak},
    task::Waker,
    time::{Duration, Instant},
};

pub(crate) const HOLD_LIMIT: Duration = Duration::from_secs(30);
type Hook = Arc<dyn Fn(Reached) + Send + Sync>;
static HOOK: OnceLock<Mutex<Option<Hook>>> = OnceLock::new();
pub(crate) fn install_hook(hook: Option<Hook>) {
    *HOOK.get_or_init(|| Mutex::new(None)).lock().unwrap() = hook;
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Intent {
    pub scope_id: String,
    pub generation: u64,
    pub barrier_id: String,
    pub hash: String,
    pub flow: String,
}
impl Intent {
    fn matches(&self, other: &Self) -> bool {
        self.scope_id == other.scope_id
            && self.generation == other.generation
            && self.barrier_id == other.barrier_id
            && self.hash == other.hash
            && self.flow == other.flow
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reached {
    #[serde(rename = "type")]
    event_type: &'static str,
    #[serde(flatten)]
    intent: Intent,
    phase: String,
    placement: String,
    read_kind: &'static str,
    offset: u64,
    elapsed_nanos: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Observation {
    #[serde(flatten)]
    intent: Intent,
    pub status: &'static str,
    phase: String,
    pub waiters: u64,
    reached: Option<Reached>,
    released_elapsed_nanos: Option<String>,
    cancellation_reason: Option<String>,
}
impl Observation {
    pub(super) fn complete(&self) -> bool {
        self.status == "released" && self.waiters == 0
    }
    pub(super) fn matches_reader(&self, hash: &str, flow: &str) -> bool {
        self.intent.hash == hash && self.intent.flow == flow
    }
}
struct State {
    status: &'static str,
    waiters: u64,
    reached: Option<Reached>,
    deadline: Option<Instant>,
    released_elapsed_nanos: Option<String>,
    cancellation_reason: Option<String>,
    wakers: Vec<Waker>,
}
pub(super) struct Barrier {
    owner: Weak<Mutex<Scope>>,
    intent: Intent,
    phase: String,
    started: Instant,
    state: Mutex<State>,
    changed: Condvar,
}
impl Barrier {
    pub(super) fn observation(&self) -> Observation {
        let state = self.state.lock().unwrap();
        Observation {
            intent: self.intent.clone(),
            status: state.status,
            phase: self.phase.clone(),
            waiters: state.waiters,
            reached: state.reached.clone(),
            released_elapsed_nanos: state.released_elapsed_nanos.clone(),
            cancellation_reason: state.cancellation_reason.clone(),
        }
    }
    fn violated(&self, reason: &str) {
        if let Some(owner) = self.owner.upgrade() {
            violation(&mut owner.lock().unwrap(), reason);
        }
    }
    fn cancel(&self, reason: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.status == "released" || state.status == "cancelled" {
            return false;
        }
        state.status = "cancelled";
        state.cancellation_reason = Some(reason.into());
        let wakers = std::mem::take(&mut state.wakers);
        drop(state);
        self.violated(reason);
        self.changed.notify_all();
        for waker in wakers {
            waker.wake();
        }
        true
    }
    pub(super) fn enter(
        self: &Arc<Self>,
        placement: &str,
        kind: &'static str,
        offset: u64,
    ) -> Option<Wait> {
        let mut state = self.state.lock().unwrap();
        if state.status == "released" {
            return None;
        }
        let event = if state.status == "armed" {
            let event = Reached {
                event_type: "readBarrierReached",
                intent: self.intent.clone(),
                phase: self.phase.clone(),
                placement: placement.into(),
                read_kind: kind,
                offset,
                elapsed_nanos: self.started.elapsed().as_nanos().to_string(),
            };
            state.status = "reached";
            state.reached = Some(event.clone());
            state.deadline = Some(Instant::now() + HOLD_LIMIT);
            Some(event)
        } else {
            None
        };
        state.waiters = state
            .waiters
            .checked_add(1)
            .expect("read barrier waiter overflow");
        drop(state);
        if let Some(event) = event {
            let hook = HOOK
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap()
                .clone();
            if let Some(hook) = hook {
                hook(event);
            } else {
                self.cancel("read-barrier-missing-event-hook");
            }
        }
        Some(Wait {
            barrier: self.clone(),
            finished: false,
            timer: None,
        })
    }
}
pub(crate) fn arm(intent: Intent) -> Result<Observation, &'static str> {
    let mut registry = registry().lock().unwrap();
    let context = registry.active.clone().ok_or("read-barrier-no-scope")?;
    let mut scope = context.scope.lock().unwrap();
    if scope.closed || scope.id != intent.scope_id || scope.generation != intent.generation {
        return Err("read-barrier-stale-scope");
    }
    if intent.barrier_id.is_empty()
        || intent.barrier_id.len() > 128
        || intent.flow != "source"
        || risunest_sync_wire::validate_hash(&intent.hash).is_err()
        || !scope
            .roles
            .get(&intent.hash)
            .is_some_and(|p| p.contains("Asset"))
    {
        return Err("read-barrier-invalid-intent");
    }
    if scope.read_barrier.is_some() || registry.used_read_barriers.contains(&intent.barrier_id) {
        return Err("read-barrier-reused");
    }
    if scope
        .started_reads
        .contains(&(intent.hash.clone(), intent.flow.clone()))
    {
        return Err("read-barrier-already-read");
    }
    registry
        .used_read_barriers
        .insert(intent.barrier_id.clone());
    let barrier = Arc::new(Barrier {
        owner: Arc::downgrade(&context.scope),
        intent,
        phase: scope.phase.clone(),
        started: scope.started,
        state: Mutex::new(State {
            status: "armed",
            waiters: 0,
            reached: None,
            deadline: None,
            released_elapsed_nanos: None,
            cancellation_reason: None,
            wakers: Vec::new(),
        }),
        changed: Condvar::new(),
    });
    let observation = barrier.observation();
    scope.read_barrier = Some(barrier);
    Ok(observation)
}
fn matching(intent: &Intent) -> Result<Arc<Barrier>, &'static str> {
    let context = active().ok_or("read-barrier-no-scope")?;
    let scope = context.scope.lock().unwrap();
    if scope.closed || scope.id != intent.scope_id || scope.generation != intent.generation {
        return Err("read-barrier-stale-scope");
    }
    let barrier = scope.read_barrier.clone().ok_or("read-barrier-not-armed")?;
    if !barrier.intent.matches(intent) {
        return Err("read-barrier-identity-mismatch");
    }
    Ok(barrier)
}
pub(crate) fn release(intent: &Intent) -> Result<Observation, &'static str> {
    let barrier = matching(intent)?;
    let mut state = barrier.state.lock().unwrap();
    if state.status != "reached" {
        return Err("read-barrier-not-reached");
    }
    if state
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        drop(state);
        barrier.cancel("read-barrier-timeout");
        return Err("read-barrier-timeout");
    }
    state.status = "released";
    state.released_elapsed_nanos = Some(barrier.started.elapsed().as_nanos().to_string());
    let wakers = std::mem::take(&mut state.wakers);
    drop(state);
    barrier.changed.notify_all();
    for waker in wakers {
        waker.wake();
    }
    Ok(barrier.observation())
}
pub(crate) fn cancel(intent: &Intent) -> Result<Observation, &'static str> {
    let barrier = matching(intent)?;
    if !barrier.cancel("read-barrier-cancelled") {
        return Err("read-barrier-terminal");
    }
    Ok(barrier.observation())
}
pub(crate) fn cancel_dangling(reason: &str) {
    let barrier = active().and_then(|c| c.scope.lock().unwrap().read_barrier.clone());
    if let Some(barrier) = barrier {
        barrier.cancel(reason);
    }
}
pub(super) struct Wait {
    barrier: Arc<Barrier>,
    finished: bool,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}
impl Wait {
    fn finish(&mut self) {
        if !self.finished {
            self.barrier.state.lock().unwrap().waiters -= 1;
            self.finished = true;
        }
    }
    fn result(&mut self) -> Option<io::Result<()>> {
        let status = self.barrier.state.lock().unwrap().status;
        match status {
            "released" => {
                self.finish();
                Some(Ok(()))
            }
            "cancelled" => {
                self.finish();
                Some(Err(io::Error::other("source read barrier cancelled")))
            }
            _ => None,
        }
    }
    pub(super) fn blocking(mut self) -> io::Result<()> {
        loop {
            if let Some(result) = self.result() {
                return result;
            }
            let state = self.barrier.state.lock().unwrap();
            if state.status != "reached" {
                continue;
            }
            let left = state
                .deadline
                .unwrap()
                .saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(state);
                self.barrier.cancel("read-barrier-timeout");
                continue;
            }
            drop(self.barrier.changed.wait_timeout(state, left).unwrap());
        }
    }
    pub(super) fn poll(&mut self, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        if let Some(result) = self.result() {
            return Poll::Ready(result);
        }
        let mut state = self.barrier.state.lock().unwrap();
        if state.status != "reached" {
            drop(state);
            return Poll::Ready(self.result().unwrap());
        }
        if !state.wakers.iter().any(|w| w.will_wake(cx.waker())) {
            state.wakers.push(cx.waker().clone());
        }
        let left = state
            .deadline
            .unwrap()
            .saturating_duration_since(Instant::now());
        drop(state);
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(left)));
        if timer.as_mut().poll(cx).is_ready() {
            self.barrier.cancel("read-barrier-timeout");
            Poll::Ready(self.result().unwrap())
        } else {
            Poll::Pending
        }
    }
}
impl Drop for Wait {
    fn drop(&mut self) {
        if !self.finished {
            self.barrier.cancel("read-barrier-reader-dropped");
            self.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_release_cancels_actual_waiter_without_resuming_body_read() {
        let _reset = super::super::tests::Reset::new();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("body");
        let body = b"synthetic deadline body";
        std::fs::write(&path, body).unwrap();
        let hash = risunest_sync_wire::hash(body);
        let generation = begin(
            root.path(),
            "deadline".into(),
            "source".into(),
            vec![Role {
                hash: hash.clone(),
                purposes: ["Asset".into()].into_iter().collect(),
            }],
        )
        .unwrap();
        let intent = Intent {
            scope_id: "deadline".into(),
            generation,
            barrier_id: "deadline-first-read".into(),
            hash: hash.clone(),
            flow: "source".into(),
        };
        let (send, receive) = std::sync::mpsc::channel();
        let reader_held = Arc::new(std::sync::Barrier::new(2));
        let release_reader = reader_held.clone();
        install_hook(Some(Arc::new(move |event| {
            send.send(event).unwrap();
            release_reader.wait();
        })));
        arm(intent.clone()).unwrap();
        let context = active().unwrap();
        let barrier = context.scope.lock().unwrap().read_barrier.clone().unwrap();
        let worker = worker();
        let owned_root = root.path().to_owned();
        let task = std::thread::spawn(move || {
            let _entered = worker.enter();
            let mut file = TrackedFile::open(&owned_root, &hash, &path, "file").unwrap();
            file.read(&mut [0; 1])
        });
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        barrier.state.lock().unwrap().deadline = Some(Instant::now());
        let released = release(&intent);
        reader_held.wait();
        let read = task.join().unwrap();
        assert!(matches!(released, Err("read-barrier-timeout")));
        assert!(read.is_err());
        let observed = snapshot(true).unwrap();
        assert!(!observed.complete);
        assert_eq!(observed.total.read_bytes, 0);
        assert_eq!(observed.read_barrier.unwrap().status, "cancelled");
    }
}
