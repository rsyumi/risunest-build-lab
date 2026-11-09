//! Buffered remote embeddings over the existing HTTP host protocol.
mod request;
mod response;

use risunest_ir::stage::{HostReply, Step};
use risunest_ir::{Budget, Charge, Error, OwnedVec, Text};
use std::fmt;

/// Custom endpoints are the effective, trimmed instance snapshot, as produced by capture.
/// The per-call custom model and credentials belong to CapturedConfig.
pub enum RemoteTarget { Ada, OpenAi3Small, OpenAi3Large, Custom { endpoint: Text } }
impl RemoteTarget {
    /// Capture the instance model and its effective custom URL, independently of per-call settings.
    pub fn capture(model: &Text, configured_model: Option<&Text>, override_url: Option<&Text>, configured_url: Option<&Text>, budget: &Budget) -> Result<Self, Failure> {
        budget.work(model.len_units())?;
        let model = if model.equals_ascii(b"auto") {
            configured_model.filter(|value| value.len_units() > 0).ok_or(Failure::Unsupported("embedding-target"))?
        } else { model };
        budget.work(model.len_units())?;
        Ok(if model.equals_ascii(b"ada") { Self::Ada }
        else if model.equals_ascii(b"openai3small") { Self::OpenAi3Small }
        else if model.equals_ascii(b"openai3large") { Self::OpenAi3Large }
        else if model.equals_ascii(b"custom") {
            let explicit = override_url.map(|value| value.trim(budget)).transpose()?;
            let endpoint = match explicit {
                Some(value) if value.len_units() > 0 => value,
                _ => match configured_url { Some(value) => value.trim(budget)?, None => Text::from_str("", budget)? },
            };
            Self::Custom { endpoint }
        } else { return Err(Failure::Unsupported("embedding-target")); })
    }
}
impl fmt::Debug for RemoteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Ada => "Ada", Self::OpenAi3Small => "OpenAi3Small", Self::OpenAi3Large => "OpenAi3Large", Self::Custom { .. } => "Custom { endpoint: <redacted> }" })
    }
}

/// Absent and present-but-empty credentials have different OpenAI fallback behavior.
pub struct Credentials(Option<Text>);
impl Credentials {
    pub fn capture(value: Option<&Text>, budget: &Budget) -> Result<Self, Failure> { Ok(Self(value.map(|value| value.trim(budget)).transpose()?)) }
}
impl fmt::Debug for Credentials { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("Credentials(<redacted>)") } }

pub struct CapturedConfig {
    custom_model: Option<Text>,
    custom_key: Credentials,
    openai_override: Credentials,
    openai_fallback: Credentials,
}
impl CapturedConfig {
    /// Capture the current request settings. Cache identity does not supply these values.
    pub fn capture(custom_model: Option<&Text>, custom_key: Option<&Text>, openai_override: Option<&Text>, openai_fallback: Option<&Text>, budget: &Budget) -> Result<Self, Failure> {
        Ok(Self { custom_model: custom_model.map(|value| value.trim(budget)).transpose()?,
            custom_key: Credentials::capture(custom_key, budget)?, openai_override: Credentials::capture(openai_override, budget)?,
            openai_fallback: Credentials::capture(openai_fallback, budget)? })
    }
}
impl fmt::Debug for CapturedConfig { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("CapturedConfig(<redacted>)") } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose { Document, Query }
pub enum Input { Scalar(Text), Batch(OwnedVec<Text>) }
impl fmt::Debug for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self { Self::Scalar(_) => f.write_str("Scalar(<redacted>)"), Self::Batch(values) => f.debug_struct("Batch").field("length", &values.as_slice().len()).finish() }
    }
}
#[derive(Debug)]
pub struct Request { pub target: RemoteTarget, pub config: CapturedConfig, pub purpose: Purpose, pub input: Input }
pub type Output = OwnedVec<Option<OwnedVec<f64>>>;

pub enum Failure {
    Invalid(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
    Cancelled,
    Remote { status: u16, diagnostic: Text },
    Decode { diagnostic: Text },
    Transport { diagnostic: Text },
}
impl fmt::Debug for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(kind) => f.debug_tuple("Invalid").field(kind).finish(),
            Self::Unsupported(kind) => f.debug_tuple("Unsupported").field(kind).finish(),
            Self::Limit(kind) => f.debug_tuple("Limit").field(kind).finish(), Self::Cancelled => f.write_str("Cancelled"),
            Self::Remote { status, .. } => f.debug_struct("Remote").field("status", status).field("diagnostic", &"<redacted>").finish(),
            Self::Decode { .. } => f.write_str("Decode(<redacted>)"), Self::Transport { .. } => f.write_str("Transport(<redacted>)"),
        }
    }
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        match error {
            Error::Limit(kind) => Self::Limit(kind), Error::Allocation => Self::Limit("allocation"), Error::Cancelled => Self::Cancelled,
            Error::Syntax { .. } => Self::Invalid("json-syntax"), Error::Type { operation } => Self::Invalid(operation),
            Error::IsolatedUtf16 => Self::Invalid("isolated-utf16"), Error::DecoderFinished => Self::Invalid("decoder-finished"),
            Error::Invariant(kind) => Self::Invalid(kind),
        }
    }
}

enum State { Fresh, Pending(OwnedVec<Charge>), Terminal }
/// One instance owns one HTTP operation. Route its reply back to this same instance.
pub struct Service { state: State }
impl Default for Service { fn default() -> Self { Self::new() } }
impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service").field("state", &match self.state { State::Fresh => "fresh", State::Pending(_) => "pending", State::Terminal => "terminal" }).finish()
    }
}
impl Service {
    pub fn new() -> Self { Self { state: State::Fresh } }
    pub fn run(&mut self, request: Request, budget: &Budget) -> Result<Step<Output>, Failure> {
        let previous = std::mem::replace(&mut self.state, State::Terminal);
        if !matches!(previous, State::Fresh) { return Err(Failure::Invalid("embedding-run-state")); }
        budget.check()?;
        let (http, leases) = request::build(request, budget)?;
        budget.check()?;
        self.state = State::Pending(leases);
        Ok(Step::NeedsHost(risunest_ir::stage::HostRequest::Http(http)))
    }
    pub fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<Output>, Failure> {
        let previous = std::mem::replace(&mut self.state, State::Terminal);
        let State::Pending(_leases) = previous else { return Err(Failure::Invalid("embedding-resume-state")); };
        budget.check()?;
        let output = match reply {
            HostReply::Http { status, body } => response::decode(status, body, budget)?,
            HostReply::Failed { message } => return Err(Failure::Transport { diagnostic: message }),
            HostReply::Cancelled => return Err(Failure::Cancelled),
            HostReply::Unsupported(_) => return Err(Failure::Unsupported("embedding-http-host")),
            _ => return Err(Failure::Invalid("embedding-http-reply")),
        };
        budget.check()?;
        Ok(Step::Done(output))
    }
}
