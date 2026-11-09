//! Anthropic Messages family: `LLMFormat.Anthropic`, `AnthropicLegacy`, `NanoGPTMessages` and `AWSBedrockClaude`.
//!
//! Request building ([`request`]), buffered response parsing ([`response`]), SSE decoding ([`stream`]) and error mapping
//! ([`owner`]) follow `src/ts/process/request/anthropic.ts` byte for byte. Plugin replacers, the request trigger, MCP
//! tool execution, retries and transport run in the owner. Bedrock requests are signed here with the clock the caller
//! supplies. `claudeBatching` is the sans-IO machine in [`batch`]: [`request::build_request`] builds the request as for any other
//! call and [`batch::BatchSession`] turns it into the create, poll and result requests. The builders decline what they do not
//! model with [`BuildError::Unsupported`].

pub mod batch;
pub(crate) mod bedrock;
pub mod owner;
pub mod request;
pub mod response;
pub mod settings;
pub mod stream;
#[cfg(test)]
mod replay;

pub use crate::common::BuildError;
