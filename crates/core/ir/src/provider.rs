//! Provider calls, wire requests and their results, independent of any family.

use crate::chat::{MessageRole, PromptMessage};
use crate::text::Text;
use crate::value::Value;
use std::borrow::Cow;

/// `LLMFormat` (`src/ts/model/types.ts`); declaration order matches the TS numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LlmFormat {
    OpenAICompatible, OpenAILegacyInstruct, Anthropic, AnthropicLegacy, Mistral, GoogleCloud, VertexAIGemini, NovelList,
    Cohere, NovelAI, WebLLM, OobaLegacy, Plugin, Ooba, Kobold, Ollama, Horde, AWSBedrockClaude, OpenAIResponseAPI, Echo,
    NanoGPT, NanoGPTResponses, NanoGPTMessages, NanoGPTLegacy,
}

/// `LLMFlags` (`src/ts/model/types.ts`); declaration order matches the TS numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LlmFlag {
    HasImageInput, HasImageOutput, HasAudioInput, HasAudioOutput, HasPrefill, HasCache, HasFullSystemPrompt,
    HasFirstSystemPrompt, HasStreaming, RequiresAlternateRole, MustStartWithUserInput, PoolSupported, HasVideoInput,
    OAICompletionTokens, DeveloperRole, GeminiThinking, GeminiBlockOff, DeepSeekPrefix, DeepSeekThinkingInput,
    DeepSeekThinkingOutput, NoCivilIntegrity, ClaudeThinking, ClaudeAdaptiveThinking, ClaudeXHighEffort,
    DeepSeekThinkingToggle, NoStructuredOutput, GeminiThinkingNoMinimal, ClaudeThinkingOnByDefault,
}

/// The `LLMModel` fields request formatting reads.
#[derive(Debug, PartialEq)]
pub struct ModelDescriptor {
    pub id: Text,
    pub internal_id: Option<Text>,
    pub format: LlmFormat,
    pub flags: Vec<LlmFlag>,
    /// `LLMParameter` names the model accepts.
    pub parameters: Vec<Text>,
    pub endpoint: Option<Text>,
}

#[derive(Debug)]
pub struct ToolSpec {
    pub name: Text,
    pub description: Text,
    pub input_schema: Value,
}

/// One request before family formatting. Family settings such as keys and URLs stay with the family builder.
#[derive(Debug)]
pub struct ProviderCall {
    pub model: ModelDescriptor,
    pub messages: Vec<PromptMessage>,
    /// Sampling parameters by TS name, in insertion order.
    pub sampling: Vec<(Text, Value)>,
    /// `[text, bias]` pairs in TS order.
    pub bias: Vec<(Text, f64)>,
    pub tools: Vec<ToolSpec>,
    /// JSON schema for structured output.
    pub schema: Option<Value>,
    pub stream: bool,
    /// `ModelModeExtended` (`model`, `submodel`, `memory`, ...).
    pub mode: Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpMethod { Get, Post, Put, Patch, Delete }

/// How the transport reads the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResponseKind { Buffered, Sse, NdJson }

/// `Debug` shows the method, the header names, the body length and response kind. It hides the entire URL,
/// header values and body, since they can carry keys or prompt text.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequestSpec {
    pub method: HttpMethod,
    pub url: String,
    /// Names and values in send order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub response: ResponseKind,
}

impl std::fmt::Debug for HttpRequestSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        let headers: Vec<(&str, &str)> = self.headers.iter().map(|(name, _)| (name.as_str(), HIDDEN)).collect();
        formatter.debug_struct("HttpRequestSpec")
            .field("method", &self.method)
            .field("url", &HIDDEN)
            .field("headers", &headers)
            .field("body_bytes", &self.body.len())
            .field("response", &self.response)
            .finish()
    }
}

#[derive(Debug)]
pub struct ToolCall {
    pub id: Text,
    pub name: Text,
    pub arguments: Text,
}

/// Which `requestChatData` result type (`src/ts/process/request/request.ts`) a [`ProviderOutput`] stands for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OutputShape {
    /// `{ type: 'success' }`.
    #[default]
    Text,
    /// `{ type: 'streaming' }`, which a provider can return for a request without `useStreaming`: the one text is the
    /// stream as `collectStreamingText` collects it.
    Streamed,
    /// `{ type: 'multiline' }` (`request.ts:1125-1130`), whatever its pair count: the role of each `[role, text]` pair,
    /// in order, beside the text of each in `texts`.
    Multiline(Vec<MessageRole>),
}

#[derive(Debug)]
pub struct ProviderOutput {
    /// One entry per output; a multiline result carries the text of each `[role, text]` pair, in order.
    pub texts: Vec<Text>,
    pub shape: OutputShape,
    pub thoughts: Vec<Text>,
    pub tool_calls: Vec<ToolCall>,
    /// The provider's usage object as received.
    pub usage: Option<Value>,
    pub finish: Option<Text>,
}

/// The transport `Failure` kinds (`src-tauri/src/native_generation/ollama_transport.rs:51-66`). `Allocation` is
/// `Limit("allocation")`; `Type` keeps the failed operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FailureCategory {
    Cancelled,
    ConsumerCancelled,
    Limit(Cow<'static, str>),
    Compat,
    Decode,
    Network,
    Type(Cow<'static, str>),
    Sdk,
    Http,
    Consumer,
    DeliveryUnknown,
    InvalidHost,
    Userinfo,
}

/// The transport `Stage` (`src-tauri/src/native_generation/ollama_transport.rs:49`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureStage { Request, Headers, Body, BufferedJson, StreamJson, SdkIterator, Transform, Credit, Offer }

/// Why an HTTP error body could not be read, as the transport `BodyFailure` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyFailure { Reader, Codec, Json, Property }

/// Matches the route DTO `Failure`: no body, prompt, credential or host.
#[derive(Debug)]
pub struct ProviderFailure {
    pub category: FailureCategory,
    pub stage: Option<FailureStage>,
    pub http_status: Option<u16>,
    /// Bounded; the text TS shows for the same outcome.
    pub sdk_message: Option<Text>,
    /// Set for an `Http` failure whose error body could not be read.
    pub body_failure: Option<BodyFailure>,
}
