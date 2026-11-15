//! The SentencePiece `ModelProto` (`sentencepiece_model.proto`) a `.model` artifact holds, as `Tokenizer.fromSentencePiece`
//! loads it: the pieces and the trainer and normalizer settings encoding reads.
//!
//! Parsing follows proto2 as the C++ library does: a scalar field keeps its last value, a repeated message field
//! appends, every occurrence of a message field merges into it, and an enum value the schema does not name leaves the
//! field as it was. Fields this reader does not keep, and kept fields that arrive with a wire type their schema does not
//! give them, are skipped. A string that is not UTF-8, which the C++ library would take, is declined. The input is a
//! fingerprinted artifact, so parsing charges no `Budget`.

pub mod processor;

use risunest_ir::stage::StageError;
use std::borrow::Cow;

/// `TrainerSpec.ModelType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelType { Unigram, Bpe, Word, Char }

/// `ModelProto.SentencePiece.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PieceType { Normal, Unknown, Control, UserDefined, Unused, Byte }

/// One vocabulary entry; its token ID is its index in [`ModelProto::pieces`].
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub piece: String,
    pub score: f32,
    pub kind: PieceType,
}

/// The `TrainerSpec` fields encoding reads, with the schema defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct TrainerSpec {
    pub model_type: ModelType,
    pub vocab_size: i32,
    pub max_sentencepiece_length: i32,
    pub split_by_unicode_script: bool,
    pub split_by_whitespace: bool,
    pub split_by_number: bool,
    pub treat_whitespace_as_suffix: bool,
    pub split_digits: bool,
    pub allow_whitespace_only_pieces: bool,
    pub byte_fallback: bool,
    pub unk_id: i32,
    pub bos_id: i32,
    pub eos_id: i32,
    pub pad_id: i32,
    pub unk_surface: String,
    pub unk_piece: String,
    pub bos_piece: String,
    pub eos_piece: String,
    pub pad_piece: String,
}

impl Default for TrainerSpec {
    fn default() -> Self {
        Self {
            model_type: ModelType::Unigram,
            vocab_size: 8000,
            max_sentencepiece_length: 16,
            split_by_unicode_script: true,
            split_by_whitespace: true,
            split_by_number: true,
            treat_whitespace_as_suffix: false,
            split_digits: false,
            allow_whitespace_only_pieces: false,
            byte_fallback: false,
            unk_id: 0,
            bos_id: 1,
            eos_id: 2,
            pad_id: -1,
            unk_surface: " \u{2047} ".to_owned(),
            unk_piece: "<unk>".to_owned(),
            bos_piece: "<s>".to_owned(),
            eos_piece: "</s>".to_owned(),
            pad_piece: "<pad>".to_owned(),
        }
    }
}

/// `NormalizerSpec`, with the schema defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizerSpec {
    pub name: String,
    /// The compiled normalization rules (a double-array trie and its replacements); empty for `identity`.
    pub precompiled_charsmap: Vec<u8>,
    pub add_dummy_prefix: bool,
    pub remove_extra_whitespaces: bool,
    pub escape_whitespaces: bool,
    pub normalization_rule_tsv: String,
}

impl Default for NormalizerSpec {
    fn default() -> Self {
        Self {
            name: String::new(),
            precompiled_charsmap: Vec::new(),
            add_dummy_prefix: true,
            remove_extra_whitespaces: true,
            escape_whitespaces: true,
            normalization_rule_tsv: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelProto {
    pub pieces: Vec<Piece>,
    pub trainer_spec: TrainerSpec,
    pub normalizer_spec: NormalizerSpec,
    /// `None` when the model has no `denormalizer_spec`, which decoding alone reads.
    pub denormalizer_spec: Option<NormalizerSpec>,
}

fn malformed() -> StageError { StageError::Invalid(Cow::Borrowed("sentencepiece-model")) }

enum Wire<'a> {
    Varint(u64),
    Fixed64,
    Bytes(&'a [u8]),
    Fixed32([u8; 4]),
}

// The fields of one message, in order, as field numbers and wire values.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Self { Self { bytes, at: 0 } }

    fn varint(&mut self) -> Result<u64, StageError> {
        let mut value = 0u64;
        for index in 0..10 {
            let byte = *self.bytes.get(self.at).ok_or_else(malformed)?;
            self.at += 1;
            // The tenth byte holds only the top bit of a 64-bit value.
            if index == 9 && byte > 1 { return Err(malformed()); }
            value |= u64::from(byte & 0x7f) << (7 * index);
            if byte < 0x80 { return Ok(value); }
        }
        Err(malformed())
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], StageError> {
        let end = self.at.checked_add(length).filter(|end| *end <= self.bytes.len()).ok_or_else(malformed)?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn next(&mut self) -> Result<Option<(u64, Wire<'a>)>, StageError> {
        if self.at == self.bytes.len() { return Ok(None); }
        let key = self.varint()?;
        let number = key >> 3;
        if number == 0 || number > (1 << 29) - 1 { return Err(malformed()); }
        let wire = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => { self.take(8)?; Wire::Fixed64 }
            2 => {
                let length = usize::try_from(self.varint()?).map_err(|_| malformed())?;
                Wire::Bytes(self.take(length)?)
            }
            5 => {
                let mut fixed = [0; 4];
                fixed.copy_from_slice(self.take(4)?);
                Wire::Fixed32(fixed)
            }
            // Groups (3 and 4) are not in this schema, and 6 and 7 are no wire type.
            _ => return Err(malformed()),
        };
        Ok(Some((number, wire)))
    }
}

fn text(bytes: &[u8]) -> Result<String, StageError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| StageError::Unsupported(Cow::Borrowed("sentencepiece-utf8")))
}

// `int32` arrives as a varint of the value sign-extended to 64 bits; the parser keeps its low 32 bits.
fn int32(value: u64) -> i32 { value as u32 as i32 }

fn model_type(value: u64) -> Option<ModelType> {
    Some(match value { 1 => ModelType::Unigram, 2 => ModelType::Bpe, 3 => ModelType::Word, 4 => ModelType::Char, _ => return None })
}

fn piece_type(value: u64) -> Option<PieceType> {
    Some(match value {
        1 => PieceType::Normal,
        2 => PieceType::Unknown,
        3 => PieceType::Control,
        4 => PieceType::UserDefined,
        5 => PieceType::Unused,
        6 => PieceType::Byte,
        _ => return None,
    })
}

fn piece(bytes: &[u8]) -> Result<Piece, StageError> {
    let mut piece = Piece { piece: String::new(), score: 0.0, kind: PieceType::Normal };
    let mut fields = Fields::new(bytes);
    while let Some((number, wire)) = fields.next()? {
        match (number, wire) {
            (1, Wire::Bytes(value)) => piece.piece = text(value)?,
            (2, Wire::Fixed32(value)) => piece.score = f32::from_le_bytes(value),
            (3, Wire::Varint(value)) => if let Some(kind) = piece_type(value) { piece.kind = kind },
            _ => {}
        }
    }
    Ok(piece)
}

fn merge_trainer(spec: &mut TrainerSpec, bytes: &[u8]) -> Result<(), StageError> {
    let mut fields = Fields::new(bytes);
    while let Some((number, wire)) = fields.next()? {
        match (number, wire) {
            (3, Wire::Varint(value)) => if let Some(kind) = model_type(value) { spec.model_type = kind },
            (4, Wire::Varint(value)) => spec.vocab_size = int32(value),
            (20, Wire::Varint(value)) => spec.max_sentencepiece_length = int32(value),
            (21, Wire::Varint(value)) => spec.split_by_unicode_script = value != 0,
            (22, Wire::Varint(value)) => spec.split_by_whitespace = value != 0,
            (23, Wire::Varint(value)) => spec.split_by_number = value != 0,
            (24, Wire::Varint(value)) => spec.treat_whitespace_as_suffix = value != 0,
            (25, Wire::Varint(value)) => spec.split_digits = value != 0,
            (26, Wire::Varint(value)) => spec.allow_whitespace_only_pieces = value != 0,
            (35, Wire::Varint(value)) => spec.byte_fallback = value != 0,
            (40, Wire::Varint(value)) => spec.unk_id = int32(value),
            (41, Wire::Varint(value)) => spec.bos_id = int32(value),
            (42, Wire::Varint(value)) => spec.eos_id = int32(value),
            (43, Wire::Varint(value)) => spec.pad_id = int32(value),
            (44, Wire::Bytes(value)) => spec.unk_surface = text(value)?,
            (45, Wire::Bytes(value)) => spec.unk_piece = text(value)?,
            (46, Wire::Bytes(value)) => spec.bos_piece = text(value)?,
            (47, Wire::Bytes(value)) => spec.eos_piece = text(value)?,
            (48, Wire::Bytes(value)) => spec.pad_piece = text(value)?,
            _ => {}
        }
    }
    Ok(())
}

fn merge_normalizer(spec: &mut NormalizerSpec, bytes: &[u8]) -> Result<(), StageError> {
    let mut fields = Fields::new(bytes);
    while let Some((number, wire)) = fields.next()? {
        match (number, wire) {
            (1, Wire::Bytes(value)) => spec.name = text(value)?,
            (2, Wire::Bytes(value)) => spec.precompiled_charsmap = value.to_vec(),
            (3, Wire::Varint(value)) => spec.add_dummy_prefix = value != 0,
            (4, Wire::Varint(value)) => spec.remove_extra_whitespaces = value != 0,
            (5, Wire::Varint(value)) => spec.escape_whitespaces = value != 0,
            (6, Wire::Bytes(value)) => spec.normalization_rule_tsv = text(value)?,
            _ => {}
        }
    }
    Ok(())
}

/// Reads a serialized `ModelProto`. Malformed wire data is `Invalid("sentencepiece-model")`.
pub fn parse(bytes: &[u8]) -> Result<ModelProto, StageError> {
    let mut model = ModelProto::default();
    let mut fields = Fields::new(bytes);
    while let Some((number, wire)) = fields.next()? {
        match (number, wire) {
            (1, Wire::Bytes(value)) => model.pieces.push(piece(value)?),
            (2, Wire::Bytes(value)) => merge_trainer(&mut model.trainer_spec, value)?,
            (3, Wire::Bytes(value)) => merge_normalizer(&mut model.normalizer_spec, value)?,
            (5, Wire::Bytes(value)) => merge_normalizer(model.denormalizer_spec.get_or_insert_with(NormalizerSpec::default), value)?,
            _ => {}
        }
    }
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Protobuf wire bytes for the tests, built field by field.
    fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push(value as u8 | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn key(number: u64, wire: u64, out: &mut Vec<u8>) { varint(number << 3 | wire, out) }
    fn number(field: u64, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        key(field, 0, &mut out);
        varint(value, &mut out);
        out
    }
    fn bytes(field: u64, value: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        key(field, 2, &mut out);
        varint(value.len() as u64, &mut out);
        out.extend_from_slice(value);
        out
    }
    fn float(field: u64, value: f32) -> Vec<u8> {
        let mut out = Vec::new();
        key(field, 5, &mut out);
        out.extend_from_slice(&value.to_le_bytes());
        out
    }
    fn piece_bytes(piece: &str, score: f32, kind: Option<u64>) -> Vec<u8> {
        let mut message = bytes(1, piece.as_bytes());
        message.extend(float(2, score));
        if let Some(kind) = kind { message.extend(number(3, kind)); }
        bytes(1, &message)
    }
    fn invalid() -> StageError { StageError::Invalid(Cow::Borrowed("sentencepiece-model")) }

    #[test]
    fn an_empty_model_carries_the_schema_defaults() {
        let model = parse(&[]).unwrap();
        assert!(model.pieces.is_empty());
        assert_eq!(model.trainer_spec, TrainerSpec::default());
        assert_eq!((model.trainer_spec.model_type, model.trainer_spec.pad_id, model.trainer_spec.unk_surface.as_str()),
            (ModelType::Unigram, -1, " \u{2047} "));
        assert_eq!(model.normalizer_spec, NormalizerSpec::default());
        assert!(model.normalizer_spec.add_dummy_prefix && model.normalizer_spec.remove_extra_whitespaces
            && model.normalizer_spec.escape_whitespaces);
        assert_eq!(model.denormalizer_spec, None);
    }

    #[test]
    fn pieces_keep_their_order_scores_and_types() {
        let mut model = piece_bytes("<unk>", 0.0, Some(2));
        model.extend(piece_bytes("\u{2581}the", -1.5, None));
        model.extend(piece_bytes("<0x41>", 0.0, Some(6)));
        model.extend(piece_bytes("<s>", 0.0, Some(3)));
        model.extend(piece_bytes("\u{2581}\u{2581}", 0.0, Some(4)));
        model.extend(piece_bytes("unused", 0.0, Some(5)));
        let pieces = parse(&model).unwrap().pieces;
        let kinds = pieces.iter().map(|piece| piece.kind).collect::<Vec<_>>();
        assert_eq!(kinds, [PieceType::Unknown, PieceType::Normal, PieceType::Byte, PieceType::Control, PieceType::UserDefined, PieceType::Unused]);
        assert_eq!((pieces[1].piece.as_str(), pieces[1].score), ("\u{2581}the", -1.5));
    }

    #[test]
    fn trainer_and_normalizer_fields_read_their_numbers() {
        let mut trainer = Vec::new();
        for (field, value) in [(3, 2), (4, 32000), (20, 12), (21, 0), (22, 0), (23, 0), (24, 1), (25, 1), (26, 1), (35, 1), (40, 3), (41, 4), (42, 5)] {
            trainer.extend(number(field, value));
        }
        // pad_id -1 travels as a ten-byte varint of the sign-extended value.
        trainer.extend(number(43, u64::MAX));
        for (field, value) in [(44, "?"), (45, "<u>"), (46, "<b>"), (47, "<e>"), (48, "<p>")] { trainer.extend(bytes(field, value.as_bytes())); }
        let mut normalizer = bytes(1, b"nmt_nfkc");
        normalizer.extend(bytes(2, &[1, 2, 3]));
        for (field, value) in [(3, 0), (4, 0), (5, 0)] { normalizer.extend(number(field, value)); }
        normalizer.extend(bytes(6, b"41\t61"));
        let mut model = bytes(2, &trainer);
        model.extend(bytes(3, &normalizer));
        model.extend(bytes(5, &bytes(1, b"identity")));
        let model = parse(&model).unwrap();
        assert_eq!(model.trainer_spec, TrainerSpec {
            model_type: ModelType::Bpe, vocab_size: 32000, max_sentencepiece_length: 12, split_by_unicode_script: false,
            split_by_whitespace: false, split_by_number: false, treat_whitespace_as_suffix: true, split_digits: true,
            allow_whitespace_only_pieces: true, byte_fallback: true, unk_id: 3, bos_id: 4, eos_id: 5, pad_id: -1,
            unk_surface: "?".into(), unk_piece: "<u>".into(), bos_piece: "<b>".into(), eos_piece: "<e>".into(), pad_piece: "<p>".into(),
        });
        assert_eq!(model.normalizer_spec, NormalizerSpec {
            name: "nmt_nfkc".into(), precompiled_charsmap: vec![1, 2, 3], add_dummy_prefix: false, remove_extra_whitespaces: false,
            escape_whitespaces: false, normalization_rule_tsv: "41\t61".into(),
        });
        assert_eq!(model.denormalizer_spec, Some(NormalizerSpec { name: "identity".into(), ..NormalizerSpec::default() }));
    }

    #[test]
    fn repeated_messages_merge_and_the_last_scalar_wins() {
        let mut model = bytes(2, &[number(4, 100), number(35, 1)].concat());
        model.extend(bytes(2, &number(4, 200)));
        model.extend(bytes(3, &number(3, 0)));
        model.extend(bytes(3, &bytes(1, b"identity")));
        let model = parse(&model).unwrap();
        assert_eq!((model.trainer_spec.vocab_size, model.trainer_spec.byte_fallback), (200, true));
        assert_eq!((model.normalizer_spec.name.as_str(), model.normalizer_spec.add_dummy_prefix), ("identity", false));
    }

    #[test]
    fn unknown_fields_wire_mismatches_and_unnamed_enums_are_skipped() {
        let mut trainer = number(3, 2);
        trainer.extend(number(3, 9)); // no ModelType is 9: the BPE before it stands
        trainer.extend(bytes(4, b"not a varint")); // vocab_size with the wrong wire type
        trainer.extend(number(999, 7));
        let mut fixed64 = Vec::new();
        key(1000, 1, &mut fixed64);
        fixed64.extend_from_slice(&[0; 8]);
        trainer.extend(&fixed64);
        let mut model = bytes(2, &trainer);
        model.extend(piece_bytes("x", 0.5, Some(7)));
        model.extend(bytes(4, &bytes(1, b"self test data")));
        model.extend(float(6, 1.0));
        let model = parse(&model).unwrap();
        assert_eq!((model.trainer_spec.model_type, model.trainer_spec.vocab_size), (ModelType::Bpe, 8000));
        assert_eq!(model.pieces, [Piece { piece: "x".into(), score: 0.5, kind: PieceType::Normal }]);
    }

    #[test]
    fn malformed_wire_data_is_invalid() {
        let truncated_length = { let mut out = Vec::new(); key(1, 2, &mut out); varint(5, &mut out); out.extend_from_slice(b"abc"); out };
        let group = { let mut out = Vec::new(); key(1, 3, &mut out); out };
        let field_zero = number(0, 1);
        let overlong = { let mut out = Vec::new(); key(4, 0, &mut out); out.extend_from_slice(&[0xff; 10]); out.push(0); out };
        let tenth_byte = { let mut out = Vec::new(); key(4, 0, &mut out); out.extend_from_slice(&[0xff; 9]); out.push(2); out };
        let open_varint = { let mut out = Vec::new(); key(4, 0, &mut out); out.push(0x80); out };
        let short_float = { let mut out = Vec::new(); key(2, 5, &mut out); out.extend_from_slice(&[0, 0]); out };
        for (name, model) in [("length", truncated_length), ("group", group), ("field zero", field_zero), ("overlong varint", overlong),
            ("tenth varint byte", tenth_byte), ("open varint", open_varint), ("short fixed32", bytes(1, &short_float))] {
            assert_eq!(parse(&model), Err(invalid()), "{name}");
        }
        // A malformed nested message is malformed too.
        assert_eq!(parse(&bytes(2, &[0x80])), Err(invalid()));
    }

    #[test]
    fn pieces_that_are_not_utf8_are_declined() {
        let model = bytes(1, &bytes(1, &[0xff, 0xfe]));
        assert_eq!(parse(&model), Err(StageError::Unsupported(Cow::Borrowed("sentencepiece-utf8"))));
    }
}
