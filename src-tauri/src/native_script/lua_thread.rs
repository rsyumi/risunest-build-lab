//! The one long-lived thread that runs every `HostRequest::Lua`. It owns the engine cache, so engines never cross
//! threads; requests and replies cross it as owned, `Send` values.
use super::lua_edit::{answer, Context, EditFailure, LuaHost, Records, PANICKED};
pub(crate) use super::lua_edit::Deadlines;
pub(crate) use super::lua_view::ConversationSnapshot;
use risunest_ir::stage::{Effect, HostReply, HostRequest};
use risunest_ir::Budget;
use risunest_native_lua_experimental::{Cancellation, Limits};
use std::{cell::RefCell, panic::{catch_unwind, AssertUnwindSafe}, rc::Rc, sync::{mpsc, Arc}, thread::{self, JoinHandle}};

/// The Lua thread's stack. A Lua call nests C frames up to `LUAI_MAXCCALLS`, which the default 2 MiB of a spawned thread
/// does not cover on every target.
pub(crate) const STACK_BYTES: usize = 8 << 20;

#[derive(Clone, Copy)]
pub(crate) struct LuaThreadConfig {
    /// The limits of each engine; `memory_bytes` is its Lua heap.
    pub limits: Limits,
    pub deadlines: Deadlines,
    /// `scriptingEngineCacheEntries`: at most this many idle engines stay cached, each up to `limits.memory_bytes`.
    pub capacity: usize,
}

/// One request with what the host reads besides it.
pub(crate) struct LuaJob {
    pub request: HostRequest,
    /// `ownerChaId` of the engine cache key.
    pub owner: String,
    /// For an edit request: the code of each trigger whose first effect is `triggerlua`, character triggers first.
    pub triggers: Vec<String>,
    /// The records the request reads, before its `LuaView` is replayed over them.
    pub snapshot: Arc<ConversationSnapshot>,
    /// Expands persona text. Build this processor from the effective records after applying the request's `LuaView`
    /// (character, chat and message effects); `snapshot` remains the records before that view. The host applies only
    /// this call's message edits to the processor and supplies its live scoped variables, never replaying `LuaView`.
    pub processor: Option<Box<dyn risunest_ir::stage::ScopedTextProcessor + Send>>,
    pub budget: Budget,
}

/// A native failure: the request ended without a reply. `effects` are the writes the request made before it, which TS
/// keeps; the owner applies them once and never retries the request.
#[derive(Debug)]
pub(crate) struct LuaFailure {
    pub reason: String,
    pub effects: Vec<Effect>,
}

impl LuaFailure {
    fn plain(reason: impl Into<String>) -> Self { Self { reason: reason.into(), effects: Vec::new() } }

    fn from_edit(failure: EditFailure) -> Self {
        Self { reason: failure.reason, effects: failure.saved.effects }
    }
}

pub(crate) type LuaResponse = Result<HostReply, LuaFailure>;

struct Envelope {
    job: LuaJob,
    reply: mpsc::Sender<LuaResponse>,
}

pub(crate) struct LuaThread {
    sender: Option<mpsc::Sender<Envelope>>,
    shutdown: Cancellation,
    handle: Option<JoinHandle<()>>,
}

impl LuaThread {
    pub(crate) fn spawn(config: LuaThreadConfig) -> std::io::Result<Self> {
        let (sender, jobs) = mpsc::channel();
        let shutdown = Cancellation::default();
        let cancellation = shutdown.clone();
        let handle = thread::Builder::new().name("risunest-lua".into()).stack_size(STACK_BYTES)
            .spawn(move || serve(config, jobs, cancellation))?;
        Ok(Self { sender: Some(sender), shutdown, handle: Some(handle) })
    }

    /// Queues a request; the receiver gets its one response. Returns the job when the thread has stopped.
    pub(crate) fn submit(&self, job: LuaJob) -> Result<mpsc::Receiver<LuaResponse>, Box<LuaJob>> {
        let Some(sender) = &self.sender else { return Err(Box::new(job)) };
        let (reply, response) = mpsc::channel();
        sender.send(Envelope { job, reply }).map_err(|error| Box::new(error.0.job))?;
        Ok(response)
    }

    /// Cancels the running request, answers the queued ones `Cancelled` and joins the thread.
    pub(crate) fn shutdown(&mut self) {
        self.shutdown.cancel();
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            // A panic outside a request already ended the thread; there is nothing left to stop.
            let _ = handle.join();
        }
    }
}

impl Drop for LuaThread {
    fn drop(&mut self) { self.shutdown(); }
}

fn serve(config: LuaThreadConfig, jobs: mpsc::Receiver<Envelope>, shutdown: Cancellation) {
    let pinned = locale::pin();
    let mut host = None;
    while let Ok(Envelope { job, reply }) = jobs.recv() {
        let response = if shutdown.is_cancelled() {
            Ok(HostReply::Cancelled)
        } else if let Err(reason) = &pinned {
            Err(LuaFailure::plain(reason.clone()))
        } else {
            match catch_unwind(AssertUnwindSafe(|| handle(&config, &mut host, job, &shutdown))) {
                Ok(response) => response,
                // The engines may hold state the panic interrupted.
                Err(_) => {
                    host = None;
                    Err(LuaFailure::plain(PANICKED))
                }
            }
        };
        // The owner stopped waiting; nothing reads the response.
        let _ = reply.send(response);
    }
}

fn handle(config: &LuaThreadConfig, host: &mut Option<LuaHost>, job: LuaJob, shutdown: &Cancellation) -> LuaResponse {
    let host = match host {
        Some(host) => host,
        None => host.insert(LuaHost::new(config.limits, config.deadlines, config.capacity).map_err(LuaFailure::plain)?),
    };
    let LuaJob { request, owner, triggers, snapshot, processor, budget } = job;
    let processor = processor.map(|processor| Rc::new(RefCell::new(processor)));
    let records = match &processor {
        Some(processor) => Records::ProcessedSnapshot(&snapshot, processor),
        None => Records::Snapshot(&snapshot),
    };
    let context = Context { owner: &owner, triggers: &triggers, records, cancellation: shutdown, budget: &budget };
    answer(host, request, context).map_err(LuaFailure::from_edit)
}

/// Pins the calling thread's C locale to "C". Lua formats and parses numbers through the C library (`tostring`,
/// `string.format("%.14g")`, `tonumber`), which follows the locale. GTK sets the Linux process locale from the
/// environment, so a decimal comma there would turn 1.5 into "1,5".
pub(super) mod locale {
    #[cfg(unix)]
    pub(crate) fn pin() -> Result<(), String> {
        // SAFETY: `newlocale` reads a NUL-terminated name and allocates a locale object this thread then uses; the
        // object stays allocated for the rest of the process, so `uselocale` never refers to freed memory.
        unsafe {
            let locale = libc::newlocale(libc::LC_ALL_MASK, c"C".as_ptr(), std::ptr::null_mut());
            if locale.is_null() { return Err("lua-locale".into()); }
            if libc::uselocale(locale).is_null() {
                let _ = libc::freelocale(locale);
                return Err("lua-locale".into());
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn pin() -> Result<(), String> {
        use std::ffi::{c_char, c_int};
        unsafe extern "C" {
            fn _configthreadlocale(kind: c_int) -> c_int;
            fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
        }
        const ENABLE_PER_THREAD_LOCALE: c_int = 1;
        const LC_ALL: c_int = 0;
        // SAFETY: both are UCRT functions; `setlocale` reads a NUL-terminated name and, after
        // `_configthreadlocale`, changes only this thread's locale.
        unsafe {
            if _configthreadlocale(ENABLE_PER_THREAD_LOCALE) == -1 || setlocale(LC_ALL, c"C".as_ptr()).is_null() {
                return Err("lua-locale".into());
            }
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    pub(crate) fn pin() -> Result<(), String> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::super::lua_edit::ir_limits;
    use super::*;
    use risunest_ir::stage::{LuaCall, LuaDeadline, LuaMode, LuaReply, LuaTrigger, LuaView, TriggerMode};
    use risunest_ir::{Text, Value};
    use std::{sync::atomic::AtomicBool, time::{Duration, Instant}};

    fn budget() -> Budget { Budget::new(ir_limits(), Arc::new(AtomicBool::new(false))).unwrap() }

    fn request(code: &str, deadline: LuaDeadline, budget: &Budget) -> HostRequest {
        let call = LuaCall { code: Text::from_str(code, budget).unwrap(), entry: Text::from_str("start", budget).unwrap(), low_level: false, deadline };
        HostRequest::Lua { trigger: LuaTrigger::Effect { trigger: 0, effect: 0, call }, mode: LuaMode::Trigger(TriggerMode::Start),
            payload: Value::Undefined, view: LuaView::default() }
    }

    fn job(code: &str, deadline: LuaDeadline, budget: &Budget) -> LuaJob {
        LuaJob { request: request(code, deadline, budget), owner: "char-b".into(), triggers: Vec::new(),
            snapshot: Arc::new(ConversationSnapshot::default()), processor: None, budget: budget.clone() }
    }

    fn config() -> LuaThreadConfig { LuaThreadConfig { limits: Limits::default(), deadlines: Deadlines::default(), capacity: 4 } }

    fn text(response: &LuaResponse) -> Option<&str> {
        match response { Ok(HostReply::Lua(LuaReply { value: Value::Text(text), .. })) => text.as_unicode(), _ => None }
    }

    #[test]
    fn requests_cross_to_the_lua_thread_and_shutdown_cancels_the_running_one() {
        fn sendable<T: Send>() {}
        sendable::<LuaJob>();
        sendable::<LuaResponse>();
        sendable::<LuaThread>();
        let budget = budget();
        let mut thread = LuaThread::spawn(config()).unwrap();
        let code = "function onStart(id) setChatVar(id, 'k', 'v') return 'done' end";
        let reply = thread.submit(job(code, LuaDeadline::Listener, &budget)).ok().unwrap().recv().unwrap();
        assert_eq!(text(&reply), Some("done"), "{reply:?}");
        assert!(matches!(&reply, Ok(HostReply::Lua(LuaReply { effects, .. })) if effects.len() == 1), "{reply:?}");
        let running = thread.submit(job("function onStart(id) while true do end end", LuaDeadline::UserStarted, &budget)).ok().unwrap();
        assert!(running.recv_timeout(Duration::from_millis(300)).is_err(), "the request ended before shutdown");
        let started = Instant::now();
        thread.shutdown();
        assert!(matches!(running.recv().unwrap(), Ok(HostReply::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(30));
        assert!(thread.submit(job(code, LuaDeadline::Listener, &budget)).is_err());
    }

    fn response_once(response: mpsc::Receiver<LuaResponse>) -> LuaResponse {
        let reply = response.recv_timeout(Duration::from_secs(30)).expect("Lua thread did not answer the request");
        assert!(matches!(response.recv_timeout(Duration::from_secs(30)), Err(mpsc::RecvTimeoutError::Disconnected)),
            "the request must have exactly one response");
        reply
    }

    fn panic_job(code: &str, panic: bool, budget: &Budget) -> LuaJob {
        let mut job = job(code, LuaDeadline::Listener, budget);
        if panic {
            let HostRequest::Lua { view, .. } = &mut job.request else { unreachable!() };
            view.effects.push(Effect::ChatVariable { name: Text::from_str("boom", budget).unwrap(),
                value: Value::Text(Text::from_str("yes", budget).unwrap()) });
        }
        job
    }

    fn count(response: LuaResponse) -> f64 {
        match response {
            Ok(HostReply::Lua(LuaReply { value: Value::Number(count), effects, stop_sending: false })) if effects.is_empty() => count,
            reply => panic!("expected an unchanged successful count: {reply:?}"),
        }
    }

    fn panic_prefix(response: LuaResponse) {
        let failure = response.expect_err("a host panic must fail the request");
        assert_eq!(failure.reason, PANICKED);
        assert!(matches!(failure.effects.as_slice(),
            [Effect::ChatVariable { name, value: Value::Text(value) }]
                if name.as_unicode() == Some("before") && value.as_unicode() == Some("1")),
            "only writes before the panic may survive: {:?}", failure.effects);
    }

    #[test]
    fn callback_panics_through_submit_fail_once_and_replace_only_the_faulty_engine() {
        use super::super::lua_edit::TEST_PANIC;
        let budget = budget();
        let thread = LuaThread::spawn(config()).unwrap();
        let unaffected = "count = 0 function onStart(id) count = count + 1 return count end";
        let submit = |code: &str, panic| response_once(thread.submit(panic_job(code, panic, &budget)).ok().unwrap());
        for caught in [
            format!("{TEST_PANIC}()"),
            format!("pcall({TEST_PANIC})"),
            format!("xpcall({TEST_PANIC}, function() setChatVar(id, 'handler', 'forbidden') end)"),
            format!("coroutine.resume(coroutine.create({TEST_PANIC}))"),
            format!("coroutine.wrap(function() pcall({TEST_PANIC}) end)()"),
        ] {
            let code = format!("count = 0 function onStart(id)
                count = count + 1
                if getChatVar(id, 'boom') == 'yes' then
                    setChatVar(id, 'before', '1')
                    {caught}
                    setChatVar(id, 'after', 'forbidden')
                end
                return count
            end");
            assert_eq!([count(submit(&code, false)), count(submit(&code, false))], [1.0, 2.0], "{caught}");
            // A healthy key must survive the faulty engine's removal.
            let healthy_before = count(submit(unaffected, false));
            panic_prefix(submit(&code, true));
            assert_eq!([count(submit(&code, false)), count(submit(&code, false))], [1.0, 2.0], "{caught}");
            assert_eq!(count(submit(unaffected, false)), healthy_before + 1.0, "{caught}");
        }
    }

    #[test]
    fn runtime_unwind_through_submit_preserves_prefix_and_restarts_the_faulty_engine() {
        use super::super::lua_edit::TEST_UNWIND;
        let budget = budget();
        let thread = LuaThread::spawn(config()).unwrap();
        let code = format!("count = 0 function onStart(id)
            count = count + 1
            if getChatVar(id, 'boom') == 'yes' then
                setChatVar(id, 'before', '1')
                {TEST_UNWIND}()
                coroutine.yield()
                setChatVar(id, 'after', 'forbidden')
            end
            return count
        end");
        let submit = |panic| response_once(thread.submit(panic_job(&code, panic, &budget)).ok().unwrap());
        assert_eq!([count(submit(false)), count(submit(false))], [1.0, 2.0]);
        panic_prefix(submit(true));
        assert_eq!([count(submit(false)), count(submit(false))], [1.0, 2.0]);
    }

    #[test]
    fn an_owned_persona_processor_crosses_to_the_lua_thread() {
        use risunest_cbs_experimental::cbs::processor::{Processor, Snapshot};
        use risunest_ir::rng::Tape;
        let budget = budget();
        let thread = LuaThread::spawn(config()).unwrap();
        let mut job = job("function onStart(id) setChatVar(id, 'scope', 'thread-value') return getPersonaDescription(id) end",
            LuaDeadline::Listener, &budget);
        job.snapshot = Arc::new(ConversationSnapshot { root: serde_json::json!({ "personaPrompt": "persona {{getvar::scope}}" }),
            ..ConversationSnapshot::default() });
        job.processor = Some(Box::new(Processor::new(Snapshot::default(), Tape::new(Vec::new()))));
        let reply = response_once(thread.submit(job).ok().unwrap());
        assert_eq!(text(&reply), Some("persona thread-value"), "{reply:?}");
        assert!(matches!(&reply, Ok(HostReply::Lua(LuaReply { effects, .. }))
            if matches!(effects.as_slice(), [Effect::ChatVariable { name, value: Value::Text(value) }]
                if name.as_unicode() == Some("scope") && value.as_unicode() == Some("thread-value"))), "{reply:?}");
    }

    // Sets this thread's C locale to one with a decimal comma; false when the system has none.
    #[cfg(windows)]
    fn decimal_comma() -> bool {
        use std::ffi::{c_char, c_int};
        unsafe extern "C" {
            fn _configthreadlocale(kind: c_int) -> c_int;
            fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
        }
        // SAFETY: per-thread locale as `locale::pin` sets it, with a NUL-terminated name.
        unsafe { _configthreadlocale(1) != -1 && !setlocale(0, c"de-DE".as_ptr()).is_null() }
    }

    #[cfg(unix)]
    fn decimal_comma() -> bool {
        // SAFETY: as in `locale::pin`; the locale object stays allocated for the process.
        unsafe {
            [c"de_DE.UTF-8", c"de_DE.utf8", c"de_DE"].iter().any(|name| {
                let locale = libc::newlocale(libc::LC_ALL_MASK, name.as_ptr(), std::ptr::null_mut());
                !locale.is_null() && !libc::uselocale(locale).is_null()
            })
        }
    }

    // Lua formats and parses numbers through the C locale; the pin keeps a decimal point whatever the process locale is.
    #[test]
    fn the_pinned_locale_keeps_a_decimal_point() {
        std::thread::spawn(|| {
            let budget = budget();
            let mut host = LuaHost::new(Limits::default(), Deadlines::default(), 4).unwrap();
            let snapshot = Arc::new(ConversationSnapshot::default());
            let code = "function onStart(id)
                return table.concat({ tostring(1.5), string.format('%.14g', 2.25), tostring(tonumber('3,5')), json.encode({ 0.5 }) }, '|')
            end";
            let mut run = || {
                let context = Context { owner: "char-b", triggers: &[], records: Records::Snapshot(&snapshot),
                    cancellation: &Cancellation::default(), budget: &budget };
                let response = answer(&mut host, request(code, LuaDeadline::Listener, &budget), context).map_err(|failure| LuaFailure::plain(failure.reason));
                text(&response).map(str::to_owned)
            };
            if decimal_comma() {
                assert_eq!(run().as_deref(), Some("1,5|2,25|3,5|[0,5]"));
            }
            locale::pin().unwrap();
            assert_eq!(run().as_deref(), Some("1.5|2.25|nil|[0.5]"));
        }).join().unwrap();
    }
}
