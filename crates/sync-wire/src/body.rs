//! zstd request and response bodies. HTTP framing carries the encoded bytes;
//! every protocol size, offset, hash and limit still refers to the raw body.
use super::{Result, WireError};
use std::io::Read;

/// Marks a body that is one zstd frame.
pub const ENCODING_HEADER: &str = "x-risu-body-encoding";
/// Sent by a client that decodes encoded replies.
pub const ACCEPT_HEADER: &str = "x-risu-accept-body-encoding";
pub const ZSTD: &str = "zstd";
/// Smaller bodies are always sent raw.
pub const MIN_ENCODED_BYTES: usize = 1024;
/// What an encoded body adds on an HTTP/1.1 hop: the marker line and, on a
/// reply, the longer content type and cache control values.
pub const HEADER_MARGIN: usize = 64;
const LEVEL: i32 = 1;
const WINDOW_LOG: u32 = 20;
const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Whether the marker values describe an encoded body. A repeated, empty or
/// unknown marker is an error, never a raw body.
pub fn marked<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Result<bool> {
    let mut values = values.into_iter();
    match (values.next(), values.next()) {
        (None, _) => Ok(false),
        (Some(value), None) if value == ZSTD.as_bytes() => Ok(true),
        _ => Err(WireError("invalid-body-encoding")),
    }
}

/// Whether a request asks for encoded replies.
pub fn accepted<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> bool {
    let mut values = values.into_iter();
    matches!((values.next(), values.next()), (Some(value), None) if value == ZSTD.as_bytes())
}

/// The encoded body, or `None` when the raw body should travel as it is.
pub fn encode(raw: &[u8]) -> Result<Option<Vec<u8>>> {
    if raw.len() < MIN_ENCODED_BYTES {
        return Ok(None);
    }
    let failed = |_| WireError("body-encoding-failed");
    let mut compressor = zstd::bulk::Compressor::new(LEVEL).map_err(failed)?;
    compressor
        .set_parameter(zstd::zstd_safe::CParameter::WindowLog(WINDOW_LOG))
        .map_err(failed)?;
    let encoded = compressor.compress(raw).map_err(failed)?;
    Ok((encoded.len() + HEADER_MARGIN < raw.len()).then_some(encoded))
}

/// Decodes one frame whose raw body is at most `limit` bytes. A kept encoding
/// is always smaller than its raw body, so the encoded input shares the limit.
pub fn decode(encoded: &[u8], limit: usize) -> Result<Vec<u8>> {
    if encoded.len() > limit {
        return Err(WireError("body-too-large"));
    }
    let invalid = |_| WireError("invalid-body-encoding");
    // Reject skippable frames, concatenated frames and bytes after the end.
    if !encoded.starts_with(&MAGIC) {
        return Err(WireError("invalid-body-encoding"));
    }
    let mut decoder = zstd::stream::read::Decoder::with_buffer(encoded)
        .map_err(invalid)?
        .single_frame();
    decoder.window_log_max(WINDOW_LOG).map_err(invalid)?;
    let mut raw = Vec::new();
    decoder
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(invalid)?;
    if raw.len() > limit {
        return Err(WireError("body-too-large"));
    }
    if !decoder.finish().is_empty() {
        return Err(WireError("invalid-body-encoding"));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(len: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(len);
        let mut index = 0u64;
        while bytes.len() < len {
            bytes.extend_from_slice(format!("{{\"role\":\"user\",\"data\":\"synthetic line {index}\"}},").as_bytes());
            index += 1;
        }
        bytes.truncate(len);
        bytes
    }

    fn random(len: usize) -> Vec<u8> {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    fn frame(raw: &[u8], window_log: u32) -> Vec<u8> {
        let mut compressor = zstd::bulk::Compressor::new(LEVEL).unwrap();
        compressor
            .set_parameter(zstd::zstd_safe::CParameter::WindowLog(window_log))
            .unwrap();
        compressor.compress(raw).unwrap()
    }

    #[test]
    fn text_round_trips_and_shrinks() {
        let raw = text(512 * 1024);
        let encoded = encode(&raw).unwrap().unwrap();
        assert!(encoded.len() * 2 < raw.len());
        assert_eq!(decode(&encoded, raw.len()).unwrap(), raw);
    }

    #[test]
    fn small_and_incompressible_bodies_stay_raw() {
        assert_eq!(encode(&text(MIN_ENCODED_BYTES - 1)).unwrap(), None);
        assert_eq!(encode(&random(1024 * 1024)).unwrap(), None);
        // Saving less than the header margin is not worth a marker.
        let mut barely = random(4096);
        barely.extend_from_slice(&[0; HEADER_MARGIN / 2]);
        assert_eq!(encode(&barely).unwrap(), None);
        assert!(encode(&text(MIN_ENCODED_BYTES)).unwrap().is_some());
    }

    #[test]
    fn decode_enforces_the_raw_limit_on_input_and_output() {
        let raw = text(256 * 1024);
        let encoded = encode(&raw).unwrap().unwrap();
        assert_eq!(decode(&encoded, raw.len() - 1), Err(WireError("body-too-large")));
        assert_eq!(decode(&encoded, encoded.len() - 1), Err(WireError("body-too-large")));
        // A bomb: a tiny frame that inflates past its limit.
        let bomb = frame(&vec![0; 4 * 1024 * 1024], WINDOW_LOG);
        assert!(bomb.len() < 1024 * 1024);
        assert_eq!(decode(&bomb, 1024 * 1024), Err(WireError("body-too-large")));
    }

    #[test]
    fn decode_rejects_malformed_frames() {
        let raw = text(64 * 1024);
        let encoded = encode(&raw).unwrap().unwrap();
        let limit = raw.len();
        let invalid = Err(WireError("invalid-body-encoding"));
        assert_eq!(decode(&encoded[..encoded.len() - 1], limit), invalid);
        assert_eq!(decode(&[], limit), invalid);
        assert_eq!(decode(&raw[..1024], limit), invalid);
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing, limit), invalid);
        let mut concatenated = encoded.clone();
        concatenated.extend_from_slice(&encoded);
        assert_eq!(decode(&concatenated, limit * 2), invalid);
        let mut skippable = vec![0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 1, 2, 3, 4];
        skippable.extend_from_slice(&encoded);
        assert_eq!(decode(&skippable, limit), invalid);
        let wide = frame(&text(4 * 1024 * 1024), 22);
        assert_eq!(decode(&wide, 4 * 1024 * 1024), invalid);
    }

    #[test]
    fn markers_must_be_exactly_one_zstd_value() {
        assert_eq!(marked([]), Ok(false));
        assert_eq!(marked([b"zstd".as_slice()]), Ok(true));
        for values in [
            vec![b"".as_slice()],
            vec![b"gzip".as_slice()],
            vec![b"ZSTD".as_slice()],
            vec![b"zstd".as_slice(), b"zstd".as_slice()],
        ] {
            assert_eq!(marked(values), Err(WireError("invalid-body-encoding")));
        }
        assert!(accepted([b"zstd".as_slice()]));
        assert!(!accepted([]));
        assert!(!accepted([b"zstd".as_slice(), b"zstd".as_slice()]));
        assert!(!accepted([b"gzip".as_slice()]));
    }
}
