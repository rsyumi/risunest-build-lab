//! Models from the repository's `public/` (the bundled frontend assets), registered the way an owner registers them,
//! and the declines of a tokenizer this build leaves out.
#![allow(dead_code)]

use risunest_ir::stage::{StageError, TokenCounter, TokenInput};
use risunest_ir::{Budget, Text};
use risunest_tokenizer::counter::encode::Encoder;
use risunest_tokenizer::counter::{ChatAccounting, Counter};
use risunest_tokenizer::id::model::{is_registered, register};
use risunest_tokenizer::id::TokenizerId;

/// The bytes of the artifact `id` loads, as the repository stores them.
pub fn artifact_bytes(id: TokenizerId) -> Vec<u8> {
    let path = id.artifact().expect("a tokenizer with an artifact").path;
    std::fs::read(format!("{}/../../../public{path}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Registers the model `id` reads, unless this build leaves `id` out, and returns `id`.
pub fn ready(id: TokenizerId) -> TokenizerId {
    if let (None, Some(model)) = (id.decline(), id.model()) {
        if !is_registered(model) { register(model, &artifact_bytes(model)).unwrap(); }
    }
    id
}

/// The decline of a tokenizer this build leaves out; every build declines `tokenizer-local`.
pub fn left_out(id: TokenizerId) -> Option<&'static str> { id.decline().filter(|name| *name != "tokenizer-local") }

/// A tokenizer this build leaves out declines in the encoder and the counter before any host request, so the owner
/// runs TS for the whole operation instead.
pub fn declines(id: TokenizerId, text: &Text, budget: &Budget) -> Result<(), String> {
    declines_as(id, text, budget, left_out(id).ok_or_else(|| format!("{id:?} is in this build"))?)
}

/// The encoder and the counter of `id` both decline `text` as `decline`, before any host request.
pub fn declines_as(id: TokenizerId, text: &Text, budget: &Budget, decline: &str) -> Result<(), String> {
    let encoded = Encoder::new(id).encode(text, budget);
    let counted = Counter::new(id, ChatAccounting::default()).count(TokenInput::Text(text), budget);
    match (encoded, counted) {
        (Err(StageError::Unsupported(encoded)), Err(StageError::Unsupported(counted))) if encoded == decline && counted == decline => Ok(()),
        (encoded, counted) => Err(format!("{id:?}: {encoded:?} and {counted:?} where this build declines {decline}")),
    }
}
