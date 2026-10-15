use super::super::invalid;
use crate::persistent_store::StoreResult;
use rusqlite::{params, Connection, MAIN_DB, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};

pub(crate) const MAX_VALUE_BYTES: u64 = risunest_external_storage_format::section::MAX_PLUGIN_VALUE_BYTES;
pub(crate) const PART_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const REQUEST_BYTES: u64 = 64 * 1024 * 1024;
const BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TextInfo {
    pub(crate) byte_length: u64,
    pub(crate) encoded_length: u64,
    pub(crate) hash: String,
}

fn bad_text() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "plugin device value is not canonical UTF-8 text") }
fn too_large() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "plugin device value is too large") }

pub(crate) fn encoded_length(value: &str) -> u64 {
    value.bytes().fold(2, |size, byte| size + match byte {
        b'"' | b'\\' | 8 | 9 | 10 | 12 | 13 => 2,
        0..=31 => 6,
        _ => 1,
    })
}

/// Each callback receives complete UTF-8 code points, including when the source
/// ends a read in the middle of a character.
pub(crate) fn visit_text(mut input: impl Read, mut visit: impl FnMut(&str) -> io::Result<()>) -> io::Result<()> {
    let mut buffer = vec![0; BUFFER_BYTES + 4];
    let mut held = 0;
    loop {
        let count = input.read(&mut buffer[held..BUFFER_BYTES])?;
        let length = held + count;
        if length == 0 { return Ok(()); }
        let valid = match std::str::from_utf8(&buffer[..length]) {
            Ok(_) => length,
            Err(error) if error.error_len().is_none() && count != 0 => error.valid_up_to(),
            Err(_) => return Err(bad_text()),
        };
        visit(std::str::from_utf8(&buffer[..valid]).map_err(|_| bad_text())?)?;
        buffer.copy_within(valid..length, 0);
        held = length - valid;
        if count == 0 { return if held == 0 { Ok(()) } else { Err(bad_text()) }; }
    }
}

struct HashedWriter<W> { output: W, hash: Sha256, length: u64, limit: u64 }
impl<W: Write> Write for HashedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.length + bytes.len() as u64 > self.limit { return Err(too_large()); }
        let count = self.output.write(bytes)?;
        self.hash.update(&bytes[..count]);
        self.length += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> { self.output.flush() }
}

pub(crate) fn encode(input: impl Read, output: impl Write, limit: u64) -> io::Result<TextInfo> {
    let mut output = HashedWriter { output, hash: Sha256::new(), length: 0, limit };
    output.write_all(b"\"")?;
    let mut byte_length = 0;
    let mut encoded = Vec::with_capacity(BUFFER_BYTES * 2);
    visit_text(input, |text| {
        encoded.clear();
        byte_length += text.len() as u64;
        let bytes = text.as_bytes();
        let mut start = 0;
        for (index, byte) in bytes.iter().copied().enumerate() {
            let escape: &[u8] = match byte {
                b'"' => b"\\\"", b'\\' => b"\\\\", 8 => b"\\b", 9 => b"\\t",
                10 => b"\\n", 12 => b"\\f", 13 => b"\\r", 0..=31 => b"", _ => continue,
            };
            encoded.extend_from_slice(&bytes[start..index]);
            if escape.is_empty() {
                const HEX: &[u8] = b"0123456789abcdef";
                encoded.extend_from_slice(&[b'\\', b'u', b'0', b'0', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]);
            } else { encoded.extend_from_slice(escape); }
            start = index + 1;
        }
        encoded.extend_from_slice(&bytes[start..]);
        output.write_all(&encoded)
    })?;
    output.write_all(b"\"")?;
    output.flush()?;
    Ok(TextInfo { byte_length, encoded_length: output.length, hash: hex::encode(output.hash.finalize()) })
}

struct HashedReader<R> { input: R, hash: Sha256, length: u64, limit: u64 }
impl<R: Read> Read for HashedReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.length += count as u64;
        if self.length > self.limit { return Err(too_large()); }
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}

/// Accept precisely serde_json's string encoding, without materializing the
/// encoded body or its decoded string. Other JSON values are not plugin text.
pub(crate) fn decode(input: impl Read, output: impl Write, limit: u64) -> io::Result<TextInfo> {
    let mut output = io::BufWriter::with_capacity(BUFFER_BYTES, output);
    let hashed = HashedReader { input, hash: Sha256::new(), length: 0, limit };
    let mut input = BufReader::with_capacity(BUFFER_BYTES, hashed);
    fn byte(input: &mut impl Read) -> io::Result<u8> {
        let mut one = [0]; input.read_exact(&mut one)?; Ok(one[0])
    }
    if byte(&mut input)? != b'"' { return Err(bad_text()); }
    let mut pending = Vec::with_capacity(BUFFER_BYTES + 4);
    let mut byte_length = 0u64;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() { return Err(bad_text()); }
        let count = available.iter().position(|byte| *byte == b'"' || *byte == b'\\' || *byte < 32).unwrap_or(available.len());
        pending.extend_from_slice(&available[..count]);
        let special = available.get(count).copied();
        input.consume(count);
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) if error.error_len().is_none() && special.is_none() => error.valid_up_to(),
            Err(_) => return Err(bad_text()),
        };
        output.write_all(&pending[..valid])?;
        byte_length += valid as u64;
        pending.copy_within(valid.., 0);
        pending.truncate(pending.len() - valid);
        let Some(special) = special else { continue; };
        input.consume(1);
        match special {
            b'"' => {
                if !input.fill_buf()?.is_empty() { return Err(bad_text()); }
                output.flush()?;
                let hashed = input.into_inner();
                return Ok(TextInfo { byte_length, encoded_length: hashed.length, hash: hex::encode(hashed.hash.finalize()) });
            }
            b'\\' => {
                let decoded = match byte(&mut input)? {
                    b'"' => b'"', b'\\' => b'\\', b'b' => 8, b't' => 9,
                    b'n' => 10, b'f' => 12, b'r' => 13,
                    b'u' => {
                        if byte(&mut input)? != b'0' || byte(&mut input)? != b'0' { return Err(bad_text()); }
                        let high = byte(&mut input)?;
                        let low = byte(&mut input)?;
                        let hex = |digit| match digit { b'0'..=b'9' => Ok(digit - b'0'), b'a'..=b'f' => Ok(digit - b'a' + 10), _ => Err(bad_text()) };
                        let decoded = hex(high)? * 16 + hex(low)?;
                        if decoded > 31 || matches!(decoded, 8 | 9 | 10 | 12 | 13) { return Err(bad_text()); }
                        decoded
                    }
                    _ => return Err(bad_text()),
                };
                output.write_all(&[decoded])?;
                byte_length += 1;
            }
            _ => return Err(bad_text()),
        }
    }
}

pub(crate) fn object_row(db: &Connection, hash: &str) -> StoreResult<(i64, u64)> {
    let (rowid, length): (i64, i64) = db.query_row("SELECT rowid,length(body) FROM message_page_objects WHERE hash=?1", [hash],
        |row| Ok((row.get(0)?, row.get(1)?))).optional()?.ok_or_else(|| invalid("unit-object-body-required"))?;
    Ok((rowid, u64::try_from(length).map_err(|_| invalid("message object size is invalid"))?))
}

pub(crate) fn inspect_object(db: &Connection, hash: &str) -> StoreResult<TextInfo> {
    let (rowid, length) = object_row(db, hash)?;
    if length > MAX_VALUE_BYTES { return Err(invalid("plugin device value is too large")); }
    let blob = db.blob_open(MAIN_DB, "message_page_objects", "body", rowid, true)?;
    let info = decode(blob, io::sink(), MAX_VALUE_BYTES)?;
    if info.hash != hash { return Err(invalid("message object hash mismatch")); }
    Ok(info)
}

pub(crate) fn store_encoded<R: Read>(db: &Connection, mut source: impl FnMut() -> StoreResult<R>) -> StoreResult<TextInfo> {
    let info = encode(source()?, io::sink(), MAX_VALUE_BYTES)?;
    let exists: Option<i64> = db.query_row("SELECT rowid FROM message_page_objects WHERE hash=?1", [&info.hash], |row| row.get(0)).optional()?;
    if exists.is_none() {
        db.execute("INSERT INTO message_page_objects(hash,body) VALUES(?1,zeroblob(?2))", params![info.hash, info.encoded_length as i64])?;
        let rowid = db.last_insert_rowid();
        let mut blob = db.blob_open(MAIN_DB, "message_page_objects", "body", rowid, false)?;
        let written = encode(source()?, &mut blob, MAX_VALUE_BYTES)?;
        blob.close()?;
        if written != info { return Err(invalid("plugin device value changed while encoding")); }
    }
    if inspect_object(db, &info.hash)? != info { return Err(invalid("message object identity collision")); }
    db.execute("INSERT INTO message_page_verified_objects(hash) SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM message_page_verified_objects WHERE hash=?1)", [&info.hash])?;
    db.execute("DELETE FROM message_page_object_marks WHERE hash=?1", [&info.hash])?;
    Ok(info)
}

pub(crate) fn copy_object(source: &Connection, target: &Connection, info: &TextInfo) -> StoreResult<()> {
    let (source_row, length) = object_row(source, &info.hash)?;
    if length != info.encoded_length || inspect_object(source, &info.hash)? != *info {
        return Err(invalid("message object identity collision"));
    }
    let exists: bool = target.query_row("SELECT EXISTS(SELECT 1 FROM message_page_objects WHERE hash=?1)", [&info.hash], |row| row.get(0))?;
    if !exists {
        target.execute("INSERT INTO message_page_objects(hash,body) VALUES(?1,zeroblob(?2))", params![info.hash, length as i64])?;
        let rowid = target.last_insert_rowid();
        let mut output = target.blob_open(MAIN_DB, "message_page_objects", "body", rowid, false)?;
        let mut input = source.blob_open(MAIN_DB, "message_page_objects", "body", source_row, true)?;
        if io::copy(&mut input, &mut output)? != length { return Err(invalid("message object is incomplete")); }
        output.close()?;
    }
    if inspect_object(target, &info.hash)? != *info { return Err(invalid("message object identity collision")); }
    target.execute("INSERT INTO message_page_verified_objects(hash) SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM message_page_verified_objects WHERE hash=?1)", [&info.hash])?;
    target.execute("DELETE FROM message_page_object_marks WHERE hash=?1", [&info.hash])?;
    Ok(())
}

pub(crate) fn range(db: &Connection, rowid: i64, start: u64, encoded_limit: usize) -> StoreResult<String> {
    let mut blob = db.blob_open(MAIN_DB, "plugin_device_storage", "value", rowid, true)?;
    let size = blob.len() as u64;
    if start > size || encoded_limit < 8 { return Err(invalid("plugin device range is invalid")); }
    blob.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0; (size - start).min(encoded_limit as u64) as usize];
    blob.read_exact(&mut bytes)?;
    let valid = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() && start + (bytes.len() as u64) < size => error.valid_up_to(),
        Err(_) => return Err(invalid("plugin device range is not UTF-8")),
    };
    bytes.truncate(valid);
    let mut text = String::from_utf8(bytes).map_err(|_| invalid("plugin device range is not UTF-8"))?;
    let mut length = 2usize;
    let end = text.char_indices().find_map(|(index, character)| {
        let mut buffer = [0; 4];
        length += encoded_length(character.encode_utf8(&mut buffer)) as usize - 2;
        (length > encoded_limit).then_some(index)
    });
    if let Some(end) = end { text.truncate(end); }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Split<'a>(&'a [u8], usize);
    impl Read for Split<'_> {
        fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
            let count = target.len().min(self.1).min(self.0.len());
            target[..count].copy_from_slice(&self.0[..count]);
            self.0 = &self.0[count..]; Ok(count)
        }
    }

    #[test]
    fn ranged_string_codec_matches_whole_units_and_rejects_noncanonical_or_oversized_bodies() {
        let text = format!("{}가😀\"\\\n{}", "a".repeat(BUFFER_BYTES - 2), (0..32).map(char::from).collect::<String>());
        let expected = serde_json::to_vec(&text).unwrap();
        for width in [1, 3, 7, BUFFER_BYTES] {
            let mut encoded = vec![];
            let info = encode(Split(text.as_bytes(), width), &mut encoded, expected.len() as u64).unwrap();
            assert_eq!(encoded, expected);
            assert_eq!(info.hash, risunest_sync_wire::hash(&expected));
            assert_eq!(info.encoded_length, encoded_length(&text));
            let mut decoded = vec![];
            assert_eq!(decode(Split(&encoded, width), &mut decoded, info.encoded_length).unwrap(), info);
            assert_eq!(decoded, text.as_bytes());
            assert!(encode(Split(text.as_bytes(), width), io::sink(), info.encoded_length - 1).is_err());
            assert!(decode(Split(&encoded, width), io::sink(), info.encoded_length - 1).is_err());
        }
        for body in [br#""\u0061""#.as_slice(), br#""\u000a""#, br#""\/""#, b"true", b"\"a\" ", b"\"\xff\"", b"\"truncated"] {
            assert!(decode(Split(body, 1), io::sink(), 256).is_err(), "{body:?}");
        }
    }
}
