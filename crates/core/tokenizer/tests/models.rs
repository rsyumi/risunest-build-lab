//! Model registration and the declines counters and encoders return before their first host request when the model
//! they read is missing. Only the registration test registers a model in this test binary (Llama), so every other
//! family stays absent here.

mod common;

use common::artifact_bytes;
use risunest_ir::stage::{StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{json, Budget, Limits, Text};
use risunest_tokenizer::counter::ban::{CaseTables, StrongBan};
use risunest_tokenizer::counter::encode::Encoder;
use risunest_tokenizer::counter::google::{GoogleCloud, GoogleCloudMemo};
use risunest_tokenizer::counter::{ChatAccounting, Counter};
use risunest_tokenizer::id::model::{is_registered, register};
use risunest_tokenizer::id::TokenizerId;
use std::borrow::Cow;
use std::sync::{atomic::AtomicBool, Arc};

const SENTENCEPIECE: [TokenizerId; 4] = [TokenizerId::Mistral, TokenizerId::Llama, TokenizerId::NovelAi, TokenizerId::NovelList];
const HUGGINGFACE: [TokenizerId; 8] = [
    TokenizerId::Claude, TokenizerId::Llama3, TokenizerId::Gemma, TokenizerId::Cohere, TokenizerId::DeepSeek,
    TokenizerId::DeepSeekV4, TokenizerId::Glm4, TokenizerId::Glm5,
];
// What a Hugging Face family returns while its model is missing: the decline of a build without the feature.
const HUGGINGFACE_ABSENT: &str = if cfg!(feature = "huggingface") { "tokenizer-model" } else { "tokenizer-huggingface" };

fn budget() -> Budget {
    let limits = Limits { max_owned_bytes: 64 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
        max_depth: 64, max_work: 1 << 32 };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
}
fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }
fn ids(step: Step<Vec<f64>>) -> Vec<f64> {
    match step { Step::Done(ids) => ids, Step::NeedsHost(request) => panic!("unexpected host request {request:?}") }
}

// Each call returns its error instead of a `Step::NeedsHost`, so no host request precedes it.
#[test]
fn tokenizers_without_their_model_decline_before_any_host_request() {
    let budget = budget();
    let text = Text::from_str("hello", &budget).unwrap();
    let absent = SENTENCEPIECE.into_iter().filter(|id| *id != TokenizerId::Llama).chain(HUGGINGFACE);
    for id in absent {
        let expected = unsupported(if HUGGINGFACE.contains(&id) { HUGGINGFACE_ABSENT } else { "tokenizer-model" });
        assert!(!is_registered(id), "{id:?}");
        assert_eq!(Counter::new(id, ChatAccounting::default()).count(TokenInput::Text(&text), &budget).err(), Some(expected.clone()), "{id:?}");
        assert_eq!(Encoder::new(id).encode(&text, &budget).err(), Some(expected.clone()), "{id:?}");
        // A composed stage passes the decline on from its first text.
        let bias = json::parse("{}", &budget).unwrap();
        assert_eq!(StrongBan::new(Encoder::new(id)).start(&text, bias, None, false, CaseTables::Native, &budget).err(), Some(expected), "{id:?}");
    }
    // Google Cloud falls back to Gemma after countTokens answers, so it declines before sending countTokens.
    let memo = GoogleCloudMemo::new();
    let google = || GoogleCloud { internal_id: Text::from_str("synthetic-gemma-fallback", &budget).unwrap(), access_token: None,
        memo: memo.clone() };
    let expected = unsupported(HUGGINGFACE_ABSENT);
    let counted = Counter::new(TokenizerId::GoogleCloud, ChatAccounting::default()).with_google_cloud(google())
        .count(TokenInput::Text(&text), &budget);
    assert_eq!(counted.err(), Some(expected.clone()));
    assert_eq!(Encoder::new(TokenizerId::GoogleCloud).with_google_cloud(google()).encode(&text, &budget).err(), Some(expected));
}

#[test]
fn registration_verifies_the_artifact_and_keeps_the_first_model() {
    let budget = budget();
    let llama = artifact_bytes(TokenizerId::Llama);
    let mistral = artifact_bytes(TokenizerId::Mistral);
    let refused = Err(unsupported("tokenizer-artifact"));
    let text = Text::from_str("hello world", &budget).unwrap();
    let count = || Counter::new(TokenizerId::Llama, ChatAccounting::default()).count(TokenInput::Text(&text), &budget);
    // Another model's bytes, or the artifact cut short, are refused and leave the tokenizer declining.
    for bytes in [mistral.as_slice(), &llama[..llama.len() - 1]] {
        assert_eq!(register(TokenizerId::Llama, bytes), refused);
        assert!(!is_registered(TokenizerId::Llama));
        assert_eq!(count().err(), Some(unsupported("tokenizer-model")));
    }
    assert_eq!(register(TokenizerId::Llama, &llama), Ok(()));
    assert!(is_registered(TokenizerId::Llama));
    let encoded = ids(Encoder::new(TokenizerId::Llama).encode(&text, &budget).unwrap());
    assert!(!encoded.is_empty());
    assert!(matches!(count(), Ok(Step::Done(count)) if count as usize == encoded.len()));
    // A kept model still refuses other bytes, and the same artifact again changes nothing.
    assert_eq!(register(TokenizerId::Llama, &mistral), refused);
    assert_eq!(register(TokenizerId::Llama, &llama), Ok(()));
    assert_eq!(ids(Encoder::new(TokenizerId::Llama).encode(&text, &budget).unwrap()), encoded);
    assert!(!is_registered(TokenizerId::Mistral));
}

#[test]
fn only_tokenizers_with_a_model_artifact_register() {
    for id in [TokenizerId::Cl100kBase, TokenizerId::O200kBase, TokenizerId::Plugin, TokenizerId::PluginMissing, TokenizerId::Local] {
        assert_eq!(id.model(), None, "{id:?}");
        assert_eq!(register(id, b""), Err(StageError::Invalid(Cow::Borrowed("tokenizer-model-id"))), "{id:?}");
    }
    // Google Cloud reads Gemma's model, which the owner registers under Gemma.
    assert_eq!(TokenizerId::GoogleCloud.model(), Some(TokenizerId::Gemma));
    assert_eq!(register(TokenizerId::GoogleCloud, b""), Err(StageError::Invalid(Cow::Borrowed("tokenizer-model-id"))));
    for id in SENTENCEPIECE.into_iter().chain(HUGGINGFACE) { assert_eq!(id.model(), Some(id), "{id:?}"); }
    // A build without `huggingface` refuses those models before reading the bytes.
    let empty = if cfg!(feature = "huggingface") { "tokenizer-artifact" } else { "tokenizer-huggingface" };
    for id in HUGGINGFACE {
        assert_eq!(register(id, b""), Err(unsupported(empty)), "{id:?}");
        assert!(!is_registered(id), "{id:?}");
    }
}
