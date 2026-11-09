//! `LLMFormat.OpenAICompatible` and `LLMFormat.NanoGPT`.
//!
//! Request building ([`request`]), parameter and schema helpers, buffered response parsing, SSE decoding and error
//! mapping follow `src/ts/process/request/openAI/requests.ts` byte for byte. Plugin replacers
//! (`request.ts:248-300`), the request trigger, tokenization, MCP tool execution and transport run in the owner.

pub(crate) use crate::common::{js, wire};
pub mod owner;
pub mod params;
pub mod reformat;
pub mod request;
pub mod response;
pub mod responses_request;
pub mod responses_response;
pub mod responses_stream;
#[cfg(test)]
mod responses_replay;
pub use crate::common::schema;
pub mod settings;
pub mod stream;
#[cfg(test)]
mod replay;

pub use crate::common::BuildError;
