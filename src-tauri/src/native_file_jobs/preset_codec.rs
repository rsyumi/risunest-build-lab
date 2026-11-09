//! Bounded codecs for the released preset file formats. A declined codec leaves the owned source for the TS reader.
use aes::cipher::{BlockEncrypt, KeyInit};
use flate2::{bufread::{DeflateDecoder, GzDecoder, ZlibDecoder}, Compress, Compression, FlushCompress, Status};
use ghash::universal_hash::UniversalHash;
use risunest_ir::{Budget, OwnedVec, Text, Value};
use risunest_ir::stage::StageError;
use sha2::{Digest, Sha256};
use std::io::Read;
use subtle::ConstantTimeEq;

pub(super) const MAX_SOURCE_BYTES: usize = 32 << 20;
const MAX_ENVELOPE_BYTES: usize = 25 << 20;
const MAX_PRESET_BYTES: usize = 24 << 20;
const MAX_VALUES: usize = 1 << 20;
const MAX_DEPTH: usize = 128;
const CHUNK: usize = 64 << 10;
const RPACK_MAP: &[u8; 512] = include_bytes!("../../../src/ts/rpack/rpack_map.bin");

fn decline(name: &'static str) -> StageError { StageError::Unsupported(name.into()) }
fn check(cancel: &impl Fn() -> bool) -> Result<(), StageError> {
    if cancel() { Err(StageError::Cancelled) } else { Ok(()) }
}
fn bound(length: usize, maximum: usize, name: &'static str) -> Result<(), StageError> {
    if length > maximum { Err(decline(name)) } else { Ok(()) }
}
fn append(output: &mut Vec<u8>, bytes: &[u8], maximum: usize) -> Result<(), StageError> {
    let length = output.len().checked_add(bytes.len()).ok_or_else(|| decline("boundedPresetBytes"))?;
    bound(length, maximum, "boundedPresetBytes")?;
    output.try_reserve(bytes.len()).map_err(|_| decline("boundedPresetAllocation"))?;
    output.extend_from_slice(bytes);
    Ok(())
}

pub(super) fn decode(bytes: &[u8], name: &str, budget: &Budget, cancel: &impl Fn() -> bool) -> Result<Value, StageError> {
    check(cancel)?;
    bound(bytes.len(), MAX_SOURCE_BYTES, "boundedPresetSource")?;
    if !name.ends_with(".risup") && !name.ends_with(".risupreset") {
        bound(bytes.len(), MAX_PRESET_BYTES, "boundedPresetJson")?;
        // Buffer.toString('utf-8') replaces invalid sequences and preserves a leading BOM. JSON.parse rejects that BOM.
        let text = String::from_utf8_lossy(bytes);
        bound(text.len(), MAX_PRESET_BYTES, "boundedPresetJson")?;
        let value = risunest_ir::json::parse_json(&text, budget)?;
        check(cancel)?;
        return Ok(value);
    }
    let packed;
    let bytes = if name.ends_with(".risup") {
        packed = rpack(bytes, false, cancel)?;
        packed.as_slice()
    } else { bytes };
    let envelope = inflate(bytes, MAX_ENVELOPE_BYTES, cancel)?;
    let mut reader = Reader::new(&envelope, budget, cancel);
    let count = reader.map_length()?;
    let (mut version, mut kind, mut preset, mut pres) = (None, None, None, None);
    for _ in 0..count {
        let key = reader.key()?;
        let value = reader.value(1)?;
        let target = if key.equals_ascii(b"presetVersion") { Some(&mut version) }
            else if key.equals_ascii(b"type") { Some(&mut kind) }
            else if key.equals_ascii(b"preset") { Some(&mut preset) }
            else if key.equals_ascii(b"pres") { Some(&mut pres) } else { None };
        if let Some(target) = target { *target = Some(value); }
    }
    reader.finish()?;
    if !matches!(version, Some(Node::Data(Value::Number(n))) if n == 0.0 || n == 2.0)
        || !matches!(kind, Some(Node::Data(Value::Text(ref text))) if text.equals_ascii(b"preset")) {
        return Err(decline("risupContainer"));
    }
    let body = match preset { None | Some(Node::Data(Value::Null)) | Some(Node::Undefined) => pres, value => value };
    let Some(Node::Binary(encrypted)) = body else { return Err(decline("risupBody")); };
    let plaintext = decrypt(encrypted, cancel)?;
    let mut reader = Reader::new(&plaintext, budget, cancel);
    let value = reader.value(0)?.data()?;
    reader.finish()?;
    check(cancel)?;
    Ok(value)
}

pub(super) fn encode(exported: &Value, format: &str, budget: &Budget, cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    check(cancel)?;
    if format == "json" {
        let text = risunest_formats::preset::exported_preset_json(exported, budget)?
            .ok_or_else(|| decline("presetJson"))?;
        let text = text.as_unicode().ok_or_else(|| decline("presetJson"))?;
        bound(text.len(), MAX_PRESET_BYTES, "boundedPresetJson")?;
        check(cancel)?;
        return Ok(text.as_bytes().to_vec());
    }
    if format != "risup" { return Err(decline("presetFormat")); }
    let mut writer = Writer { bytes: Vec::new(), budget, cancel, values: 0 };
    writer.value(exported, 0)?;
    let encrypted = encrypt(&writer.bytes, cancel)?;
    let mut envelope = Vec::new();
    append(&mut envelope, &[0x83, 0xad], MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, b"presetVersion", MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, &[2, 0xa4], MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, b"type", MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, &[0xa6], MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, b"preset", MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, &[0xa6], MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, b"preset", MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, &[0xc6], MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, &(encrypted.len() as u32).to_be_bytes(), MAX_ENVELOPE_BYTES)?;
    append(&mut envelope, &encrypted, MAX_ENVELOPE_BYTES)?;
    let compressed = deflate(&envelope, cancel)?;
    rpack(&compressed, true, cancel)
}

fn rpack(bytes: &[u8], encode: bool, cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    let mut output = Vec::new();
    output.try_reserve_exact(bytes.len()).map_err(|_| decline("boundedPresetAllocation"))?;
    for chunk in bytes.chunks(CHUNK) {
        check(cancel)?;
        output.extend(chunk.iter().map(|byte| RPACK_MAP[usize::from(!encode) * 256 + *byte as usize]));
    }
    Ok(output)
}
fn inflate(bytes: &[u8], maximum: usize, cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    let mut remaining = bytes;
    let mut reader: Box<dyn Read + '_> = if bytes.starts_with(&[0x1f, 0x8b, 8]) { Box::new(GzDecoder::new(&mut remaining)) }
        else if bytes.len() >= 2 && bytes[0] & 0x0f == 8 && bytes[0] >> 4 <= 7 && u16::from_be_bytes([bytes[0], bytes[1]]) % 31 == 0 {
            Box::new(ZlibDecoder::new(&mut remaining))
        } else { Box::new(DeflateDecoder::new(&mut remaining)) };
    let mut output = Vec::new();
    let mut chunk = [0u8; CHUNK];
    loop {
        check(cancel)?;
        let size = reader.read(&mut chunk).map_err(|_| decline("presetCompression"))?;
        if size == 0 { break; }
        append(&mut output, &chunk[..size], maximum)?;
    }
    drop(reader);
    if !remaining.is_empty() { return Err(decline("presetCompressionTrailing")); }
    check(cancel)?;
    Ok(output)
}
fn deflate(bytes: &[u8], cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    let mut compressor = Compress::new(Compression::default(), true);
    let mut output = Vec::new();
    let mut chunk = [0u8; CHUNK];
    loop {
        check(cancel)?;
        let before = (compressor.total_in(), compressor.total_out());
        let status = compressor.compress(&bytes[before.0 as usize..], &mut chunk, FlushCompress::Finish)
            .map_err(|_| decline("presetCompression"))?;
        append(&mut output, &chunk[..(compressor.total_out() - before.1) as usize], MAX_SOURCE_BYTES)?;
        if status == Status::StreamEnd { return Ok(output); }
        if before == (compressor.total_in(), compressor.total_out()) { return Err(decline("presetCompression")); }
    }
}

fn cipher() -> aes::Aes256 { aes::Aes256::new_from_slice(&Sha256::digest(b"risupreset")).expect("SHA-256 produces a 32-byte AES-256 key") }
fn auth(cipher: &aes::Aes256, ciphertext: &[u8], cancel: &impl Fn() -> bool) -> Result<[u8; 16], StageError> {
    let mut key = aes::cipher::Block::<aes::Aes256>::default();
    cipher.encrypt_block(&mut key);
    let mut auth = ghash::GHash::new(&key);
    for chunk in ciphertext.chunks(CHUNK) { check(cancel)?; auth.update_padded(chunk); }
    let mut lengths = ghash::Block::default();
    lengths[8..].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());
    auth.update(&[lengths]);
    let mut mask = aes::cipher::Block::<aes::Aes256>::default();
    mask[15] = 1;
    cipher.encrypt_block(&mut mask);
    let mut tag = auth.finalize();
    for (byte, mask) in tag.iter_mut().zip(mask) { *byte ^= mask; }
    Ok(tag.into())
}
fn crypt(cipher: &aes::Aes256, bytes: &[u8], cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    bound(bytes.len(), MAX_PRESET_BYTES, "boundedPresetPlaintext")?;
    let mut output = Vec::new();
    output.try_reserve_exact(bytes.len()).map_err(|_| decline("boundedPresetAllocation"))?;
    for (index, bytes) in bytes.chunks(16).enumerate() {
        if index % (CHUNK / 16) == 0 { check(cancel)?; }
        let mut block = aes::cipher::Block::<aes::Aes256>::default();
        block[12..].copy_from_slice(&((index as u32) + 2).to_be_bytes());
        cipher.encrypt_block(&mut block);
        output.extend(bytes.iter().zip(block).map(|(byte, mask)| byte ^ mask));
    }
    Ok(output)
}
fn encrypt(bytes: &[u8], cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    let cipher = cipher();
    let mut output = crypt(&cipher, bytes, cancel)?;
    let tag = auth(&cipher, &output, cancel)?;
    append(&mut output, &tag, MAX_PRESET_BYTES + 16)?;
    Ok(output)
}
fn decrypt(bytes: &[u8], cancel: &impl Fn() -> bool) -> Result<Vec<u8>, StageError> {
    let split = bytes.len().checked_sub(16).ok_or_else(|| decline("presetAuthentication"))?;
    bound(split, MAX_PRESET_BYTES, "boundedPresetCiphertext")?;
    let cipher = cipher();
    let tag = auth(&cipher, &bytes[..split], cancel)?;
    if !bool::from(tag.as_slice().ct_eq(&bytes[split..])) { return Err(decline("presetAuthentication")); }
    crypt(&cipher, &bytes[..split], cancel)
}

enum Node<'a> { Data(Value), Binary(&'a [u8]), Undefined }
impl Node<'_> {
    fn data(self) -> Result<Value, StageError> {
        match self { Self::Data(value) => Ok(value), Self::Binary(_) => Err(decline("presetMessagePackBinary")), Self::Undefined => Err(decline("presetMessagePackExtension")) }
    }
}
struct Reader<'a, 'b, C> { bytes: &'a [u8], at: usize, budget: &'b Budget, cancel: &'b C, values: usize }
impl<'a, 'b, C: Fn() -> bool> Reader<'a, 'b, C> {
    fn new(bytes: &'a [u8], budget: &'b Budget, cancel: &'b C) -> Self { Self { bytes, at: 0, budget, cancel, values: 0 } }
    fn read(&mut self, count: usize) -> Result<&'a [u8], StageError> {
        check(self.cancel)?;
        self.budget.work(count.saturating_add(1))?;
        let end = self.at.checked_add(count).ok_or_else(|| decline("presetMessagePack"))?;
        let bytes = self.bytes.get(self.at..end).ok_or_else(|| decline("presetMessagePack"))?;
        self.at = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8, StageError> { Ok(self.read(1)?[0]) }
    fn integer(&mut self, count: usize) -> Result<u64, StageError> {
        Ok(self.read(count)?.iter().fold(0u64, |value, byte| (value << 8) | *byte as u64))
    }
    fn map_length(&mut self) -> Result<usize, StageError> {
        let marker = self.byte()?;
        self.length(marker, true)
    }
    fn length(&mut self, marker: u8, map: bool) -> Result<usize, StageError> {
        let count = match (map, marker) {
            (true, 0x80..=0x8f) => (marker & 15) as u64,
            (false, 0x90..=0x9f) => (marker & 15) as u64,
            (true, 0xde) | (false, 0xdc) => self.integer(2)?,
            (true, 0xdf) | (false, 0xdd) => self.integer(4)?,
            _ => return Err(decline("presetMessagePack")),
        };
        bound(count as usize, MAX_VALUES, "boundedPresetValues")?;
        Ok(count as usize)
    }
    fn string(&mut self, marker: u8) -> Result<Text, StageError> {
        let length = match marker { 0xa0..=0xbf => (marker & 31) as usize,
            0xd9 => self.integer(1)? as usize, 0xda => self.integer(2)? as usize, 0xdb => self.integer(4)? as usize,
            _ => return Err(decline("presetMessagePackKey")) };
        bound(length, MAX_PRESET_BYTES, "boundedPresetText")?;
        let text = std::str::from_utf8(self.read(length)?).map_err(|_| decline("presetMessagePackUtf8"))?;
        Ok(Text::from_str(text, self.budget)?)
    }
    fn key(&mut self) -> Result<Text, StageError> {
        let marker = self.byte()?;
        let key = self.string(marker)?;
        if key.equals_ascii(b"__proto__") { return Err(decline("presetMessagePackProtoKey")); }
        Ok(key)
    }
    fn value(&mut self, depth: usize) -> Result<Node<'a>, StageError> {
        bound(depth, MAX_DEPTH, "boundedPresetDepth")?;
        self.values += 1;
        bound(self.values, MAX_VALUES, "boundedPresetValues")?;
        let marker = self.byte()?;
        let value = match marker {
            0x00..=0x7f => Value::Number(marker as f64),
            0xe0..=0xff => Value::Number((marker as i8) as f64),
            0xc0 => Value::Null, 0xc2 => Value::Bool(false), 0xc3 => Value::Bool(true),
            0xcc => Value::Number(self.integer(1)? as f64), 0xcd => Value::Number(self.integer(2)? as f64),
            0xce => Value::Number(self.integer(4)? as f64),
            0xd0 => Value::Number((self.integer(1)? as i8) as f64),
            0xd1 => Value::Number((self.integer(2)? as i16) as f64),
            0xd2 => Value::Number((self.integer(4)? as i32) as f64),
            0xca | 0xcb => {
                let number = if marker == 0xca { f32::from_bits(self.integer(4)? as u32) as f64 }
                    else { f64::from_bits(self.integer(8)?) };
                if !number.is_finite() { return Err(decline("presetMessagePackNonfinite")); }
                Value::Number(number)
            },
            0xa0..=0xbf | 0xd9..=0xdb => Value::Text(self.string(marker)?),
            0xc4..=0xc6 => {
                let length = self.integer(1 << (marker - 0xc4))? as usize;
                bound(length, MAX_PRESET_BYTES + 16, "boundedPresetCiphertext")?;
                return Ok(Node::Binary(self.read(length)?));
            },
            0x90..=0x9f | 0xdc | 0xdd => {
                let count = self.length(marker, false)?;
                let mut values = OwnedVec::new(self.budget)?;
                for _ in 0..count { values.push(self.value(depth + 1)?.data()?)?; }
                Value::Array(values)
            },
            0x80..=0x8f | 0xde | 0xdf => {
                let count = self.length(marker, true)?;
                let mut object = Value::Object(OwnedVec::new(self.budget)?);
                for _ in 0..count {
                    let key = self.key()?;
                    let value = self.value(depth + 1)?.data()?;
                    object.define(key, value, self.budget)?;
                }
                object
            },
            0xd4 => {
                let extension = self.read(2)?;
                if extension[0] == 0 { return Ok(Node::Undefined); }
                return Err(decline("presetMessagePackExtension"));
            },
            0xcf | 0xd3 => return Err(decline("presetMessagePackBigInt")),
            _ => return Err(decline("presetMessagePackExtension")),
        };
        Ok(Node::Data(value))
    }
    fn finish(&self) -> Result<(), StageError> {
        if self.at != self.bytes.len() { Err(decline("presetMessagePackTrailing")) } else { check(self.cancel) }
    }
}

struct Writer<'a, C> { bytes: Vec<u8>, budget: &'a Budget, cancel: &'a C, values: usize }
impl<C: Fn() -> bool> Writer<'_, C> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), StageError> {
        check(self.cancel)?;
        self.budget.work(bytes.len().saturating_add(1))?;
        append(&mut self.bytes, bytes, MAX_PRESET_BYTES)
    }
    fn header(&mut self, count: usize, small: u8, medium: u8, large: u8) -> Result<(), StageError> {
        if count < 16 { self.write(&[small | count as u8]) }
        else if count <= u16::MAX as usize { self.write(&[medium])?; self.write(&(count as u16).to_be_bytes()) }
        else { self.write(&[large])?; self.write(&(count as u32).to_be_bytes()) }
    }
    fn string(&mut self, text: &Text) -> Result<(), StageError> {
        let text = text.as_unicode().ok_or_else(|| decline("presetMessagePackUtf16"))?;
        let length = text.len();
        bound(length, MAX_PRESET_BYTES, "boundedPresetText")?;
        if length < 32 { self.write(&[0xa0 | length as u8])?; }
        else if length <= u8::MAX as usize { self.write(&[0xd9, length as u8])?; }
        else if length <= u16::MAX as usize { self.write(&[0xda])?; self.write(&(length as u16).to_be_bytes())?; }
        else { self.write(&[0xdb])?; self.write(&(length as u32).to_be_bytes())?; }
        self.write(text.as_bytes())
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), StageError> {
        bound(depth, MAX_DEPTH, "boundedPresetDepth")?;
        self.values += 1;
        bound(self.values, MAX_VALUES, "boundedPresetValues")?;
        match value {
            Value::Null => self.write(&[0xc0]), Value::Bool(value) => self.write(&[if *value { 0xc3 } else { 0xc2 }]),
            Value::Number(number) if number.is_finite() => {
                if *number >= 0.0 && *number <= u32::MAX as f64 && number.fract() == 0.0 {
                    let number = *number as u32;
                    if number < 128 { self.write(&[number as u8]) }
                    else { self.write(&[0xce])?; self.write(&number.to_be_bytes()) }
                } else if *number >= i32::MIN as f64 && *number < 0.0 && number.fract() == 0.0 {
                    self.write(&[0xd2])?; self.write(&(*number as i32).to_be_bytes())
                } else { self.write(&[0xcb])?; self.write(&number.to_be_bytes()) }
            },
            Value::Text(text) => self.string(text),
            Value::Array(values) => {
                self.header(values.as_slice().len(), 0x90, 0xdc, 0xdd)?;
                for value in values.as_slice() { self.value(value, depth + 1)?; }
                Ok(())
            },
            Value::Object(values) => {
                self.header(values.as_slice().len(), 0x80, 0xde, 0xdf)?;
                for (key, value) in values.as_slice() { self.string(key)?; self.value(value, depth + 1)?; }
                Ok(())
            },
            _ => Err(decline("presetMessagePackValue")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use risunest_formats::{ids::Ids, preset::{export_preset, import_preset}};
    use risunest_ir::{json::{parse_json, stringify}, Limits};
    use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

    const CASES: &str = include_str!("../../../tests/fixtures/rust-core/formats/preset-codec.cases.json");
    const EXPECTED: &str = include_str!("../../../tests/fixtures/rust-core/formats/preset-codec.expected.json");
    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 28, max_json_bytes: 1 << 27, max_text_units: 1 << 27,
            max_values: 1 << 24, max_depth: 128, max_work: usize::MAX / 4 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn text(value: &Value, budget: &Budget) -> String { stringify(value, budget).unwrap().unwrap().as_unicode().unwrap().into() }
    struct FixedId;
    impl Ids for FixedId {
        fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
            Ok(Text::from_str("00000000-0000-4000-8000-000000000001", budget)?)
        }
    }
    fn fixtures() -> (serde_json::Value, serde_json::Value) {
        (serde_json::from_str(CASES).unwrap(), serde_json::from_str(EXPECTED).unwrap())
    }
    fn bytes(value: &serde_json::Value, key: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode(value[key].as_str().unwrap()).unwrap()
    }
    #[test]
    fn reads_ts_binary_and_json_fixtures_then_maps_the_actual_import() {
        let (cases, expected) = fixtures();
        let cases = cases["cases"].as_array().unwrap();
        let expected = expected["cases"].as_array().unwrap();
        assert_eq!(cases.len(), expected.len());
        let mut accepted = 0;
        let mut declined = 0;
        for (case, expected) in cases.iter().zip(expected) {
            assert_eq!(case["id"], expected["id"]);
            if case["kind"] == "export" { continue; }
            let result = &expected["result"];
            let budget = budget();
            let decoded = decode(&bytes(result, "bytesBase64"), result["name"].as_str().unwrap(), &budget, &|| false);
            if case.get("decline").is_some() {
                assert!(decoded.is_err(), "{} must retain the TS reader", case["id"]);
                declined += 1;
                continue;
            }
            let decoded = decoded.unwrap_or_else(|error| panic!("{}: {error:?}", case["id"]));
            assert_eq!(text(&decoded, &budget), result["decodedJson"].as_str().unwrap(), "{}", case["id"]);
            let imported = import_preset(&decoded, &mut FixedId, &budget).unwrap();
            assert_eq!(text(&imported, &budget), result["importedJson"].as_str().unwrap(), "{}", case["id"]);
            accepted += 1;
        }
        assert!(accepted >= 15 && declined >= 10, "accepted={accepted}, declined={declined}");
    }
    #[test]
    fn exports_exact_ts_json_and_binary_the_ts_reader_can_open() {
        let (cases, expected) = fixtures();
        let cases = cases["cases"].as_array().unwrap();
        let expected = expected["cases"].as_array().unwrap();
        let mut checked = 0;
        for (case, expected) in cases.iter().zip(expected) {
            if case["kind"] != "export" { continue; }
            let budget = budget();
            let raw = case["rawJson"].as_str().map(str::to_owned).unwrap_or_else(|| serde_json::to_string(&case["body"]).unwrap());
            let stored = parse_json(&raw, &budget).unwrap();
            let exported = export_preset(&stored, &budget).unwrap();
            let result = &expected["result"];
            assert_eq!(String::from_utf8(encode(&exported, "json", &budget, &|| false).unwrap()).unwrap(), result["json"].as_str().unwrap());
            if case["declineBinaryExport"] == true {
                assert!(matches!(encode(&exported, "risup", &budget, &|| false), Err(StageError::Unsupported(name)) if name == "presetMessagePackUtf16"));
            } else {
                let native = encode(&exported, "risup", &budget, &|| false).unwrap();
                let decoded = decode(&native, "native.risup", &budget, &|| false).unwrap();
                assert_eq!(text(&decoded, &budget), text(&exported, &budget));
                let ts = decode(&bytes(result, "binaryBase64"), "ts.risup", &budget, &|| false).unwrap();
                assert_eq!(text(&ts, &budget), text(&exported, &budget));
            }
            checked += 1;
        }
        assert_eq!(checked, 2);
    }
    #[test]
    fn compressed_data_depth_counts_and_crypto_are_bounded_and_cancellable() {
        let budget = budget();
        assert!(inflate(&deflate(b"12345678", &|| false).unwrap(), 7, &|| false).is_err());
        assert!(matches!(encode(&Value::Null, "risupreset", &budget, &|| false), Err(StageError::Unsupported(name)) if name == "presetFormat"));
        let compressed = deflate(b"12345678", &|| false).unwrap();
        assert!(matches!(inflate(&[compressed.as_slice(), &[0]].concat(), 8, &|| false), Err(StageError::Unsupported(name)) if name == "presetCompressionTrailing"));
        let nesting = [vec![0x91; MAX_DEPTH + 1], vec![0xc0]].concat();
        assert!(Reader::new(&nesting, &budget, &|| false).value(0).is_err());
        assert!(Reader::new(&[0xdd, 0xff, 0xff, 0xff, 0xff], &budget, &|| false).value(0).is_err());
        let cancelled = || true;
        assert!(matches!(decode(b"{}", "x.json", &budget, &cancelled), Err(StageError::Cancelled)));
        assert!(matches!(encrypt(b"payload", &cancelled), Err(StageError::Cancelled)));
        assert!(matches!(inflate(&[], 10, &cancelled), Err(StageError::Cancelled)));
        let flag = Arc::new(AtomicBool::new(false));
        let live = Budget::new(Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 16, max_text_units: 1 << 16,
            max_values: 1 << 16, max_depth: 128, max_work: 1 << 20 }, flag.clone()).unwrap();
        flag.store(true, Ordering::Relaxed);
        assert!(matches!(decode(b"{}", "x.json", &live, &|| false), Err(StageError::Cancelled)));
        let encrypted = encrypt(b"payload", &|| false).unwrap();
        let mut tampered = encrypted.clone();
        tampered[0] ^= 1;
        assert!(decrypt(&tampered, &|| false).is_err());
        assert_eq!(decrypt(&encrypted, &|| false).unwrap(), b"payload");
    }
    #[test]
    #[ignore = "writes synthetic native codec outputs for the TS cross-reader; requires RUST_CORE_PRESET_CODEC_NATIVE_DIR"]
    fn write_native_codec_outputs_for_ts_cross_reader() {
        let directory = std::env::var_os("RUST_CORE_PRESET_CODEC_NATIVE_DIR").expect("synthetic output directory");
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let (cases, _) = fixtures();
        let mut checked = 0;
        for case in cases["cases"].as_array().unwrap() {
            if case["kind"] != "export" { continue; }
            let budget = budget();
            let raw = case["rawJson"].as_str().map(str::to_owned).unwrap_or_else(|| serde_json::to_string(&case["body"]).unwrap());
            let stored = parse_json(&raw, &budget).unwrap();
            let exported = export_preset(&stored, &budget).unwrap();
            let id = case["id"].as_str().unwrap();
            std::fs::write(directory.join(format!("{id}.json")), encode(&exported, "json", &budget, &|| false).unwrap()).unwrap();
            if case["declineBinaryExport"] != true {
                std::fs::write(directory.join(format!("{id}.risup")), encode(&exported, "risup", &budget, &|| false).unwrap()).unwrap();
            }
            checked += 1;
        }
        assert_eq!(checked, 2);
    }
}
