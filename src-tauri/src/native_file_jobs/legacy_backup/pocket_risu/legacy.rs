use super::*;
use std::io::{self, BufRead, BufWriter, Seek, SeekFrom, Write};

fn malformed() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "invalid legacy Inlay JSON") }
fn parse_error(error: io::Error, cancellation: &dyn CancellationProbe) -> LocalBackupError {
    if error.kind() == io::ErrorKind::InvalidData { LocalBackupError::new(super::super::LocalBackupErrorCode::UnsupportedFormat, "PocketRisu Inlay data is unreadable") }
    else { super::super::cancellation_io(error, cancellation) }
}
fn byte(reader: &mut impl BufRead) -> io::Result<u8> {
    let value = *reader.fill_buf()?.first().ok_or_else(malformed)?;
    reader.consume(1);
    Ok(value)
}
fn space(reader: &mut impl BufRead) -> io::Result<()> {
    while reader.fill_buf()?.first().is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) { reader.consume(1); }
    Ok(())
}
fn hex4(reader: &mut impl BufRead) -> io::Result<u16> {
    let mut value = 0;
    for _ in 0..4 { value = value * 16 + (byte(reader)? as char).to_digit(16).ok_or_else(malformed)? as u16; }
    Ok(value)
}
fn string(reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<()> {
    if byte(reader)? != b'"' { return Err(malformed()); }
    let mut high: Option<u32> = None;
    loop {
        let b = byte(reader)?;
        let code = match b {
            b'"' => {
                if high.is_some() { writer.write_all("\u{fffd}".as_bytes())?; }
                return Ok(());
            }
            b'\\' => match byte(reader)? {
                b'"' => 34, b'\\' => 92, b'/' => 47,
                b'b' => 8, b'f' => 12, b'n' => 10, b'r' => 13, b't' => 9,
                b'u' => u32::from(hex4(reader)?), _ => return Err(malformed()),
            },
            0..=31 => return Err(malformed()),
            32..=127 => u32::from(b),
            _ => {
                let size = match b { 0xc2..=0xdf => 2, 0xe0..=0xef => 3, 0xf0..=0xf4 => 4, _ => return Err(malformed()) };
                let mut encoded = [b, 0, 0, 0];
                for slot in &mut encoded[1..size] { *slot = byte(reader)?; }
                std::str::from_utf8(&encoded[..size]).map_err(|_| malformed())?.chars().next().unwrap() as u32
            }
        };
        if let Some(first) = high.take() {
            if (0xdc00..=0xdfff).contains(&code) {
                let joined = 0x10000 + ((first - 0xd800) << 10) + code - 0xdc00;
                writer.write_all(char::from_u32(joined).unwrap().encode_utf8(&mut [0; 4]).as_bytes())?;
                continue;
            }
            writer.write_all("\u{fffd}".as_bytes())?;
        }
        if (0xd800..=0xdbff).contains(&code) { high = Some(code); }
        else { writer.write_all(char::from_u32(code).unwrap_or('\u{fffd}').encode_utf8(&mut [0; 4]).as_bytes())?; }
    }
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > MAX_METADATA_BYTES as usize { return Err(malformed()); }
        self.0.extend_from_slice(bytes); Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
fn value(reader: &mut impl BufRead) -> io::Result<Value> {
    let mut output = Bounded(Vec::new());
    let mut depth = 0u32;
    let mut quoted = false;
    let mut escaped = false;
    while let Some(&b) = reader.fill_buf()?.first() {
        if !quoted && depth == 0 && matches!(b, b',' | b'}') { break; }
        reader.consume(1);
        output.write_all(&[b])?;
        if quoted {
            if escaped { escaped = false; } else if b == b'\\' { escaped = true; } else if b == b'"' { quoted = false; }
        } else {
            match b {
                b'"' => quoted = true,
                b'{' | b'[' => { depth += 1; if depth > 128 { return Err(malformed()); } },
                b'}' | b']' => depth = depth.checked_sub(1).ok_or_else(malformed)?,
                _ => {}
            }
        }
    }
    serde_json::from_slice(&output.0).map_err(|_| malformed())
}
fn extract(reader: &mut impl BufRead, data: &mut (impl Write + Seek)) -> io::Result<(Map<String, Value>, bool)> {
    if byte(reader)? != b'{' { return Err(malformed()); }
    let mut metadata = Map::new();
    let mut saw_data = false;
    space(reader)?;
    if reader.fill_buf()?.first() == Some(&b'}') { reader.consume(1); }
    else { loop {
        let mut key = Bounded(Vec::new());
        string(reader, &mut key)?;
        let key = String::from_utf8(key.0).map_err(|_| malformed())?;
        space(reader)?;
        if byte(reader)? != b':' { return Err(malformed()); }
        space(reader)?;
        if key == "data" && reader.fill_buf()?.first() == Some(&b'"') {
            data.seek(SeekFrom::Start(0))?;
            string(reader, data)?;
            saw_data = true;
        } else {
            if key == "data" { saw_data = false; data.seek(SeekFrom::Start(0))?; }
            metadata.insert(key, value(reader)?);
            if serde_json::to_vec(&metadata).map_err(|_| malformed())?.len() > MAX_METADATA_BYTES as usize { return Err(malformed()); }
        }
        space(reader)?;
        match byte(reader)? { b'}' => break, b',' => space(reader)?, _ => return Err(malformed()) }
    } }
    space(reader)?;
    if !reader.fill_buf()?.is_empty() { return Err(malformed()); }
    Ok((metadata, saw_data))
}
struct Base64Input<R> { reader: R, pending: [u8; 3], next: usize, length: usize, done: bool }
impl<R: BufRead> Base64Input<R> {
    fn new(reader: R) -> Self { Self { reader, pending: [0; 3], next: 0, length: 0, done: false } }
}
impl<R: BufRead> Read for Base64Input<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < output.len() {
            if self.next < self.length {
                output[written] = self.pending[self.next]; self.next += 1; written += 1;
                continue;
            }
            if self.done { break; }
            let mut bits = 0u32;
            let mut count = 0;
            while count < 4 {
                let Some(&b) = self.reader.fill_buf()?.first() else { self.done = true; break; };
                self.reader.consume(1);
                let value = match b {
                    b'A'..=b'Z' => b - b'A', b'a'..=b'z' => b - b'a' + 26, b'0'..=b'9' => b - b'0' + 52,
                    b'+' | b'-' => 62, b'/' | b'_' => 63,
                    b'=' => { self.done = true; break; }, _ => continue,
                };
                bits = (bits << 6) | u32::from(value); count += 1;
            }
            bits <<= (4 - count) * 6;
            self.pending = [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8];
            self.next = 0;
            self.length = if count < 2 { 0 } else { count - 1 };
        }
        Ok(written)
    }
}

pub(super) fn prepare(
    entry: &StagedLocalBackupEntry, id: &str, metadata: &MetadataIndex<'_>,
    cas: &super::super::PayloadCas, durable: &mut super::super::DurableCasJob,
    cancellation: &dyn CancellationProbe,
) -> Result<AssetAlias, LocalBackupError> {
    let file = open_staged(entry)?;
    let mut reader = BufReader::with_capacity(64 * 1024, CancellationReader::new(file, cancellation));
    space(&mut reader).map_err(|error| parse_error(error, cancellation))?;
    if reader.fill_buf().map_err(LocalBackupError::io)?.first() != Some(&b'{') {
        return raw(entry, id, metadata, cas, durable, cancellation);
    }
    let directory = entry.staged_path.as_deref().and_then(std::path::Path::parent).ok_or_else(|| invalid("Inlay is not staged"))?;
    let mut data = tempfile::NamedTempFile::new_in(directory).map_err(LocalBackupError::io)?;
    let mut writer = BufWriter::with_capacity(64 * 1024, data.as_file_mut());
    let extracted = extract(&mut reader, &mut writer);
    if extracted.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::InvalidData) && metadata.sidecars.contains_key(id) {
        drop(writer);
        return raw(entry, id, metadata, cas, durable, cancellation);
    }
    let (object, has_data) = extracted.map_err(|error| parse_error(error, cancellation))?;
    writer.flush().map_err(LocalBackupError::io)?;
    let length = writer.stream_position().map_err(LocalBackupError::io)?;
    drop(writer);
    data.as_file().set_len(length).map_err(LocalBackupError::io)?;
    if !has_data { return raw(entry, id, metadata, cas, durable, cancellation); }
    let decoded_metadata: Metadata = serde_json::from_value(Value::Object(object)).map_err(|_| invalid("PocketRisu Inlay metadata is invalid"))?;
    let mut payload = BufReader::with_capacity(64 * 1024, data.reopen().map_err(LocalBackupError::io)?);
    let prepared = if decoded_metadata.inlay_type == "signature" {
        durable.prepare_reader(cas, &mut CancellationReader::new(payload, cancellation), super::super::CasObjectRole::DirectObject)
    } else {
        if payload.fill_buf().map_err(LocalBackupError::io)?.starts_with(b"data:") {
            while byte(&mut payload).map_err(LocalBackupError::io)? != b',' {}
        }
        durable.prepare_reader(cas, &mut CancellationReader::new(Base64Input::new(payload), cancellation), super::super::CasObjectRole::DirectObject)
    }.map_err(|error| parse_error(error, cancellation))?;
    let mut retained = Map::new();
    if let Some(provenance) = metadata.provenance.get(id) {
        retained.insert("pocketRisu".to_owned(), Value::Object(read_metadata(provenance, cancellation)?));
    }
    finish_alias(id, &prepared.content_hash, prepared.byte_size, decoded_metadata, retained)
}

fn raw(
    entry: &StagedLocalBackupEntry, id: &str, metadata: &MetadataIndex<'_>,
    cas: &super::super::PayloadCas, durable: &mut super::super::DurableCasJob, cancellation: &dyn CancellationProbe,
) -> Result<AssetAlias, LocalBackupError> {
    let alias = super::prepare(entry, id, "", metadata, cancellation)?;
    durable.prepare_reader(cas, &mut CancellationReader::new(open_staged(entry)?, cancellation), super::super::CasObjectRole::DirectObject)
        .map_err(|error| super::super::cancellation_io(error, cancellation))?;
    Ok(alias)
}
