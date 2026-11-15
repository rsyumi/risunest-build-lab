use super::controller::{Actor, Gate, Kind, ResultKind, Session};
use crate::native_generation::{
    ollama_transport::{ConsumerStop, OfferResult, OutputSink, Snapshot},
    operation::Phase,
    owner::{OwnerExecution, OwnerStore},
};
use crate::persistent_store::{generation, lww, PersistentStore, StoreResult};
use async_trait::async_trait;
use std::{future::Future, marker::PhantomData, rc::Rc, sync::Arc};

tokio::task_local! { static CURRENT: Arc<Session>; }

pub(crate) fn current() -> Option<Arc<Session>> { CURRENT.try_with(Arc::clone).ok() }
pub(crate) fn observe(kind: Kind) { if let Some(session) = current() { session.record(kind, None); } }

pub(crate) async fn owner_scope(actor: Option<Actor>, work: impl Future<Output = OwnerExecution>) -> OwnerExecution {
    let Some(actor) = actor else { return work.await };
    let result = CURRENT.scope(actor.session.clone(), work).await;
    let phase = match &result.result {
        Ok(status) => match status.phase {
            Phase::Completed => ResultKind::Completed,
            Phase::Cancelled => ResultKind::Cancelled,
            Phase::Failed => ResultKind::Failed,
            Phase::Stale => ResultKind::Stale,
            Phase::Interrupted => ResultKind::Interrupted,
            Phase::Reserved | Phase::Running => ResultKind::Running,
        },
        Err(_) => ResultKind::Error,
    };
    actor.session.record(Kind::OwnerCompleted, Some(phase));
    result
}

struct Call {
    session: Option<Arc<Session>>,
    dropped: Kind,
    returned: bool,
}
impl Call {
    fn new(session: Option<Arc<Session>>, entered: Kind, dropped: Kind) -> Self {
        if let Some(session) = &session { session.record(entered, None); }
        Self { session, dropped, returned: false }
    }
    fn record(&self, kind: Kind, result: Option<ResultKind>) {
        if let Some(session) = &self.session { session.record(kind, result); }
    }
    fn returned(&mut self, kind: Kind, result: ResultKind) {
        self.returned = true;
        self.record(kind, Some(result));
    }
}
impl Drop for Call {
    fn drop(&mut self) { if !self.returned { self.record(self.dropped, None); } }
}

pub(crate) struct ObservedSink<'a> { inner: &'a mut dyn OutputSink, session: Option<Arc<Session>> }
impl<'a> ObservedSink<'a> {
    pub fn new(inner: &'a mut dyn OutputSink) -> Self { Self { inner, session: current() } }
}
#[async_trait]
impl OutputSink for ObservedSink<'_> {
    async fn credit(&mut self) -> Result<(), ConsumerStop> {
        let mut call = Call::new(self.session.clone(), Kind::CreditEntered, Kind::CreditDropped);
        if let Some(session) = &self.session { session.wait(Gate::Credit).await; }
        call.record(Kind::CreditDelegated, None);
        let result = self.inner.credit().await;
        call.returned(Kind::CreditForwarded, match result {
            Ok(()) => ResultKind::Ok,
            Err(ConsumerStop::Cancelled) => ResultKind::Cancelled,
            Err(ConsumerStop::Failed) => ResultKind::Failed,
        });
        result
    }
    async fn offer(&mut self, snapshot: Arc<Snapshot>) -> OfferResult {
        let mut call = Call::new(self.session.clone(), Kind::OfferEntered, Kind::OfferDropped);
        if let Some(session) = &self.session { session.wait(Gate::Offer).await; }
        call.record(Kind::OfferDelegated, None);
        let result = self.inner.offer(snapshot).await;
        call.returned(Kind::OfferForwarded, match result {
            OfferResult::Accepted => ResultKind::Accepted,
            OfferResult::Rejected(ConsumerStop::Cancelled) => ResultKind::RejectedCancelled,
            OfferResult::Rejected(ConsumerStop::Failed) => ResultKind::RejectedFailed,
            OfferResult::Unknown => ResultKind::Unknown,
        });
        result
    }
}

// The thread-local guard cannot cross a thread or await. It exists only inside the store callback.
struct HeldHook { previous: Option<Box<dyn FnOnce()>>, _thread: PhantomData<Rc<()>> }
impl HeldHook {
    fn install(hook: Box<dyn FnOnce()>) -> Self {
        Self { previous: lww::BEFORE_HELD_COMMIT.with(|slot| slot.replace(Some(hook))), _thread: PhantomData }
    }
}
impl Drop for HeldHook {
    fn drop(&mut self) { lww::BEFORE_HELD_COMMIT.with(|slot| slot.replace(self.previous.take())); }
}

struct ObservedStore { inner: Arc<dyn OwnerStore>, actor: Actor }
impl OwnerStore for ObservedStore {
    fn segment(&self, work: &mut dyn FnMut(&mut PersistentStore)) -> StoreResult<()> {
        let session = &self.actor.session;
        session.record(Kind::OutputSegmentEntered, None);
        session.wait_blocking(Gate::PreAdmission);
        self.inner.segment(&mut |store| {
            let _hook_count = session.hook();
            let held = session.clone();
            let _hook = HeldHook::install(Box::new(move || {
                held.record(Kind::OutputAdmitted, None);
                held.wait_blocking(Gate::Admitted);
            }));
            work(store);
        })
    }
}

pub(crate) fn output_store(inner: Arc<dyn OwnerStore>, session: Option<&Arc<Session>>) -> Arc<dyn OwnerStore> {
    match session.and_then(|session| session.enroll()) {
        Some(actor) => Arc::new(ObservedStore { inner, actor }),
        None => inner,
    }
}

pub(crate) fn cancel_scope<T>(actor: Option<Actor>, work: impl FnOnce() -> T) -> T {
    let Some(actor) = actor else { return work() };
    let _hook_count = actor.session.hook();
    let session = actor.session.clone();
    generation::with_finish_waiting_for_segment(Box::new(move || session.record(Kind::CancelWaiting, None)), work)
}

#[cfg(test)]
#[path = "adapters_tests.rs"]
mod tests;
