//! The Hugging Face `tokenizer.json` encoder behind [`super::model`], which verifies the artifact before loading it.

use super::sentencepiece::processor::emscripten_utf8;
use super::TokenizerId;
use crate::unicode::{self, Classes};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text};
use std::borrow::Cow;
use tokenizers::utils::SysRegex;
use tokenizers::{NormalizedString, Normalizer, Tokenizer};

/// The longest run a Hugging Face family's pattern matches with one repeated class. fancy-regex 0.14.0 keeps a stack
/// entry for each code point of such a match and, past 1,000,000, stops the search without an error, so tokenizers
/// keeps the rest of the text as one piece: letter, punctuation and emoji runs did so from 999,998 code points in every
/// family but Gemma. The bound keeps a tenfold margin.
pub(crate) const MAX_CLASS_RUN: usize = 100_000;

// The classes whose runs the family's pattern matches whole, as a pattern without lookaround, which fancy-regex hands to
// the linear regex engine: letters, numbers and the rest but whitespace in the GPT-2 pattern (Claude; Cohere's `Digits`
// splits numbers first), letters and the rest in the Llama 3 pattern (Llama 3, GLM), and letters with marks or
// punctuation with symbols in DeepSeek's.
fn run_classes(id: TokenizerId) -> Option<&'static str> {
    match id {
        TokenizerId::Claude => Some(r"\p{L}+|\p{N}+|[^\s\p{L}\p{N}]+"),
        TokenizerId::Cohere | TokenizerId::Llama3 | TokenizerId::Glm4 | TokenizerId::Glm5 => Some(r"\p{L}+|[^\s\p{L}\p{N}]+"),
        TokenizerId::DeepSeek | TokenizerId::DeepSeekV4 => Some(r"[\p{L}\p{M}]+|[\p{P}\p{S}]+"),
        _ => None,
    }
}

pub(crate) struct Processor {
    tokenizer: Tokenizer,
    // Every family but Gemma, which splits on a literal space, pre-tokenizes with Unicode category classes and
    // `\s+(?!\S)`, so its text passes `unicode::screen` and the run screen first.
    classes: Option<Classes>,
    runs: Option<SysRegex>,
    // Claude's NFKC and Cohere's NFC can lengthen a run, so their runs are measured after the normalizer.
    normalized: bool,
}

impl Processor {
    pub(crate) fn load(id: TokenizerId, bytes: &[u8]) -> Result<Self, StageError> {
        let tokenizer = Tokenizer::from_bytes(bytes).map_err(|_| StageError::Invalid(Cow::Borrowed("huggingface-model")))?;
        let classes = match id {
            TokenizerId::Gemma => None,
            TokenizerId::DeepSeek | TokenizerId::DeepSeekV4 => Some(Classes::Categories),
            _ => Some(Classes::LettersAndNumbers),
        };
        let runs = run_classes(id).map(|pattern| SysRegex::new(pattern).expect("run classes compile"));
        Ok(Self { tokenizer, classes, runs, normalized: matches!(id, TokenizerId::Claude | TokenizerId::Cohere) })
    }

    // Whether the family's pattern would match a run longer than `MAX_CLASS_RUN`.
    fn long_run(&self, source: &str, budget: &Budget) -> Result<bool, StageError> {
        let Some(runs) = &self.runs else { return Ok(false) };
        let mut normalized = None;
        if let Some(normalizer) = self.tokenizer.get_normalizer().filter(|_| self.normalized) {
            let mut text = NormalizedString::from(source);
            normalizer.normalize(&mut text).map_err(|_| StageError::Invalid(Cow::Borrowed("huggingface-normalize")))?;
            normalized = Some(text);
        }
        let text = normalized.as_ref().map_or(source, NormalizedString::get);
        budget.work(text.len())?;
        Ok(runs.find_iter(text).any(|(start, end)| end - start > MAX_CLASS_RUN && text[start..end].chars().count() > MAX_CLASS_RUN))
    }

    pub(crate) fn encode(&self, text: &Text, budget: &Budget) -> Result<Vec<u32>, StageError> {
        // web-tokenizers sends both JSON and SentencePiece encoders emscripten's UTF-8 conversion.
        let input = emscripten_utf8(text, budget)?;
        let source = std::str::from_utf8(input.as_slice()).map_err(|_| StageError::Invalid(Cow::Borrowed("emscripten-utf8")))?;
        budget.work(source.len())?;
        if let Some(classes) = self.classes {
            budget.work(source.len())?;
            unicode::screen(source, classes).map_err(|name| StageError::Unsupported(Cow::Borrowed(name)))?;
        }
        if self.long_run(source, budget)? {
            return Err(StageError::Unsupported(Cow::Borrowed("tokenizer-regex-limit")));
        }
        let encoded = self.tokenizer.encode_fast(source, false).map_err(|_| StageError::Invalid(Cow::Borrowed("huggingface-encode")))?;
        budget.work(encoded.len())?;
        let _charge = budget.reserve_owned(encoded.len().saturating_mul(std::mem::size_of::<u32>()))?;
        Ok(encoded.get_ids().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{register, repository};
    use super::super::TokenizerId;
    use super::MAX_CLASS_RUN;
    use risunest_ir::stage::StageError;
    use risunest_ir::{Budget, Limits, Text};
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget(owned: usize, work: usize, cancelled: bool) -> Budget {
        Budget::new(Limits { max_owned_bytes: owned, max_json_bytes: 1024, max_text_units: 1024,
            max_values: 1024, max_depth: 16, max_work: work }, Arc::new(AtomicBool::new(cancelled))).unwrap()
    }

    // Registration verifies the bytes before loading them, also once another test has kept the model.
    #[test]
    fn artifact_verification_precedes_huggingface_loading() {
        let artifacts = [TokenizerId::Claude, TokenizerId::DeepSeek, TokenizerId::DeepSeekV4, TokenizerId::Llama3, TokenizerId::Glm4,
            TokenizerId::Glm5, TokenizerId::Gemma, TokenizerId::Cohere]
            .map(|id| (id, std::fs::read(format!("{}/../../../public{}", env!("CARGO_MANIFEST_DIR"), id.artifact().unwrap().path)).unwrap()));
        for (id, bytes) in &artifacts {
            // Trailing JSON whitespace preserves the model but changes its pinned artifact identity.
            let changed = [bytes.as_slice(), b" "].concat();
            for altered in [changed.as_slice(), &bytes[..bytes.len() - 1]] {
                assert!(matches!(register(*id, altered),
                    Err(StageError::Unsupported(name)) if name == "tokenizer-artifact"), "{id:?}");
            }
            for (other, other_bytes) in &artifacts {
                if other == id { continue; }
                assert!(matches!(register(*id, other_bytes),
                    Err(StageError::Unsupported(name)) if name == "tokenizer-artifact"), "{id:?} accepted {other:?}");
            }
        }
    }

    #[test]
    fn alternating_huggingface_ids_preserve_added_token_identity() {
        let budget = budget(1 << 20, 1 << 20, false);
        let input = Text::from_str("<｜begin▁of▁repo▁name｜>", &budget).unwrap();
        let encode = |id| repository(id).encode(&input, &budget).unwrap();
        let deepseek = encode(TokenizerId::DeepSeek);
        let v4 = encode(TokenizerId::DeepSeekV4);
        let claude = encode(TokenizerId::Claude);
        assert_eq!(v4.len(), 1);
        assert_ne!(deepseek, v4);
        assert_ne!(claude, v4);
        for (id, expected) in [(TokenizerId::DeepSeekV4, &v4), (TokenizerId::DeepSeek, &deepseek),
            (TokenizerId::Claude, &claude), (TokenizerId::DeepSeek, &deepseek), (TokenizerId::DeepSeekV4, &v4)] {
            assert_eq!(&encode(id), expected, "{id:?}");
        }
        let input = Text::from_str("[gMASK]", &budget).unwrap();
        let encode = |id| repository(id).encode(&input, &budget).unwrap();
        let glm4 = encode(TokenizerId::Glm4);
        let glm5 = encode(TokenizerId::Glm5);
        let llama3 = encode(TokenizerId::Llama3);
        assert_eq!((glm4.len(), glm5.len()), (1, 1));
        assert_ne!(glm4, glm5);
        assert_ne!(llama3, glm4);
        assert_ne!(llama3, glm5);
        for (id, expected) in [(TokenizerId::Glm5, &glm5), (TokenizerId::Llama3, &llama3),
            (TokenizerId::Glm4, &glm4), (TokenizerId::Llama3, &llama3), (TokenizerId::Glm5, &glm5)] {
            assert_eq!(&encode(id), expected, "{id:?}");
        }
        let input = Text::from_str("<|turn>", &budget).unwrap();
        let encode = |id| repository(id).encode(&input, &budget).unwrap();
        let gemma = encode(TokenizerId::Gemma);
        let cohere = encode(TokenizerId::Cohere);
        let claude = encode(TokenizerId::Claude);
        assert_eq!(gemma.len(), 1);
        assert_ne!(gemma, cohere);
        for (id, expected) in [(TokenizerId::Cohere, &cohere), (TokenizerId::Gemma, &gemma),
            (TokenizerId::Claude, &claude), (TokenizerId::Gemma, &gemma), (TokenizerId::Cohere, &cohere)] {
            assert_eq!(&encode(id), expected, "{id:?}");
        }
        let empty = Text::from_str("", &budget).unwrap();
        for id in [TokenizerId::Gemma, TokenizerId::Cohere] {
            assert!(repository(id).encode(&empty, &budget).unwrap().is_empty(), "{id:?}");
        }
        let decomposed = Text::from_str("cafe\u{301}", &budget).unwrap();
        let composed = Text::from_str("café", &budget).unwrap();
        let normalized = |id, text: &Text| repository(id).encode(text, &budget).unwrap();
        assert_eq!(normalized(TokenizerId::Cohere, &decomposed), normalized(TokenizerId::Cohere, &composed));
        assert_ne!(normalized(TokenizerId::Gemma, &decomposed), normalized(TokenizerId::Gemma, &composed));
    }

    #[test]
    fn encoding_checks_work_memory_and_cancellation_budgets() {
        let rich = budget(1 << 20, 1 << 20, false);
        let input = Text::from_str("!!!!!!!!!!!!!!!!!!!!!!!!!", &rich).unwrap();
        for id in [TokenizerId::Claude, TokenizerId::Gemma, TokenizerId::Cohere] {
            let processor = repository(id);
            let expected = processor.encode(&input, &rich).unwrap();
            assert!(!expected.is_empty());
            assert_eq!(processor.encode(&input, &budget(1 << 20, 25, false)).unwrap_err(), StageError::Limit("work".into()));
            // The 25-byte input occupies a 32-byte OwnedVec, leaving no room for returned IDs.
            assert_eq!(processor.encode(&input, &budget(Budget::context_bytes() + 32, 1 << 20, false)).unwrap_err(),
                StageError::Limit("owned-bytes".into()));
            assert_eq!(processor.encode(&input, &budget(1 << 20, 1 << 20, true)).unwrap_err(), StageError::Cancelled);
            assert_eq!(processor.encode(&input, &rich).unwrap(), expected);
        }
    }

    // A run of each class a family's pattern matches whole is encoded up to `MAX_CLASS_RUN` code points after the
    // family's normalizer and declined past it: U+FB00 is two letters after Claude's NFKC and U+0344 two marks after
    // Cohere's NFC, so their runs are declined at half the bound. A run the pattern splits is not declined.
    #[test]
    fn class_runs_past_the_bound_are_declined() {
        let budget = Budget::new(Limits { max_owned_bytes: 1 << 28, max_json_bytes: 1024, max_text_units: 1 << 22,
            max_values: 1024, max_depth: 16, max_work: usize::MAX / 4 }, Arc::new(AtomicBool::new(false))).unwrap();
        let encode = |id, unit: &str, count| repository(id).encode(&Text::from_str(&unit.repeat(count), &budget).unwrap(), &budget);
        let llama3: &[(&str, usize)] = &[("a", 1), ("!", 1)];
        let deepseek: &[(&str, usize)] = &[("a", 1), ("e\u{301}", 2), ("!", 1)];
        let runs: [(TokenizerId, &[(&str, usize)]); 7] = [
            (TokenizerId::Claude, &[("a", 1), ("1", 1), ("!", 1), ("\u{fb00}", 2)]),
            (TokenizerId::Cohere, &[("a", 1), ("!", 1), ("\u{344}", 2)]),
            (TokenizerId::Llama3, llama3), (TokenizerId::Glm4, llama3), (TokenizerId::Glm5, llama3),
            (TokenizerId::DeepSeek, deepseek), (TokenizerId::DeepSeekV4, deepseek),
        ];
        for (id, runs) in runs {
            for &(unit, width) in runs {
                assert!(encode(id, unit, MAX_CLASS_RUN / width).is_ok(), "{id:?} {unit:?}");
                assert_eq!(encode(id, unit, MAX_CLASS_RUN / width + 1).unwrap_err(),
                    StageError::Unsupported("tokenizer-regex-limit".into()), "{id:?} {unit:?}");
            }
        }
        for (id, unit) in [(TokenizerId::Cohere, "1"), (TokenizerId::Llama3, "1"), (TokenizerId::Llama3, "e\u{301}"),
            (TokenizerId::DeepSeek, "1"), (TokenizerId::Gemma, "a")] {
            assert!(encode(id, unit, MAX_CLASS_RUN + 1).is_ok(), "{id:?} {unit:?}");
        }
        // The screen's own patterns run on the linear engine, so they still find a run past fancy-regex's limit.
        for (id, unit) in [(TokenizerId::Claude, "a"), (TokenizerId::Llama3, "!"), (TokenizerId::DeepSeek, "!")] {
            assert_eq!(encode(id, unit, 1_200_000).unwrap_err(), StageError::Unsupported("tokenizer-regex-limit".into()), "{id:?}");
        }
    }
}
