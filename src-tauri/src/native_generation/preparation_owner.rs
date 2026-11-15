use super::{
    inputs::{PreparationError, PreparationSnapshot},
    memory_http::{self as http, Executor, Operation, ReplyEnvelope},
    ollama_decode::{CancelOrigin, Cancellation},
    ollama_transport::{CapturedOllamaConfig, Think, TransportLimits},
    prepare::{self, CoreYield, MachineCore, Names, PreparationAttempt, RawMachineCompletion},
    sub_request::{self, MemoryRequestConfig},
};
use risunest_ir::{stage::{HostReply, HostRequest, StageError}, Budget, Charge, Text};
use risunest_providers::embeddings::Failure;
use std::{borrow::Cow, mem::size_of, sync::{Arc, OnceLock, atomic::{AtomicBool, AtomicU8}}};
use tokio::{sync::Semaphore, task::JoinHandle};

pub(super) fn admit(live: Arc<()>) -> Result<Operation, Failure> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Operation::new_with_liveness(SLOTS.get_or_init(|| Arc::new(Semaphore::new(64))).clone(), prepare::PREPARATION_LIMITS, live)
}

pub(super) struct MemoryConfig {
    model_id: Text, user_name: Text, host: Text, model: Text, user_agent: Text, think: Option<Think>,
    _charge: Charge,
}

impl MemoryConfig {
    pub(super) fn capture(model_id: &str, user_name: &Text, config: CapturedOllamaConfig<'_>, budget: &Budget) -> Result<Self, StageError> {
        let charge = budget.reserve_owned(size_of::<Self>())?;
        Ok(Self { model_id: Text::from_str(model_id, budget)?, user_name: user_name.copy(budget)?,
            host: Text::from_str(config.host, budget)?, model: Text::from_str(config.model, budget)?,
            user_agent: Text::from_str(config.browser_user_agent, budget)?, think: config.think, _charge: charge })
    }
    fn borrowed(&self, budget: &Budget) -> Result<MemoryRequestConfig<'_>, StageError> {
        Ok(MemoryRequestConfig { model_id: unicode(&self.model_id)?, user_name: self.user_name.copy(budget)?,
            ollama: CapturedOllamaConfig { host: unicode(&self.host)?, model: unicode(&self.model)?, streaming: false,
                think: self.think, browser_user_agent: unicode(&self.user_agent)? } })
    }
}

fn unicode(value: &Text) -> Result<&str, StageError> {
    value.as_unicode().ok_or_else(|| StageError::Invalid(Cow::Borrowed("memory-config-unicode")))
}

#[derive(Debug)]
pub(super) enum OwnerError { Http(Failure), Stage(StageError), Join }

impl From<Failure> for OwnerError { fn from(error: Failure) -> Self { Self::Http(error) } }
impl From<StageError> for OwnerError { fn from(error: StageError) -> Self { Self::Stage(error) } }
impl From<risunest_ir::Error> for OwnerError { fn from(error: risunest_ir::Error) -> Self { Self::Http(error.into()) } }

pub(super) enum Work {
    Build(PreparationSnapshot),
    Start(MachineCore),
    Resume { core: MachineCore, reply: HostReply },
    ResumeHttp { core: MachineCore, executor: Executor, reply: ReplyEnvelope },
    Finalize { raw: RawMachineCompletion, snapshot: PreparationSnapshot, names: Names },
}

pub(super) enum HttpResume {
    Rejected { core: MachineCore, error: Failure },
    Resumed { progress: CoreYield, outer: Result<(), Failure> },
}

pub(super) enum TaskValue {
    Built { snapshot: PreparationSnapshot, start: Result<prepare::OwnedStart, PreparationError> },
    Core(CoreYield),
    Http { executor: Executor, result: HttpResume },
    Finalized { snapshot: PreparationSnapshot, attempt: PreparationAttempt },
    Rejected { work: Work, error: Failure },
}

pub(super) struct TaskRejection { pub(super) work: Work, pub(super) error: Failure }
pub(super) struct StepTask { work: Work, charge: Charge, operation: Operation }
pub(super) struct StepOutput { pub(super) value: TaskValue, charge: Charge, operation: Operation }

impl StepTask {
    pub(super) fn new(work: Work, operation: &Operation) -> Result<Self, TaskRejection> {
        let checked = operation.check().and_then(|()|
            operation.budget().reserve_owned(size_of::<Self>().max(size_of::<StepOutput>())).map_err(Into::into));
        match checked {
            Ok(charge) => Ok(Self { work, charge, operation: operation.clone() }),
            Err(error) => Err(TaskRejection { work, error }),
        }
    }
    pub(super) fn run(self) -> StepOutput {
        let Self { work, charge, operation } = self;
        #[cfg(test)]
        let mut gate = tests::take_step_gate(operation.cancel(), 0);
        #[cfg(test)]
        if let Some(gate) = &mut gate { gate.before(operation.budget()); }
        let value = match operation.check() {
            Err(error) => TaskValue::Rejected { work, error },
            Ok(()) => match work {
                Work::Build(snapshot) => {
                    let start = prepare::start_owned(&snapshot, operation.budget());
                    TaskValue::Built { snapshot, start }
                }
                Work::Start(core) => TaskValue::Core(core.start(operation.budget())),
                Work::Resume { core, reply } => TaskValue::Core(core.resume(reply, operation.budget())),
                Work::ResumeHttp { core, mut executor, reply } => {
                    let mut waiting = Some(core);
                    let mut progress = None;
                    let outer = executor.resume(reply, |reply| {
                        progress = Some(waiting.take().expect("one owned HTTP resume").resume(reply, operation.budget()));
                    });
                    let result = match progress {
                        Some(progress) => HttpResume::Resumed { progress, outer },
                        None => HttpResume::Rejected { core: waiting.unwrap(),
                            error: outer.err().unwrap_or(Failure::Invalid("preparation-http-callback")) },
                    };
                    TaskValue::Http { executor, result }
                }
                Work::Finalize { raw, snapshot, names } => {
                    let attempt = raw.finish(&snapshot, names);
                    TaskValue::Finalized { snapshot, attempt }
                }
            },
        };
        let output = StepOutput { value, charge, operation };
        #[cfg(test)]
        if let Some(gate) = &mut gate { gate.after(output.operation.budget()); }
        output
    }
}

struct PendingStep { handle: JoinHandle<StepOutput>, operation: Operation, settled: bool }
impl Drop for PendingStep {
    fn drop(&mut self) {
        if !self.settled {
            self.operation.cancel().cancel_as(CancelOrigin::Owner);
            self.handle.abort();
        }
    }
}

pub(super) enum SettledStep {
    Finished(StepOutput),
    Stopped { output: Option<StepOutput>, error: OwnerError },
}

pub(super) async fn settle_step(handle: JoinHandle<StepOutput>, operation: &Operation) -> SettledStep {
    let mut pending = PendingStep { handle, operation: operation.clone(), settled: false };
    let joined = tokio::select! { biased;
        _ = operation.cancel().cancelled() => {
            pending.handle.abort();
            (&mut pending.handle).await
        }
        result = &mut pending.handle => result,
    };
    pending.settled = true;
    // A ready join never outranks hard cancellation, and a running closure has now actually settled.
    if let Err(error) = operation.check() {
        return SettledStep::Stopped { output: joined.ok(), error: error.into() };
    }
    match joined {
        Ok(output) => SettledStep::Finished(output),
        Err(_) => SettledStep::Stopped { output: None, error: OwnerError::Join },
    }
}

pub(super) struct Resources {
    executor: Option<Executor>,
    memory: Option<MemoryConfig>,
    transport: TransportLimits,
    stop: Option<Cancellation>,
    charge: Charge,
    operation: Operation,
}

impl Resources {
    pub(super) fn new(operation: Operation, limits: http::Limits, transport: TransportLimits, memory: Option<MemoryConfig>,
        stop: Option<Cancellation>) -> Result<Self, Failure> {
        let charge = Self::reserve(&operation)?;
        let executor = Executor::new(operation.clone(), limits)?;
        Ok(Self { executor: Some(executor), memory, transport, stop, charge, operation })
    }
    pub(super) fn no_memory(operation: Operation, transport: TransportLimits) -> Result<Self, Failure> {
        let charge = Self::reserve(&operation)?;
        Ok(Self { executor: None, memory: None, transport, stop: None, charge, operation })
    }
    fn reserve(operation: &Operation) -> Result<Charge, Failure> {
        let bytes = size_of::<Self>().checked_add(size_of::<Work>().max(size_of::<CompletionValue>()))
            .and_then(|bytes| bytes.checked_add(size_of::<(PreparationSnapshot, Names)>()))
            .ok_or_else(http::limit)?;
        Ok(operation.budget().reserve_owned(bytes)?)
    }
}

struct Abandonment { operation: Operation, armed: bool }
impl Drop for Abandonment {
    fn drop(&mut self) { if self.armed { self.operation.cancel().cancel_as(CancelOrigin::Owner); } }
}

pub(super) enum FailureState {
    Task(StepOutput),
    Core(MachineCore),
    Progress(CoreYield),
    Work(Work),
    Prepared { snapshot: PreparationSnapshot, attempt: PreparationAttempt },
    Raw(RawMachineCompletion),
    None,
}

pub(super) enum CompletionValue {
    Prepared { snapshot: PreparationSnapshot, attempt: PreparationAttempt },
    Raw(RawMachineCompletion),
    Failed { error: OwnerError, state: FailureState },
}

pub(super) struct Completion {
    pub(super) value: CompletionValue,
    charge: Charge,
    pub(super) operation: Operation,
}

pub(super) struct PreparationRetention {
    _charge: Charge,
    operation: Operation,
}

impl PreparationRetention {
    #[cfg(test)]
    pub(super) fn budget(&self) -> Budget { self.operation.budget().clone() }
}

impl Completion {
    pub(super) fn into_parts(self) -> (CompletionValue, PreparationRetention) {
        let Self { value, charge, operation } = self;
        (value, PreparationRetention { _charge: charge, operation })
    }
    fn failed(error: OwnerError, state: FailureState, resources: Resources) -> Self {
        resources.operation.cancel().cancel_as(CancelOrigin::Owner);
        let Resources { executor, memory, charge, operation, .. } = resources;
        drop(executor); drop(memory);
        Self { value: CompletionValue::Failed { error, state }, charge, operation }
    }
    fn finished(value: CompletionValue, resources: Resources) -> Self {
        if let Err(error) = resources.operation.check() {
            let state = match value {
                CompletionValue::Prepared { snapshot, attempt } => FailureState::Prepared { snapshot, attempt },
                CompletionValue::Raw(raw) => FailureState::Raw(raw),
                CompletionValue::Failed { state, .. } => state,
            };
            return Self::failed(error.into(), state, resources);
        }
        let Resources { executor, memory, charge, operation, .. } = resources;
        drop(executor); drop(memory);
        Self { value, charge, operation }
    }
}

async fn memory_call(request: &risunest_ir::stage::SubRequest, config: &MemoryConfig, transport: TransportLimits,
    operation: &Operation, stop: Option<&Cancellation>) -> Result<HostReply, OwnerError> {
    let config = config.borrowed(operation.budget())?;
    let controls = http::arc_bytes::<AtomicBool>()?.checked_add(http::arc_bytes::<AtomicU8>()?)
        .and_then(|bytes| bytes.checked_add(http::arc_bytes::<tokio::sync::Notify>().ok()?))
        .and_then(|bytes| bytes.checked_add(size_of::<MemoryRequestConfig<'_>>())).ok_or_else(http::limit)?;
    let _io_charge = operation.budget().reserve_owned(controls)?;
    let io = Cancellation::new();
    let pending = sub_request::execute(request, &config, transport, operation.budget(), &io);
    tokio::pin!(pending);
    let (mut hard, mut soft) = (false, false);
    loop {
        tokio::select! { biased;
            _ = operation.cancel().cancelled(), if !hard => { hard = true; io.cancel_as(CancelOrigin::Owner); }
            _ = async { match stop { Some(stop) => stop.cancelled().await, None => std::future::pending().await } }, if !soft => {
                soft = true; io.cancel_as(CancelOrigin::Caller);
            }
            reply = &mut pending => {
                operation.check()?;
                return reply.map_err(Into::into);
            }
        }
    }
}

pub(super) async fn prepare(snapshot: PreparationSnapshot, resources: Resources) -> Completion {
    drive(Work::Build(snapshot), resources).await
}

pub(super) async fn run_core(core: MachineCore, resources: Resources) -> Completion {
    drive(Work::Start(core), resources).await
}

async fn drive(mut work: Work, mut resources: Resources) -> Completion {
    let mut context = None;
    let mut abandonment = Abandonment { operation: resources.operation.clone(), armed: true };
    loop {
        #[cfg(test)]
        if let Some(mut gate) = tests::take_step_gate(resources.operation.cancel(), 1) { gate.admission(resources.operation.budget()).await; }
        let task = match StepTask::new(work, &resources.operation) {
            Ok(task) => task,
            Err(TaskRejection { work, error }) => return Completion::failed(error.into(), FailureState::Work(work), resources),
        };
        let handle = tokio::task::spawn_blocking(move || task.run());
        #[cfg(test)]
        if let Some(mut gate) = tests::take_step_gate(resources.operation.cancel(), 2) { gate.before(resources.operation.budget()); }
        let output = match settle_step(handle, &resources.operation).await {
            SettledStep::Finished(output) => output,
            SettledStep::Stopped { output, error } => return Completion::failed(error, output.map_or(FailureState::None, FailureState::Task), resources),
        };
        let StepOutput { value, charge, operation } = output;
        let progress = match value {
            TaskValue::Built { snapshot, start } => match start {
                Ok(prepare::OwnedStart { names, budget: _, progress }) => { context = Some((snapshot, names)); progress }
                Err(error) => {
                    abandonment.armed = false;
                    return Completion::finished(CompletionValue::Prepared { snapshot, attempt: PreparationAttempt {
                        result: Err(error), variables: Default::default() } }, resources);
                }
            },
            TaskValue::Core(progress) => progress,
            TaskValue::Http { executor, result } => {
                resources.executor = Some(executor);
                match result {
                    HttpResume::Rejected { core, error } => return Completion::failed(error.into(), FailureState::Core(core), resources),
                    HttpResume::Resumed { progress, outer: Ok(()) } => progress,
                    HttpResume::Resumed { progress, outer: Err(error) } => return Completion::failed(error.into(), FailureState::Progress(progress), resources),
                }
            }
            TaskValue::Finalized { snapshot, attempt } => {
                abandonment.armed = false;
                return Completion::finished(CompletionValue::Prepared { snapshot, attempt }, resources);
            }
            TaskValue::Rejected { work, error } => {
                return Completion::failed(error.into(), FailureState::Work(work), resources);
            }
        };
        match progress {
            CoreYield::Done(raw) => {
                drop(charge); drop(operation);
                if let Some((snapshot, names)) = context.take() {
                    work = Work::Finalize { raw, snapshot, names };
                    continue;
                }
                abandonment.armed = false;
                return Completion::finished(CompletionValue::Raw(raw), resources);
            }
            CoreYield::NeedsHost { core, request } => {
                // The operation is sequential: no machine task runs while this one host request is pending.
                if let Err(error) = resources.operation.check() {
                    return Completion::failed(error.into(), FailureState::Core(core), resources);
                }
                match request {
                    HostRequest::Http(request) if resources.executor.is_some() => {
                        let mut executor = resources.executor.take().unwrap();
                        let reply = executor.execute(request, resources.stop.as_ref()).await;
                        match reply {
                            Ok(reply) => work = Work::ResumeHttp { core, executor, reply },
                            Err(error) => { drop(executor); return Completion::failed(error.into(), FailureState::Core(core), resources); }
                        }
                    }
                    HostRequest::Llm(request) if resources.memory.is_some() => {
                        let reply = memory_call(&request, resources.memory.as_ref().unwrap(), resources.transport,
                            &resources.operation, resources.stop.as_ref()).await;
                        match reply {
                            Ok(reply) => work = Work::Resume { core, reply },
                            Err(error) => return Completion::failed(error, FailureState::Core(core), resources),
                        }
                    }
                    request => work = Work::Resume { core, reply: HostReply::Unsupported(Cow::Borrowed(prepare::host_refusal(&request))) },
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "preparation_owner_tests.rs"]
pub(super) mod tests;
