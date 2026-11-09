//! Account read scheduling for the official asset restore job. The rules are
//! those of the renderer pool this job replaces; time is passed in so tests run
//! on a fixed clock.
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

const MAX_ATTEMPTS: u32 = 3;
const SERVICE_STOP: u32 = 5;
const MAX_FALLBACK_MS: f64 = 300_000.0;
// Keep reported deadlines exact as JavaScript numbers.
const MAX_DEADLINE_MS: u64 = (1 << 53) - 1;

/// Overload state shared by every run for one account and origin.
#[derive(Debug, Default)]
pub(super) struct Pause {
    epoch: u64,
    until: u64,
    paused: bool,
    probe: Option<u64>,
    cap: usize,
    transient: u32,
    episodes: u32,
    healthy: usize,
}

type PauseKey = (String, String);
static PAUSES: OnceLock<Mutex<HashMap<PauseKey, Arc<Mutex<Pause>>>>> = OnceLock::new();
static RUN_TOKENS: AtomicU64 = AtomicU64::new(1);

pub(super) fn shared_pause(account_id: &str, origin: &str) -> Arc<Mutex<Pause>> {
    let mut pauses = PAUSES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        pauses
            .entry((account_id.to_owned(), origin.to_owned()))
            .or_default(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum StopReason {
    Authentication,
    ReauthenticationNeeded,
    Local,
    LibraryChanged,
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Settled {
    Installed,
    Missing,
    Failed,
}

/// How one account read ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Exchange {
    Body,
    Missing,
    /// A completed exchange that no retry can change.
    Permanent,
    Retryable {
        status: Option<u16>,
        retry_after: Option<String>,
    },
    /// 401 or 403.
    Rejected,
    Local,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// The body is on disk; its slot is free and it waits for installation.
    Install,
    Settled(Settled),
    Requeued,
    Unsettled,
}

#[derive(Debug)]
pub(super) struct Attempt {
    pub(super) index: usize,
    attempts: u32,
    ticket: u64,
    probe: bool,
}

#[derive(Debug)]
struct Retry {
    index: usize,
    attempts: u32,
    ready: u64,
}

pub(super) struct Scheduler {
    nominal: usize,
    limit: usize,
    pause: Arc<Mutex<Pause>>,
    token: u64,
    reauthenticated: bool,
    total: usize,
    received: usize,
    next_key: usize,
    retries: Vec<Retry>,
    active: usize,
    service_failures: u32,
    reason: Option<StopReason>,
    jitter: Box<dyn FnMut() -> f64 + Send>,
}

impl Scheduler {
    pub(super) fn new(
        nominal: usize,
        pause: Arc<Mutex<Pause>>,
        total: usize,
        reauthenticated: bool,
        jitter: Box<dyn FnMut() -> f64 + Send>,
    ) -> Self {
        let limit = {
            let mut state = lock(&pause);
            if !state.paused {
                state.cap = nominal;
                state.transient = 0;
                state.episodes = 0;
                state.healthy = 0;
            }
            nominal.min(state.cap)
        };
        Self {
            nominal,
            limit,
            pause,
            token: RUN_TOKENS.fetch_add(1, Ordering::Relaxed),
            reauthenticated,
            total,
            received: 0,
            next_key: 0,
            retries: Vec::new(),
            active: 0,
            service_failures: 0,
            reason: None,
            jitter,
        }
    }

    pub(super) fn receive(&mut self, count: usize) {
        self.received = (self.received + count).min(self.total);
    }

    pub(super) fn reason(&self) -> Option<StopReason> {
        self.reason
    }

    pub(super) fn stop(&mut self, reason: StopReason) {
        self.reason.get_or_insert(reason);
    }

    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    pub(super) fn active(&self) -> usize {
        self.active
    }

    fn queued(&self) -> bool {
        self.next_key < self.total || !self.retries.is_empty()
    }

    /// Nothing is in flight and nothing more will start.
    pub(super) fn done(&self) -> bool {
        self.active == 0 && (self.reason.is_some() || !self.queued())
    }

    /// The next read to start, if a slot, the server and the install backlog allow one.
    pub(super) fn next(&mut self, now: u64, backlog_open: bool) -> Option<Attempt> {
        if self.reason.is_some() || !backlog_open || self.active >= self.limit {
            return None;
        }
        let mut pause = lock(&self.pause);
        if pause.paused && (pause.until > now || pause.probe.is_some()) {
            return None;
        }
        let item = match self.retries.iter().position(|item| item.ready <= now) {
            Some(index) => self.retries.remove(index),
            None if self.next_key < self.received
                && self.active + self.retries.len() < self.limit =>
            {
                self.next_key += 1;
                Retry { index: self.next_key - 1, attempts: 0, ready: 0 }
            }
            None => return None,
        };
        let probe = pause.paused;
        if probe {
            pause.probe = Some(self.token);
        }
        self.active += 1;
        Some(Attempt {
            index: item.index,
            attempts: item.attempts + 1,
            ticket: pause.epoch,
            probe,
        })
    }

    /// A read that a stop or cancellation abandoned; it settles nothing.
    pub(super) fn abandon(&mut self, _attempt: Attempt) {
        self.active -= 1;
    }

    pub(super) fn finish(&mut self, attempt: Attempt, exchange: Exchange, now: u64) -> Outcome {
        self.active -= 1;
        match exchange {
            Exchange::Body | Exchange::Missing => {
                self.succeeded(&attempt);
                if exchange == Exchange::Body {
                    Outcome::Install
                } else {
                    Outcome::Settled(Settled::Missing)
                }
            }
            Exchange::Local => {
                self.stop(StopReason::Local);
                Outcome::Settled(Settled::Failed)
            }
            Exchange::Rejected if self.reauthenticated => {
                self.stop(StopReason::Authentication);
                Outcome::Settled(Settled::Failed)
            }
            Exchange::Rejected => {
                self.stop(StopReason::ReauthenticationNeeded);
                Outcome::Unsettled
            }
            Exchange::Retryable { status, retry_after } => {
                let slow = {
                    let mut pause = lock(&self.pause);
                    pause.transient += 1;
                    status == Some(429)
                        || (status == Some(503) && retry_after.as_deref().is_some_and(|value| !value.is_empty()))
                        || pause.transient >= 2
                        || attempt.probe
                };
                if slow {
                    self.slow(attempt.ticket, retry_after.as_deref(), now);
                }
                if attempt.attempts < MAX_ATTEMPTS {
                    let paused = lock(&self.pause).paused;
                    let delay = if paused { 0 } else { self.fallback(attempt.attempts - 1) };
                    self.retries.push(Retry {
                        index: attempt.index,
                        attempts: attempt.attempts,
                        ready: now.saturating_add(delay),
                    });
                    return Outcome::Requeued;
                }
                self.service_failures += 1;
                self.failed()
            }
            Exchange::Permanent => {
                let mut pause = lock(&self.pause);
                // A permanent object response is a completed exchange, but not a successful recovery probe.
                if attempt.probe && attempt.ticket == pause.epoch {
                    pause.probe = None;
                }
                drop(pause);
                self.failed()
            }
        }
    }

    fn failed(&mut self) -> Outcome {
        if self.service_failures >= SERVICE_STOP {
            self.stop(StopReason::Service);
        }
        Outcome::Settled(Settled::Failed)
    }

    fn succeeded(&mut self, attempt: &Attempt) {
        let mut pause = lock(&self.pause);
        if attempt.ticket != pause.epoch {
            return;
        }
        self.service_failures = 0;
        pause.transient = 0;
        if attempt.probe && pause.probe == Some(self.token) {
            pause.paused = false;
            pause.probe = None;
            pause.until = 0;
            pause.episodes = pause.episodes.saturating_sub(1);
        } else if !pause.paused {
            pause.healthy += 1;
            if pause.healthy >= self.limit * 2 {
                pause.cap = self.nominal.min(pause.cap + 1);
                self.limit = self.nominal.min(pause.cap);
                pause.healthy = 0;
                pause.episodes = pause.episodes.saturating_sub(1);
            }
        }
    }

    fn slow(&mut self, ticket: u64, retry_after: Option<&str>, now: u64) {
        let episodes = {
            let mut pause = lock(&self.pause);
            if ticket == pause.epoch {
                pause.epoch += 1;
                pause.cap = (self.limit / 2).max(1);
                pause.episodes += 1;
                pause.healthy = 0;
                pause.paused = true;
                pause.probe = None;
            }
            self.limit = self.limit.min(pause.cap);
            pause.episodes
        };
        let deadline = match retry_deadline(retry_after, now) {
            Some(deadline) => deadline,
            None => now.saturating_add(self.fallback(episodes.saturating_sub(1))),
        };
        let mut pause = lock(&self.pause);
        pause.until = pause.until.max(deadline);
    }

    fn fallback(&mut self, attempt: u32) -> u64 {
        let jitter = (self.jitter)().clamp(0.0, 1.0);
        (1000.0 * 2f64.powi(attempt.min(18) as i32) * (1.0 + jitter * 0.2)).min(MAX_FALLBACK_MS) as u64
    }

    /// When `next` can start something without another event, if it ever can.
    pub(super) fn wake_at(&self, backlog_open: bool) -> Option<u64> {
        let pause = lock(&self.pause);
        if self.reason.is_some() || !backlog_open || self.active >= self.limit || pause.probe.is_some() {
            return None;
        }
        let fresh = self.next_key < self.received && self.active + self.retries.len() < self.limit;
        let ready = self
            .retries
            .iter()
            .map(|item| item.ready)
            .chain(fresh.then_some(0))
            .min()?;
        Some(ready.max(if pause.paused { pause.until } else { 0 }))
    }

    /// The deadline the progress display counts down to.
    pub(super) fn waiting_until(&self, now: u64) -> Option<u64> {
        let pause = lock(&self.pause);
        if pause.paused {
            return Some(pause.until);
        }
        self.retries.iter().find(|item| item.ready > now).map(|item| item.ready)
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        let mut pause = lock(&self.pause);
        if pause.probe == Some(self.token) {
            pause.probe = None;
        }
    }
}

fn lock(pause: &Mutex<Pause>) -> MutexGuard<'_, Pause> {
    pause.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A `Retry-After` value as an absolute deadline in epoch milliseconds, never before `now`.
pub(super) fn retry_deadline(value: Option<&str>, now: u64) -> Option<u64> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    let deadline = if text.bytes().all(|byte| byte.is_ascii_digit()) {
        let seconds = text.parse::<u64>().unwrap_or(u64::MAX);
        now.saturating_add(seconds.saturating_mul(1000))
    } else if text.bytes().any(|byte| byte.is_ascii_alphabetic()) {
        let date = httpdate::parse_http_date(text).ok()?;
        u64::try_from(date.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()?
    } else {
        return None;
    };
    Some(deadline.max(now).min(MAX_DEADLINE_MS.max(now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    fn scheduler(nominal: usize, total: usize) -> Scheduler {
        let pause = shared_pause(&uuid::Uuid::new_v4().to_string(), "https://synthetic.invalid");
        let mut scheduler = Scheduler::new(nominal, pause, total, true, Box::new(|| 1.0));
        scheduler.receive(total);
        scheduler
    }

    fn retryable(status: u16, retry_after: Option<&str>) -> Exchange {
        Exchange::Retryable { status: Some(status), retry_after: retry_after.map(str::to_owned) }
    }

    #[derive(Default)]
    struct Run {
        now: u64,
        calls: Vec<usize>,
        settled: BTreeMap<usize, Settled>,
        limits: Vec<usize>,
        waits: Vec<u64>,
    }

    /// Starts every read the scheduler allows, answers the oldest one, and advances the
    /// clock only when nothing can start, until the run is done.
    fn drive(scheduler: &mut Scheduler, run: &mut Run, mut respond: impl FnMut(usize, usize) -> Exchange) {
        let mut active = VecDeque::new();
        loop {
            while let Some(attempt) = scheduler.next(run.now, true) {
                active.push_back(attempt);
            }
            run.limits.push(scheduler.limit());
            if let Some(until) = scheduler.waiting_until(run.now) {
                run.waits.push(until - run.now);
            }
            if scheduler.done() {
                return;
            }
            if let Some(attempt) = active.pop_front() {
                let index = attempt.index;
                run.calls.push(index);
                let exchange = respond(index, run.calls.len());
                match scheduler.finish(attempt, exchange, run.now) {
                    Outcome::Install => { run.settled.insert(index, Settled::Installed); }
                    Outcome::Settled(result) => { run.settled.insert(index, result); }
                    Outcome::Requeued | Outcome::Unsettled => {}
                }
                if scheduler.reason().is_some() {
                    while let Some(abandoned) = active.pop_front() {
                        scheduler.abandon(abandoned);
                    }
                }
                continue;
            }
            run.now = scheduler.wake_at(true).expect("an idle run must have a wake time").max(run.now);
        }
    }

    #[test]
    fn an_authentication_stop_fails_only_its_cause_and_a_first_rejection_settles_nothing() {
        for reauthenticated in [true, false] {
            let pause = shared_pause(&uuid::Uuid::new_v4().to_string(), "https://synthetic.invalid");
            let mut scheduler = Scheduler::new(2, pause, 4, reauthenticated, Box::new(|| 0.0));
            scheduler.receive(4);
            let first = scheduler.next(0, true).unwrap();
            let second = scheduler.next(0, true).unwrap();
            let outcome = scheduler.finish(first, Exchange::Rejected, 0);
            if reauthenticated {
                assert_eq!(outcome, Outcome::Settled(Settled::Failed));
                assert_eq!(scheduler.reason(), Some(StopReason::Authentication));
            } else {
                assert_eq!(outcome, Outcome::Unsettled);
                assert_eq!(scheduler.reason(), Some(StopReason::ReauthenticationNeeded));
            }
            assert!(scheduler.next(0, true).is_none());
            assert!(!scheduler.done());
            scheduler.abandon(second);
            assert!(scheduler.done());
        }
    }

    #[test]
    fn a_local_failure_stops_without_retrying_other_files() {
        let mut scheduler = scheduler(1, 12);
        let mut run = Run::default();
        drive(&mut scheduler, &mut run, |_, _| Exchange::Local);
        assert_eq!(scheduler.reason(), Some(StopReason::Local));
        assert_eq!(run.settled.values().copied().collect::<Vec<_>>(), vec![Settled::Failed]);
    }

    #[test]
    fn a_slot_frees_when_its_body_is_on_disk_and_the_install_backlog_bounds_admission() {
        let mut scheduler = scheduler(2, 6);
        let first = scheduler.next(0, true).unwrap();
        let second = scheduler.next(0, true).unwrap();
        assert!(scheduler.next(0, true).is_none());
        assert_eq!(scheduler.finish(first, Exchange::Body, 0), Outcome::Install);
        assert!(scheduler.next(0, false).is_none());
        assert_eq!(scheduler.wake_at(false), None);
        assert_eq!(scheduler.next(0, true).map(|attempt| attempt.index), Some(2));
        assert_eq!(scheduler.finish(second, Exchange::Body, 0), Outcome::Install);
        assert_eq!(scheduler.active(), 1);
    }

    #[test]
    fn keys_that_have_not_arrived_wait_without_a_timer() {
        let pause = shared_pause(&uuid::Uuid::new_v4().to_string(), "https://synthetic.invalid");
        let mut scheduler = Scheduler::new(2, pause, 3, true, Box::new(|| 0.0));
        assert!(scheduler.next(0, true).is_none());
        assert_eq!(scheduler.wake_at(true), None);
        assert!(!scheduler.done());
        scheduler.receive(1);
        let first = scheduler.next(0, true).unwrap();
        assert!(scheduler.next(0, true).is_none());
        assert_eq!(scheduler.finish(first, Exchange::Missing, 0), Outcome::Settled(Settled::Missing));
        assert!(!scheduler.done());
        scheduler.receive(2);
        assert_eq!(scheduler.next(0, true).map(|attempt| attempt.index), Some(1));
    }

    #[test]
    fn simultaneous_overload_reduces_once_and_recovers_through_one_probe() {
        let mut scheduler = scheduler(4, 8);
        let initial = (0..4).map(|_| scheduler.next(0, true).unwrap()).collect::<Vec<_>>();
        assert!(scheduler.next(0, true).is_none());
        let mut initial = initial.into_iter();
        assert_eq!(scheduler.finish(initial.next().unwrap(), retryable(429, Some("45")), 0), Outcome::Requeued);
        assert_eq!(scheduler.finish(initial.next().unwrap(), retryable(429, Some("45")), 0), Outcome::Requeued);
        assert_eq!(scheduler.finish(initial.next().unwrap(), Exchange::Body, 0), Outcome::Install);
        assert_eq!(scheduler.finish(initial.next().unwrap(), Exchange::Body, 0), Outcome::Install);
        assert!(scheduler.next(44_999, true).is_none());
        assert_eq!(scheduler.limit(), 2);
        assert_eq!(scheduler.waiting_until(44_999), Some(45_000));
        assert_eq!(scheduler.wake_at(true), Some(45_000));
        let probe = scheduler.next(45_000, true).unwrap();
        assert!(scheduler.next(45_000, true).is_none());
        assert_eq!(scheduler.wake_at(true), None);
        assert_eq!(scheduler.finish(probe, Exchange::Body, 45_000), Outcome::Install);
        let resumed = [scheduler.next(45_000, true).unwrap(), scheduler.next(45_000, true).unwrap()];
        assert!(scheduler.next(45_000, true).is_none());
        for attempt in resumed {
            assert_eq!(scheduler.finish(attempt, Exchange::Body, 45_000), Outcome::Install);
        }
        let mut run = Run { now: 45_000, ..Default::default() };
        drive(&mut scheduler, &mut run, |_, _| Exchange::Body);
        assert!(run.limits.iter().all(|limit| *limit >= 2));
        assert_eq!(scheduler.reason(), None);
    }

    #[test]
    fn five_distinct_exhausted_objects_stop_the_run_at_concurrency_one() {
        let mut scheduler = scheduler(1, 20);
        let mut run = Run::default();
        drive(&mut scheduler, &mut run, |_, _| retryable(429, Some("0")));
        assert_eq!(scheduler.reason(), Some(StopReason::Service));
        assert_eq!(run.calls.len(), 15);
        assert_eq!(run.settled.values().copied().collect::<Vec<_>>(), vec![Settled::Failed; 5]);
    }

    #[test]
    fn sustained_successful_exchanges_recover_the_nominal_concurrency() {
        let mut scheduler = scheduler(4, 20);
        let mut run = Run::default();
        drive(&mut scheduler, &mut run, |_, call| if call == 1 { retryable(429, Some("0")) } else { Exchange::Body });
        assert!(run.limits.contains(&2));
        assert_eq!(run.limits.last(), Some(&4));
        assert_eq!(run.settled.len(), 20);
        assert!(run.settled.values().all(|result| *result == Settled::Installed));
    }

    #[test]
    fn fallback_pauses_decay_after_each_recovery_instead_of_accumulating() {
        let mut scheduler = scheduler(1, 8);
        let mut attempts = HashMap::new();
        let mut run = Run::default();
        drive(&mut scheduler, &mut run, |index, _| {
            let attempt = attempts.entry(index).or_insert(0);
            *attempt += 1;
            if *attempt == 1 { retryable(429, None) } else { Exchange::Body }
        });
        assert_eq!(run.settled.len(), 8);
        assert!(run.settled.values().all(|result| *result == Settled::Installed));
        assert_eq!(run.waits.iter().max(), Some(&1200));
    }

    #[test]
    fn permanent_errors_continue_and_transient_failures_retry_within_three_attempts() {
        let mut scheduler = scheduler(2, 10);
        let mut run = Run::default();
        drive(&mut scheduler, &mut run, |_, call| match call {
            1..=6 => Exchange::Permanent,
            7..=8 => Exchange::Retryable { status: None, retry_after: None },
            _ => Exchange::Body,
        });
        assert_eq!(scheduler.reason(), None);
        assert_eq!(run.settled.values().filter(|result| **result == Settled::Installed).count(), 4);
        assert_eq!(run.settled.values().filter(|result| **result == Settled::Failed).count(), 6);
    }

    #[test]
    fn a_503_without_retry_after_slows_only_on_the_second_transient_failure() {
        let mut scheduler = scheduler(4, 8);
        let first = scheduler.next(0, true).unwrap();
        let second = scheduler.next(0, true).unwrap();
        assert_eq!(scheduler.finish(first, retryable(503, None), 0), Outcome::Requeued);
        assert_eq!(scheduler.limit(), 4);
        assert_eq!(scheduler.waiting_until(0), Some(1200));
        assert_eq!(scheduler.finish(second, retryable(503, None), 0), Outcome::Requeued);
        assert_eq!(scheduler.limit(), 2);
        assert_eq!(scheduler.waiting_until(0), Some(1200));
    }

    #[test]
    fn a_server_pause_outlives_the_run_that_received_it() {
        let account = uuid::Uuid::new_v4().to_string();
        let origin = "https://synthetic.invalid";
        let mut first = Scheduler::new(2, shared_pause(&account, origin), 1, true, Box::new(|| 0.0));
        first.receive(1);
        let attempt = first.next(0, true).unwrap();
        assert_eq!(first.finish(attempt, retryable(503, Some("86400")), 1), Outcome::Requeued);
        drop(first);
        let mut second = Scheduler::new(2, shared_pause(&account, origin), 1, true, Box::new(|| 0.0));
        second.receive(1);
        assert!(second.next(60_000, true).is_none());
        assert_eq!(second.waiting_until(60_000), Some(86_400_001));
        assert!(second.next(86_400_001, true).is_some());
    }

    #[test]
    fn retry_after_accepts_delay_seconds_and_http_dates_without_shortening_a_deadline() {
        let now = 1_792_368_000_000;
        assert_eq!(retry_deadline(Some("86400"), now), Some(now + 86_400_000));
        assert_eq!(retry_deadline(Some(&httpdate::fmt_http_date(std::time::UNIX_EPOCH + std::time::Duration::from_millis(now + 600_000))), now), Some(now + 600_000));
        assert_eq!(retry_deadline(Some("Thu, 01 Jan 1970 00:00:00 GMT"), now), Some(now));
        assert_eq!(retry_deadline(Some("nonsense"), now), None);
        assert_eq!(retry_deadline(Some("-1"), now), None);
        assert_eq!(retry_deadline(Some(" "), now), None);
        assert_eq!(retry_deadline(Some("99999999999999999999999"), now), Some(MAX_DEADLINE_MS));
    }
}
