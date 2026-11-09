//! The SentencePiece and Hugging Face models counters and encoders read. The owner registers each model from the
//! artifact bytes it bundles ([`super::Artifact::path`], under the frontend assets); a family's model is built once and
//! kept for the process. An owner that keeps only some models loaded builds an [`OwnedModel`] instead and drops it when
//! done. The app's native tokenizer is such an owner and registers no model, so a [`Counter`](crate::counter::Counter)
//! or [`Encoder`](crate::counter::encode::Encoder) the app runs for a model family declines (`tokenizer-model`). Model
//! initialization is bounded by the pinned artifact, outside the per-operation budget.

#[cfg(feature = "huggingface")]
use super::huggingface::Processor as HuggingFace;
use super::sentencepiece::{self, processor::Processor as SentencePiece};
use super::{ArtifactFormat, TokenizerId};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text};
use std::borrow::Cow;
use std::sync::OnceLock;

pub(crate) enum Model {
    SentencePiece(SentencePiece),
    #[cfg(feature = "huggingface")]
    HuggingFace(HuggingFace),
}

impl Model {
    pub(crate) fn encode(&self, text: &Text, budget: &Budget) -> Result<Vec<u32>, StageError> {
        match self {
            Self::SentencePiece(processor) => processor.encode(text, budget),
            #[cfg(feature = "huggingface")]
            Self::HuggingFace(processor) => processor.encode(text, budget),
        }
    }
}

// The tokenizers with a model, one slot each.
const FAMILIES: [TokenizerId; 12] = [
    TokenizerId::Mistral, TokenizerId::Llama, TokenizerId::NovelAi, TokenizerId::NovelList, TokenizerId::Claude,
    TokenizerId::Llama3, TokenizerId::Gemma, TokenizerId::Cohere, TokenizerId::DeepSeek, TokenizerId::DeepSeekV4,
    TokenizerId::Glm4, TokenizerId::Glm5,
];
static MODELS: [OnceLock<Model>; 12] = [const { OnceLock::new() }; 12];

fn slot(id: TokenizerId) -> Option<&'static OnceLock<Model>> {
    FAMILIES.iter().position(|family| *family == id).map(|index| &MODELS[index])
}

/// Builds the model of `id` from its artifact bytes and keeps it for every later count. Bytes other than the recorded
/// artifact are `Unsupported("tokenizer-artifact")` and leave the slot empty, so the owner can register again; once a
/// model is kept, registering the same artifact again does nothing. A tokenizer this build leaves out returns its
/// [`TokenizerId::decline`], and one without a model (see [`TokenizerId::model`]) is `Invalid("tokenizer-model-id")`.
pub fn register(id: TokenizerId, bytes: &[u8]) -> Result<(), StageError> {
    let (slot, format) = verified(id, bytes)?;
    if slot.get().is_some() { return Ok(()); }
    let model = build(id, format, bytes)?;
    // A concurrent registration of the same verified artifact may have won; its model is the same.
    let _ = slot.set(model);
    Ok(())
}

/// A family's model that its owner keeps, rather than the process: dropping it frees the model.
pub struct OwnedModel(Model);

impl OwnedModel {
    /// Builds the model of `id` from its artifact bytes, refusing what [`register`] refuses.
    pub fn new(id: TokenizerId, bytes: &[u8]) -> Result<Self, StageError> {
        let (_, format) = verified(id, bytes)?;
        build(id, format, bytes).map(Self)
    }

    /// The IDs of `text`, as the family's encoder returns them.
    pub fn encode(&self, text: &Text, budget: &Budget) -> Result<Vec<u32>, StageError> { self.0.encode(text, budget) }
}

// The slot and artifact format of `id` once its bytes are the recorded artifact, refused as `register` documents.
fn verified(id: TokenizerId, bytes: &[u8]) -> Result<(&'static OnceLock<Model>, ArtifactFormat), StageError> {
    let (Some(slot), Some(artifact)) = (slot(id), id.artifact()) else {
        return Err(StageError::Invalid(Cow::Borrowed("tokenizer-model-id")));
    };
    if let Some(name) = id.decline() { return Err(StageError::Unsupported(Cow::Borrowed(name))); }
    artifact.verify(bytes)?;
    Ok((slot, artifact.format))
}

#[cfg_attr(not(feature = "huggingface"), allow(unused_variables))]
fn build(id: TokenizerId, format: ArtifactFormat, bytes: &[u8]) -> Result<Model, StageError> {
    Ok(match format {
        ArtifactFormat::SentencePiece => Model::SentencePiece(SentencePiece::new(&sentencepiece::parse(bytes)?)?),
        #[cfg(feature = "huggingface")]
        ArtifactFormat::HuggingFace => Model::HuggingFace(HuggingFace::load(id, bytes)?),
        _ => return Err(StageError::Invalid(Cow::Borrowed("tokenizer-model-id"))),
    })
}

/// Whether the model of `id` is registered.
pub fn is_registered(id: TokenizerId) -> bool { slot(id).is_some_and(|slot| slot.get().is_some()) }

/// The registered model of a family, or the decline for a missing one.
pub(crate) fn get(id: TokenizerId) -> Result<&'static Model, StageError> {
    slot(id).and_then(OnceLock::get).ok_or(StageError::Unsupported(Cow::Borrowed("tokenizer-model")))
}

/// What a counter or an encoder for `id` checks before its first host request: a tokenizer this build leaves out
/// declines, and so does one whose model is not registered. Google Cloud needs Gemma's model for its fallback.
pub(crate) fn admit(id: TokenizerId) -> Result<Option<&'static Model>, StageError> {
    if let Some(name) = id.decline() { return Err(StageError::Unsupported(Cow::Borrowed(name))); }
    id.model().map(get).transpose()
}

/// Registers the repository's model of `id` from `public/` and returns it.
#[cfg(all(test, feature = "huggingface"))]
pub(crate) fn repository(id: TokenizerId) -> &'static Model {
    let path = id.artifact().expect("a tokenizer with a model").path;
    let bytes = std::fs::read(format!("{}/../../../public{path}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    register(id, &bytes).unwrap();
    get(id).unwrap()
}
