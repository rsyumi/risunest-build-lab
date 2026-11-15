//! `SentencePieceProcessor::Encode` as web-tokenizers runs it for a `Tokenizer.fromSentencePiece` tokenizer: the JS
//! string reaches the library as emscripten's UTF-8, the normalizer applies the precompiled charsmap and the whitespace
//! settings, the BPE or unigram model splits the result, and unknown pieces become byte pieces under byte fallback or
//! merge with the unknown piece before them otherwise. No special token is added.
//!
//! Only what the repository models use is implemented. Building a processor declines the rest: the word and char
//! models (`sentencepiece-model-type`), unused pieces (`sentencepiece-unused-piece`), user-defined pieces in a unigram
//! model (`sentencepiece-unigram-user-defined`), `treat_whitespace_as_suffix` (`sentencepiece-whitespace-suffix`) and
//! unescaped whitespace (`sentencepiece-unescaped-whitespace`). A model the C++ library refuses to load is
//! `Invalid("sentencepiece-model")`.

use super::{ModelProto, ModelType, PieceType};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Charge, OwnedVec, Text};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

fn malformed() -> StageError { StageError::Invalid(Cow::Borrowed("sentencepiece-model")) }
fn unsupported(name: &'static str) -> StageError { StageError::Unsupported(Cow::Borrowed(name)) }

// U+2581, which stands for a space once the normalizer escapes whitespace.
const SPACE: &[u8] = "\u{2581}".as_bytes();
const REPLACEMENT: &[u8] = "\u{fffd}".as_bytes();
// `kUnkPenalty`: an unknown piece scores this much below the lowest normal piece.
const UNKNOWN_PENALTY: f32 = 10.0;
// `kMaxTrieResultsSize`: `NormalizePrefix` reads at most this many charsmap matches.
const CHARSMAP_RESULTS: usize = 32;

/// The bytes emscripten's `stringToUTF8` writes for a JS string: a surrogate unit, paired or not, combines with the
/// unit after it into one code point and consumes it, and one at the end combines with 0. So a lone U+D800 at the end
/// is U+10000 and U+D800 followed by `B` is U+10042.
pub fn emscripten_utf8(text: &Text, budget: &Budget) -> Result<OwnedVec<u8>, StageError> {
    let mut bytes = OwnedVec::new(budget)?;
    if let Some(source) = text.as_unicode() {
        bytes.extend(source.as_bytes())?;
        return Ok(bytes);
    }
    budget.work(text.len_units())?;
    let mut units = text.units();
    let mut encoded = [0; 4];
    while let Some(unit) = units.next() {
        let unit = u32::from(unit);
        let point = if (0xd800..=0xdfff).contains(&unit) {
            let next = units.next().map_or(0, u32::from);
            (0x10000 + ((unit & 0x3ff) << 10)) | (next & 0x3ff)
        } else {
            unit
        };
        let character = char::from_u32(point).ok_or(StageError::Invalid(Cow::Borrowed("emscripten-utf8")))?;
        bytes.extend(character.encode_utf8(&mut encoded).as_bytes())?;
    }
    Ok(bytes)
}

// The length of the character a lead byte starts (`OneCharLen`).
fn one_char_len(lead: u8) -> usize { [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 3, 4][usize::from(lead >> 4)] }

// The length of the well-formed character `input` starts with, or `None` (`IsValidDecodeUTF8`).
fn valid_char_len(input: &[u8]) -> Option<usize> {
    let lead = *input.first()?;
    let trail = |index: usize| input.get(index).filter(|byte| **byte & 0xc0 == 0x80).map(|byte| u32::from(byte & 0x3f));
    let valid = |point: u32| point < 0xd800 || (0xe000..=0x10ffff).contains(&point);
    if lead < 0x80 { return Some(1); }
    if lead & 0xe0 == 0xc0 {
        let point = (u32::from(lead & 0x1f) << 6) | trail(1)?;
        return (point >= 0x80 && valid(point)).then_some(2);
    }
    if lead & 0xf0 == 0xe0 {
        let point = (u32::from(lead & 0x0f) << 12) | (trail(1)? << 6) | trail(2)?;
        return (point >= 0x800 && valid(point)).then_some(3);
    }
    if lead & 0xf8 == 0xf0 {
        let point = (u32::from(lead & 0x07) << 18) | (trail(1)? << 12) | (trail(2)? << 6) | trail(3)?;
        return (point >= 0x10000 && valid(point)).then_some(4);
    }
    None
}

// A darts-clone double array, the trie format of the precompiled charsmap.
struct DoubleArray(Vec<u32>);
impl DoubleArray {
    fn offset(unit: u32) -> usize { ((unit >> 10) << ((unit & (1 << 9)) >> 6)) as usize }
    fn label(unit: u32) -> u32 { unit & ((1 << 31) | 0xff) }
    fn has_leaf(unit: u32) -> bool { (unit >> 8) & 1 == 1 }
    fn value(unit: u32) -> usize { (unit & ((1 << 31) - 1)) as usize }

    // `commonPrefixSearch` keeping the first `CHARSMAP_RESULTS` matches, reduced to the longest: (length, value).
    fn longest_prefix(&self, key: &[u8]) -> Option<(usize, usize)> {
        let mut node = Self::offset(*self.0.first()?);
        let (mut longest, mut results) = (None, 0);
        for (index, byte) in key.iter().enumerate() {
            node ^= usize::from(*byte);
            let Some(unit) = self.0.get(node).copied() else { break };
            if Self::label(unit) != u32::from(*byte) { break; }
            node ^= Self::offset(unit);
            if Self::has_leaf(unit) {
                if results < CHARSMAP_RESULTS {
                    let Some(leaf) = self.0.get(node).copied() else { break };
                    longest = Some((index + 1, Self::value(leaf)));
                }
                results += 1;
            }
        }
        longest
    }
}

// `precompiled_charsmap`: a little-endian trie size, the trie, then the NUL-terminated replacements it indexes.
struct Charsmap { trie: DoubleArray, replacements: Vec<u8> }
impl Charsmap {
    fn decode(blob: &[u8]) -> Result<Option<Self>, StageError> {
        if blob.is_empty() { return Ok(None); }
        let size = blob.get(..4).filter(|_| blob.len() > 4).map(|size| u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as usize)
            .ok_or_else(malformed)?;
        let trie = blob.get(4..).and_then(|rest| rest.get(..size)).filter(|_| size < blob.len()).ok_or_else(malformed)?;
        let trie = DoubleArray(trie.chunks_exact(4).map(|unit| u32::from_le_bytes([unit[0], unit[1], unit[2], unit[3]])).collect());
        Ok(Some(Self { trie, replacements: blob[4 + size..].to_vec() }))
    }
    // The replacement of the longest rule `input` starts with, and the bytes it replaces.
    fn rule(&self, input: &[u8]) -> Option<(&[u8], usize)> {
        let (length, value) = self.trie.longest_prefix(input)?;
        let replacement = self.replacements.get(value..).unwrap_or_default();
        let end = replacement.iter().position(|byte| *byte == 0).unwrap_or(replacement.len());
        Some((&replacement[..end], length))
    }
}

// The user-defined pieces by first byte, for the longest one a text starts with (`PrefixMatcher`).
struct Matcher(Vec<Vec<Box<[u8]>>>);
impl Matcher {
    fn longest(&self, input: &[u8]) -> Option<usize> {
        let candidates = self.0.get(usize::from(*input.first()?))?;
        candidates.iter().filter(|piece| input.starts_with(piece)).map(|piece| piece.len()).max()
    }
}

struct Normalizer { charsmap: Option<Charsmap>, add_dummy_prefix: bool, remove_extra_whitespaces: bool }

enum Model { Bpe, Unigram { unknown_score: f32, longest: usize } }

/// A loaded SentencePiece model, ready to encode.
pub struct Processor {
    kinds: Vec<PieceType>,
    scores: Vec<f32>,
    // `pieces_`: the normal and user-defined pieces, which the models build from.
    pieces: HashMap<Box<[u8]>, u32>,
    // `reserved_id_map_`: the control, unknown and byte pieces, which `PieceToId` looks up first.
    reserved: HashMap<Box<[u8]>, u32>,
    user_defined: Matcher,
    unknown: u32,
    // The `<0xHH>` piece of each byte under byte fallback.
    byte_pieces: Option<Vec<u32>>,
    normalizer: Normalizer,
    model: Model,
}

// The `<0xHH>` text of a byte piece (`ByteToPiece`), and the byte such a text names (`PieceToByte`).
fn byte_piece(byte: u8) -> String { format!("<0x{byte:02X}>") }
fn piece_byte(piece: &str) -> Option<u8> {
    let hex = piece.strip_prefix("<0x")?.strip_suffix('>')?;
    let upper = hex.len() == 2 && hex.bytes().all(|digit| digit.is_ascii_digit() || (b'A'..=b'F').contains(&digit));
    if upper { u8::from_str_radix(hex, 16).ok() } else { None }
}

impl Processor {
    /// Builds the processor `SentencePieceProcessor::Load` would, or declines a model outside the repository's
    /// configuration.
    pub fn new(model: &ModelProto) -> Result<Self, StageError> {
        let trainer = &model.trainer_spec;
        let normalizer = &model.normalizer_spec;
        let unigram = match trainer.model_type {
            ModelType::Unigram => true,
            ModelType::Bpe => false,
            ModelType::Word | ModelType::Char => return Err(unsupported("sentencepiece-model-type")),
        };
        if trainer.treat_whitespace_as_suffix { return Err(unsupported("sentencepiece-whitespace-suffix")); }
        if !normalizer.escape_whitespaces { return Err(unsupported("sentencepiece-unescaped-whitespace")); }
        let id = |index: usize| u32::try_from(index).map_err(|_| malformed());
        let (mut pieces, mut reserved) = (HashMap::new(), HashMap::new());
        let mut user_defined = vec![Vec::new(); 256];
        let mut unknown = None;
        for (index, piece) in model.pieces.iter().enumerate() {
            let text = piece.piece.as_bytes();
            if text.is_empty() { return Err(malformed()); }
            let target = match piece.kind {
                PieceType::Unused => return Err(unsupported("sentencepiece-unused-piece")),
                PieceType::UserDefined if unigram => return Err(unsupported("sentencepiece-unigram-user-defined")),
                PieceType::Normal | PieceType::UserDefined => &mut pieces,
                PieceType::Control | PieceType::Unknown | PieceType::Byte => &mut reserved,
            };
            if target.insert(Box::<[u8]>::from(text), id(index)?).is_some() { return Err(malformed()); }
            match piece.kind {
                PieceType::UserDefined => user_defined[usize::from(text[0])].push(Box::<[u8]>::from(text)),
                PieceType::Unknown if unknown.is_some() => return Err(malformed()),
                PieceType::Unknown => unknown = Some(id(index)?),
                PieceType::Byte if !trainer.byte_fallback || piece_byte(&piece.piece).is_none() => return Err(malformed()),
                _ => {}
            }
        }
        let unknown = unknown.ok_or_else(malformed)?;
        let mut processor = Self {
            kinds: model.pieces.iter().map(|piece| piece.kind).collect(),
            scores: model.pieces.iter().map(|piece| piece.score).collect(),
            pieces,
            reserved,
            user_defined: Matcher(user_defined),
            unknown,
            byte_pieces: None,
            normalizer: Normalizer {
                charsmap: Charsmap::decode(&normalizer.precompiled_charsmap)?,
                add_dummy_prefix: normalizer.add_dummy_prefix,
                remove_extra_whitespaces: normalizer.remove_extra_whitespaces,
            },
            model: Model::Bpe,
        };
        if trainer.byte_fallback {
            processor.byte_pieces = Some((0..=255).map(|byte| processor.piece_to_id(byte_piece(byte).as_bytes())).collect());
        }
        if unigram {
            let lowest = model.pieces.iter().filter(|piece| piece.kind == PieceType::Normal).map(|piece| piece.score).fold(f32::MAX, f32::min);
            let longest = processor.pieces.keys().map(|piece| piece.len()).max().unwrap_or(0);
            processor.model = Model::Unigram { unknown_score: lowest - UNKNOWN_PENALTY, longest };
        }
        Ok(processor)
    }

    // `PieceToId`: reserved pieces first, then the model pieces, then the unknown piece.
    fn piece_to_id(&self, piece: &[u8]) -> u32 {
        self.reserved.get(piece).or_else(|| self.pieces.get(piece)).copied().unwrap_or(self.unknown)
    }

    /// The IDs `Encode` returns for `text`, as `tokenizer.encode` gives them to TS.
    pub fn encode(&self, text: &Text, budget: &Budget) -> Result<Vec<u32>, StageError> {
        budget.work(1)?;
        let input = emscripten_utf8(text, budget)?;
        let (normalized, length) = self.normalize(input.as_slice(), budget)?;
        let normalized = &normalized.as_slice()[..length];
        drop(input);
        let pieces = match self.model {
            Model::Bpe => self.bpe(normalized, budget)?,
            Model::Unigram { unknown_score, longest } => self.unigram(normalized, unknown_score, longest, budget)?,
        };
        let mut ids = Vec::with_capacity(pieces.as_slice().len());
        let mut previous_unknown = false;
        for &(start, end, id) in pieces.as_slice() {
            let kind = self.kinds[id as usize];
            let unknown = kind == PieceType::Unknown;
            match &self.byte_pieces {
                _ if kind == PieceType::Control => ids.push(id),
                Some(bytes) if unknown => {
                    budget.work(end - start)?;
                    ids.extend(normalized[start..end].iter().map(|byte| bytes[usize::from(*byte)]));
                }
                // Adjacent unknown pieces become one.
                _ if unknown && previous_unknown => {}
                _ => ids.push(id),
            }
            previous_unknown = unknown;
        }
        budget.work(ids.len())?;
        Ok(ids)
    }

    // `NormalizePrefix`: a user-defined piece as it is, else the longest charsmap rule, else one character, with a
    // malformed byte read as U+FFFD.
    fn normalize_prefix<'a>(&'a self, input: &'a [u8]) -> (&'a [u8], usize) {
        if let Some(length) = self.user_defined.longest(input) { return (&input[..length], length); }
        if let Some(rule) = self.normalizer.charsmap.as_ref().and_then(|charsmap| charsmap.rule(input)) { return rule; }
        match valid_char_len(input) {
            Some(length) => (&input[..length], length),
            None => (REPLACEMENT, 1),
        }
    }

    // `Normalizer::Normalize`, with the normalized bytes and the length to keep.
    fn normalize(&self, mut input: &[u8], budget: &Budget) -> Result<(OwnedVec<u8>, usize), StageError> {
        let mut normalized = OwnedVec::new(budget)?;
        let remove = self.normalizer.remove_extra_whitespaces;
        // The library first drops leading prefixes that normalize to a space. The loop below drops them too, since it
        // starts as if a space came before, and the trailing trim removes a dummy prefix only whitespace follows.
        if input.is_empty() { return Ok((normalized, 0)); }
        if self.normalizer.add_dummy_prefix { normalized.extend(SPACE)?; }
        let mut previous_space = remove;
        let mut check = 0;
        while !input.is_empty() {
            check += 1;
            if check % 4096 == 0 { budget.check()?; }
            let (mut piece, consumed) = self.normalize_prefix(input);
            if previous_space {
                while let Some(rest) = piece.strip_prefix(b" ") { piece = rest; }
            }
            if !piece.is_empty() {
                previous_space = piece.ends_with(b" ");
                while let Some(position) = piece.iter().position(|byte| *byte == b' ') {
                    normalized.extend(&piece[..position])?;
                    normalized.extend(SPACE)?;
                    piece = &piece[position + 1..];
                }
                normalized.extend(piece)?;
            }
            input = &input[consumed..];
            if !remove { previous_space = false; }
        }
        let mut length = normalized.as_slice().len();
        if remove {
            while normalized.as_slice()[..length].ends_with(SPACE) { length -= SPACE.len(); }
        }
        Ok((normalized, length))
    }

    // `bpe::Model::Encode`: merge the adjacent pair whose piece scores highest, the leftmost among equal scores, until
    // no pair forms a piece. A user-defined piece is one symbol that never merges.
    fn bpe(&self, normalized: &[u8], budget: &Budget) -> Result<OwnedVec<(usize, usize, u32)>, StageError> {
        #[derive(Clone, Copy)]
        struct Symbol { start: usize, length: usize, previous: Option<usize>, next: Option<usize>, frozen: bool }
        struct Pair { score: f32, left: usize, right: usize, length: usize }
        impl PartialEq for Pair { fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal } }
        impl Eq for Pair {}
        impl PartialOrd for Pair { fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) } }
        impl Ord for Pair {
            fn cmp(&self, other: &Self) -> Ordering {
                self.score.partial_cmp(&other.score).unwrap_or(Ordering::Equal).then_with(|| other.left.cmp(&self.left))
            }
        }
        let mut symbols = OwnedVec::new(budget)?;
        let mut start = 0;
        while start < normalized.len() {
            let rest = &normalized[start..];
            let (length, frozen) = match self.user_defined.longest(rest) {
                Some(length) => (length, true),
                None => (one_char_len(rest[0]).min(rest.len()), false),
            };
            let index = symbols.as_slice().len();
            let next = (start + length < normalized.len()).then_some(index + 1);
            symbols.push(Symbol { start, length, previous: index.checked_sub(1), next, frozen })?;
            start += length;
        }
        let symbols = symbols.as_mut_slice();
        let mut agenda = BinaryHeap::new();
        // The agenda grows only after its larger buffer is charged.
        let mut charge: Option<Charge> = None;
        let mut push = |agenda: &mut BinaryHeap<Pair>, symbols: &[Symbol], left: Option<usize>, right: Option<usize>| -> Result<(), StageError> {
            let (Some(left), Some(right)) = (left, right) else { return Ok(()) };
            let (first, second) = (symbols[left], symbols[right]);
            if first.frozen || second.frozen { return Ok(()); }
            let length = first.length + second.length;
            budget.work(length)?;
            let Some(&id) = self.pieces.get(&normalized[first.start..first.start + length]) else { return Ok(()) };
            if agenda.len() == agenda.capacity() {
                let target = agenda.capacity().saturating_mul(2).max(16);
                charge = Some(budget.reserve_owned(target.saturating_mul(std::mem::size_of::<Pair>()))?);
                agenda.reserve_exact(target - agenda.len());
            }
            agenda.push(Pair { score: self.scores[id as usize], left, right, length });
            Ok(())
        };
        for right in 1..symbols.len() { push(&mut agenda, symbols, Some(right - 1), Some(right))?; }
        while let Some(top) = agenda.pop() {
            budget.work(1)?;
            let (left, right) = (top.left, top.right);
            if symbols[left].length == 0 || symbols[right].length == 0 || symbols[left].length + symbols[right].length != top.length {
                continue;
            }
            symbols[left].length = top.length;
            symbols[left].next = symbols[right].next;
            if let Some(next) = symbols[right].next { symbols[next].previous = Some(left); }
            symbols[right].length = 0;
            push(&mut agenda, symbols, symbols[left].previous, Some(left))?;
            push(&mut agenda, symbols, Some(left), symbols[left].next)?;
        }
        let mut pieces = OwnedVec::new(budget)?;
        let mut index = (!symbols.is_empty()).then_some(0);
        while let Some(current) = index {
            let symbol = symbols[current];
            let end = symbol.start + symbol.length;
            budget.work(symbol.length)?;
            pieces.push((symbol.start, end, self.piece_to_id(&normalized[symbol.start..end])))?;
            index = symbol.next;
        }
        Ok(pieces)
    }

    // `unigram::Model::EncodeOptimized`: the best-scoring path through the pieces, kept per end position. A piece's
    // candidate is computed in double precision against the stored single-precision best, so of two equal paths the
    // one whose last piece starts first stays; a character no piece covers is an unknown piece scored
    // `unknown_score`, in single precision.
    fn unigram(&self, normalized: &[u8], unknown_score: f32, longest: usize, budget: &Budget) -> Result<OwnedVec<(usize, usize, u32)>, StageError> {
        #[derive(Clone, Copy)]
        struct Best { id: u32, score: f32, start: Option<usize> }
        let size = normalized.len();
        let _charge = budget.reserve_owned((size + 1).saturating_mul(std::mem::size_of::<Best>()))?;
        let mut best = vec![Best { id: self.unknown, score: 0.0, start: None }; size + 1];
        let mut start = 0;
        while start < size {
            let till_here = best[start].score;
            let character = one_char_len(normalized[start]).min(size - start);
            let mut single = false;
            let reach = longest.min(size - start);
            budget.work(reach)?;
            for length in 1..=reach {
                let Some(&id) = self.pieces.get(&normalized[start..start + length]) else { continue };
                let target = &mut best[start + length];
                let candidate = f64::from(self.scores[id as usize]) + f64::from(till_here);
                if target.start.is_none() || candidate > f64::from(target.score) {
                    *target = Best { id, score: candidate as f32, start: Some(start) };
                }
                single |= length == character;
            }
            if !single {
                let target = &mut best[start + character];
                let candidate = unknown_score + till_here;
                if target.start.is_none() || candidate > target.score {
                    *target = Best { id: self.unknown, score: candidate, start: Some(start) };
                }
            }
            start += character;
        }
        let mut pieces = OwnedVec::new(budget)?;
        let mut end = size;
        while end > 0 {
            let node = best[end];
            let start = node.start.ok_or(StageError::Invalid(Cow::Borrowed("sentencepiece-lattice")))?;
            pieces.push((start, end, node.id))?;
            end = start;
        }
        budget.work(pieces.as_slice().len())?;
        pieces.as_mut_slice().reverse();
        Ok(pieces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::sentencepiece::{NormalizerSpec, Piece, TrainerSpec};
    use PieceType::{Byte, Control, Normal, Unknown, Unused, UserDefined};

    fn model(model_type: ModelType, pieces: &[(&str, PieceType)]) -> ModelProto {
        ModelProto {
            pieces: pieces.iter().map(|(piece, kind)| Piece { piece: (*piece).into(), score: -1.0, kind: *kind }).collect(),
            trainer_spec: TrainerSpec { model_type, ..TrainerSpec::default() },
            normalizer_spec: NormalizerSpec::default(),
            denormalizer_spec: None,
        }
    }
    fn built(model: &ModelProto) -> Option<StageError> { Processor::new(model).err() }
    fn budget() -> Budget {
        let limits = risunest_ir::Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 10,
            max_depth: 16, max_work: 1 << 30 };
        Budget::new(limits, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap()
    }

    // `bpe_model.cc` freezes a user-defined piece: it stays one symbol and never merges. No repository model has a piece
    // such a merge would form, so this synthetic model is what shows it.
    #[test]
    fn user_defined_pieces_never_merge() {
        let mut model = model(ModelType::Bpe, &[("<unk>", Unknown), ("ab", UserDefined), ("c", Normal), ("abc", Normal), ("a", Normal), ("b", Normal)]);
        model.normalizer_spec.add_dummy_prefix = false;
        let processor = Processor::new(&model).unwrap();
        let budget = budget();
        let encode = |text: &str| processor.encode(&Text::from_str(text, &budget).unwrap(), &budget).unwrap();
        assert_eq!(encode("abc"), [1, 2]);
    }

    #[test]
    fn configurations_the_repository_models_do_not_use_are_declined() {
        let base = [("<unk>", Unknown), ("<s>", Control), ("a", Normal)];
        assert_eq!(built(&model(ModelType::Bpe, &base)), None);
        assert_eq!(built(&model(ModelType::Unigram, &base)), None);
        for model_type in [ModelType::Word, ModelType::Char] {
            assert_eq!(built(&model(model_type, &base)), Some(unsupported("sentencepiece-model-type")));
        }
        assert_eq!(built(&model(ModelType::Bpe, &[("<unk>", Unknown), ("a", Unused)])), Some(unsupported("sentencepiece-unused-piece")));
        assert_eq!(built(&model(ModelType::Bpe, &[("<unk>", Unknown), ("ab", UserDefined)])), None);
        assert_eq!(built(&model(ModelType::Unigram, &[("<unk>", Unknown), ("ab", UserDefined)])),
            Some(unsupported("sentencepiece-unigram-user-defined")));
        let mut suffix = model(ModelType::Bpe, &base);
        suffix.trainer_spec.treat_whitespace_as_suffix = true;
        assert_eq!(built(&suffix), Some(unsupported("sentencepiece-whitespace-suffix")));
        let mut unescaped = model(ModelType::Bpe, &base);
        unescaped.normalizer_spec.escape_whitespaces = false;
        assert_eq!(built(&unescaped), Some(unsupported("sentencepiece-unescaped-whitespace")));
    }

    #[test]
    fn models_the_library_refuses_to_load_are_invalid() {
        let invalid = Some(malformed());
        assert_eq!(built(&model(ModelType::Bpe, &[("a", Normal)])), invalid, "no unknown piece");
        assert_eq!(built(&model(ModelType::Bpe, &[("<unk>", Unknown), ("<u>", Unknown)])), invalid, "two unknown pieces");
        assert_eq!(built(&model(ModelType::Bpe, &[("<unk>", Unknown), ("a", Normal), ("a", UserDefined)])), invalid, "duplicate");
        assert_eq!(built(&model(ModelType::Bpe, &[("<unk>", Unknown), ("", Normal)])), invalid, "empty piece");
        let bytes = [("<unk>", Unknown), ("<0xFF>", Byte)];
        assert_eq!(built(&model(ModelType::Bpe, &bytes)), invalid, "byte piece without byte fallback");
        let mut fallback = model(ModelType::Bpe, &bytes);
        fallback.trainer_spec.byte_fallback = true;
        assert_eq!(built(&fallback), None);
        fallback.pieces[1].piece = "<0xff>".into();
        assert_eq!(built(&fallback), invalid, "lowercase byte piece");
        let mut charsmap = model(ModelType::Bpe, &[("<unk>", Unknown)]);
        for blob in [vec![1, 0, 0, 0], vec![8, 0, 0, 0, 1, 2, 3, 4], vec![6, 0, 0, 0, 1, 2, 3, 4]] {
            charsmap.normalizer_spec.precompiled_charsmap = blob;
            assert_eq!(built(&charsmap), invalid, "charsmap");
        }
    }
}
