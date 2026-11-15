use super::{
    ollama_decode::Cancellation,
    ollama_transport::{self as transport, BufferedOllamaRequest, CapturedOllamaConfig, Failure, Stage, Think, ThinkLevel, TransportLimits},
    prepare,
};
use risunest_ir::{chat::Role, provider::{FailureCategory, FailureStage, OutputShape, ProviderFailure, ProviderOutput},
    stage::{HostReply, StageError, SubRequest}, Budget, Error, OwnedVec, Text, Value};
use std::borrow::Cow;

// Resolved from the admitted memory model and the send's captured effective user identity.
pub(crate) struct MemoryRequestConfig<'a> {
    pub model_id: &'a str,
    pub user_name: Text,
    pub ollama: CapturedOllamaConfig<'a>,
}

fn property(object: &mut OwnedVec<(Text, Value)>, key: &str, value: Value, budget: &Budget) -> Result<(), StageError> {
    object.push((Text::from_str(key, budget)?, value))?;
    Ok(())
}
fn body(request: &SubRequest, config: &MemoryRequestConfig<'_>, budget: &Budget) -> Result<Value, StageError> {
    let mut messages = OwnedVec::new(budget)?;
    for message in &request.messages {
        budget.work(1)?;
        let role = match message.role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => continue };
        let mut wire = OwnedVec::new(budget)?;
        property(&mut wire, "role", Value::Text(Text::from_str(role, budget)?), budget)?;
        property(&mut wire, "content", Value::Text(prepare::unescape_text(&message.content, budget)?), budget)?;
        messages.push(Value::Object(wire))?;
    }
    let mut body = OwnedVec::new(budget)?;
    property(&mut body, "model", Value::Text(Text::from_str(config.ollama.model, budget)?), budget)?;
    property(&mut body, "messages", Value::Array(messages), budget)?;
    property(&mut body, "stream", Value::Bool(false), budget)?;
    if let Some(think) = config.ollama.think {
        let value = match think {
            Think::Boolean(value) => Value::Bool(value),
            Think::Level(level) => Value::Text(Text::from_str(match level { ThinkLevel::Low => "low", ThinkLevel::Medium => "medium", ThinkLevel::High => "high" }, budget)?),
        };
        property(&mut body, "think", value, budget)?;
    }
    Ok(Value::Object(body))
}

fn returned_cancelled(message: Text) -> HostReply {
    HostReply::Llm(Err(ProviderFailure { category: FailureCategory::Cancelled, stage: Some(FailureStage::Request),
        sdk_message: Some(message), http_status: None, body_failure: None }))
}

pub(super) fn completed(value: Value, cancel: &Cancellation, aborted: Text) -> Result<HostReply, StageError> {
    // requestChatData checks the signal again only after the provider has returned successfully.
    if cancel.is_cancelled() { return Ok(returned_cancelled(aborted)); }
    let Value::Text(text) = value else { return Err(StageError::Invalid(Cow::Borrowed("memory-output"))) };
    Ok(HostReply::Llm(Ok(ProviderOutput { texts: vec![text], shape: OutputShape::Text,
        thoughts: Vec::new(), tool_calls: Vec::new(), usage: None, finish: None })))
}

fn failure(failure: Failure, aborted: Text, budget: &Budget) -> Result<HostReply, StageError> {
    let message = match failure {
        Failure::Cancelled => aborted,
        Failure::Http { message, .. } | Failure::Sdk { message, .. } => message,
        Failure::Compat { error: Error::Syntax { byte }, .. } => Text::from_str(&format!("Invalid JSON at byte {byte}"), budget)?,
        Failure::Compat { stage: Stage::Headers | Stage::Transform, error: Error::Type { operation: "object-to-primitive" } } =>
            Text::from_str("Cannot convert object to primitive value", budget)?,
        Failure::Compat { error, .. } => return Err(error.into()),
        Failure::Limit(kind) => return Err(StageError::Limit(Cow::Borrowed(kind))),
        Failure::Allocation => return Err(Error::Allocation.into()),
        Failure::Network { .. } => Text::from_str("Failed to fetch", budget)?,
        Failure::Type { operation, .. } => Text::from_str(&format!("Invalid Ollama response ({operation})"), budget)?,
        Failure::Decode(_) => Text::from_str("Response decoding failed", budget)?,
        Failure::InvalidHost => Text::from_str("Invalid URL", budget)?,
        Failure::Userinfo => Text::from_str("Request URL includes credentials", budget)?,
        Failure::ConsumerCancelled | Failure::Consumer { .. } | Failure::DeliveryUnknown =>
            return Err(StageError::Invalid(Cow::Borrowed("memory-delivery"))),
    };
    Ok(HostReply::Failed { message })
}

// The operation budget belongs to the host; caller cancellation is a separate dependency outcome.
pub(crate) async fn execute(request: &SubRequest, config: &MemoryRequestConfig<'_>, limits: TransportLimits,
    budget: &Budget, cancel: &Cancellation) -> Result<HostReply, StageError> {
    if !request.mode.equals_ascii(b"memory") || config.model_id != "ollama-hosted" {
        return Ok(HostReply::Unsupported(Cow::Borrowed("memory-model")));
    }
    let returned_abort = Text::from_str("Aborted", budget)?;
    if cancel.is_cancelled() { return Ok(returned_cancelled(returned_abort)); }
    let thrown_abort = Text::from_str("This operation was aborted", budget)?;
    let body = body(request, config, budget)?;
    let wire = BufferedOllamaRequest { body: &body, messages: &request.messages, user_name: &config.user_name };
    match transport::buffered(wire, &config.ollama, limits, budget, cancel).await {
        Ok(value) => completed(value, cancel, returned_abort),
        Err(error) => failure(error, thrown_abort, budget),
    }
}
