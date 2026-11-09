//! Google family: `LLMFormat.VertexAIGemini` and `GoogleCloud` (`src/ts/process/request/google.ts`).

pub mod request;
pub mod response;
pub mod settings;
pub mod stream;
pub mod token;

#[cfg(test)]
mod replay;

pub use crate::common::BuildError;
