//! The checked-in `@dqbd/tiktoken` corpus (`benchmarks/tokenizer/native-tokenizer-corpus.json`) that the app's native
//! tokenizer command replays, here against the core encoders and the `TokenCounter` stage.

use risunest_ir::stage::{StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{Budget, Limits, Text};
use risunest_tokenizer::counter::encode::Encoder;
use risunest_tokenizer::counter::{ChatAccounting, Counter};
use risunest_tokenizer::id::TokenizerId;
use risunest_tokenizer::tiktoken::{DisallowedSpecial, Encoding, Refusal};
use serde::Deserialize;
use std::sync::{atomic::AtomicBool, Arc};

const CORPUS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../benchmarks/tokenizer/native-tokenizer-corpus.json"));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Corpus { artifacts: Artifacts, cases: Vec<CorpusCase> }

#[derive(Deserialize)]
struct Artifacts { cl100k_base: Artifact, o200k_base: Artifact }

#[derive(Deserialize)]
struct Artifact { fingerprint: String }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CorpusCase { name: String, tokenizer_id: TokenizerId, input: CorpusInput, ids: Option<Vec<u32>>, error: Option<CorpusError> }

#[derive(Deserialize)]
struct CorpusError { code: String, token: String }

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
enum CorpusInput {
    Text { value: String },
    Utf16CodeUnits { code_units: Vec<u16> },
    Repeat { value: String, count: usize },
}

impl CorpusInput {
    fn units(&self) -> Vec<u16> {
        match self {
            Self::Text { value } => value.encode_utf16().collect(),
            Self::Utf16CodeUnits { code_units } => code_units.clone(),
            Self::Repeat { value, count } => value.repeat(*count).encode_utf16().collect(),
        }
    }
    // The string the WASM encoder receives: isolated surrogates arrive as U+FFFD.
    fn resolve(&self) -> String { char::decode_utf16(self.units()).map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)).collect() }
}

fn corpus() -> Corpus { serde_json::from_str(CORPUS).expect("checked-in tokenizer corpus should parse") }
fn budget() -> Budget {
    let limits = Limits { max_owned_bytes: 256 << 20, max_json_bytes: 16 << 20, max_text_units: 16 << 20, max_values: 1 << 20,
        max_depth: 64, max_work: 1 << 40 };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
}
fn encoding(id: TokenizerId) -> Encoding { id.encoding().expect("corpus tokenizers are built in") }

#[test]
fn corpus_fingerprints_name_the_core_encoders() {
    let corpus = corpus();
    assert_eq!(TokenizerId::Cl100kBase.fingerprint(), Some(corpus.artifacts.cl100k_base.fingerprint.as_str()));
    assert_eq!(TokenizerId::O200kBase.fingerprint(), Some(corpus.artifacts.o200k_base.fingerprint.as_str()));
    assert_eq!(TokenizerId::Plugin.fingerprint(), None);
}

#[test]
fn successful_corpus_entries_have_exact_ids_and_counter_counts() {
    let corpus = corpus();
    let budget = budget();
    let mut checked = 0;
    for entry in &corpus.cases {
        let Some(expected) = &entry.ids else { continue };
        let source = entry.input.resolve();
        assert_eq!(&encoding(entry.tokenizer_id).encode(&source).unwrap(), expected, "{} IDs differed", entry.name);
        assert_eq!(encoding(entry.tokenizer_id).count(&source).unwrap(), expected.len(), "{} count differed", entry.name);
        let text = Text::from_units(&entry.input.units(), &budget).unwrap();
        let mut counter = Counter::new(entry.tokenizer_id, ChatAccounting::default());
        match counter.count(TokenInput::Text(&text), &budget).unwrap() {
            Step::Done(count) => assert_eq!(count as usize, expected.len(), "{} counter differed", entry.name),
            Step::NeedsHost(request) => panic!("{} asked the host: {request:?}", entry.name),
        }
        checked += 1;
    }
    println!("tokenizer corpus: {checked} ID cases, 0 mismatches");
    assert_eq!(checked, 68);
}

#[test]
fn special_token_errors_match_the_corpus() {
    let corpus = corpus();
    let budget = budget();
    let mut checked = 0;
    for entry in &corpus.cases {
        let Some(expected) = &entry.error else { continue };
        assert_eq!(expected.code, "disallowed_special_token", "{}", entry.name);
        let source = entry.input.resolve();
        let Refusal::DisallowedSpecial(DisallowedSpecial { token }) = encoding(entry.tokenizer_id).encode(&source).unwrap_err()
            else { panic!("{} was declined", entry.name) };
        assert_eq!(token, expected.token, "{}", entry.name);
        assert_eq!(encoding(entry.tokenizer_id).count(&source).unwrap_err(), Refusal::DisallowedSpecial(DisallowedSpecial { token }),
            "{}", entry.name);
        let text = Text::from_str(&source, &budget).unwrap();
        let refused = Counter::new(entry.tokenizer_id, ChatAccounting::default()).count(TokenInput::Text(&text), &budget).unwrap_err();
        assert_eq!(refused, StageError::Invalid("tokenizer-special-token".into()), "{}", entry.name);
        checked += 1;
    }
    assert_eq!(checked, 14);
}

#[test]
fn counts_preserve_empty_and_duplicate_texts() {
    assert_eq!(Encoding::Cl100kBase.encode("hello").unwrap(), vec![15339]);
    let counts = ["", "hello", "hello"].map(|text| Encoding::O200kBase.count(text).unwrap());
    assert_eq!(counts, [0, 1, 1]);
    assert_eq!(["hello", "안녕하세요"].map(|text| Encoding::Cl100kBase.count(text).unwrap()), [1, 5]);
}

// tiktoken-rs panics on a whitespace run its regex engine cannot finish (from 999,999 code points), so the encoders and
// the stage encode runs up to 900,000 and decline longer ones. tiktoken-rs 0.12.1 encodes 900,000 spaces and an `x` as
// 7,033 tokens under both encodings.
#[test]
fn whitespace_runs_past_the_regex_bound_decline_before_encoding() {
    let budget = budget();
    let limit = StageError::Unsupported("tokenizer-regex-limit".into());
    let refusal = Refusal::Declined("tokenizer-regex-limit");
    for id in [TokenizerId::Cl100kBase, TokenizerId::O200kBase] {
        for (run, declined) in [(900_000, false), (900_001, true)] {
            let source = format!("{}x", " ".repeat(run));
            let text = Text::from_str(&source, &budget).unwrap();
            let encoded = Encoder::new(id).encode(&text, &budget);
            let counted = Counter::new(id, ChatAccounting::default()).count(TokenInput::Text(&text), &budget);
            if declined {
                assert_eq!((encoded.unwrap_err(), counted.unwrap_err()), (limit.clone(), limit.clone()), "{id:?}");
                assert_eq!((encoding(id).screen(&source), encoding(id).encode(&source), encoding(id).count(&source)),
                    (Err(refusal), Err(refusal), Err(refusal)), "{id:?}");
                continue;
            }
            assert_eq!(encoding(id).screen(&source), Ok(()), "{id:?}");
            let Ok(Step::Done(encoded)) = encoded else { panic!("{id:?}: {encoded:?}") };
            assert_eq!(encoded.len(), 7_033, "{id:?}");
            assert!(matches!(counted, Ok(Step::Done(7_033))), "{id:?}: {counted:?}");
        }
    }
}
