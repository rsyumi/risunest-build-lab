use super::*;
use serde_json::{json, Value};

fn frame(header: serde_json::Value, body: &[u8]) -> Vec<u8> {
    let encoded = serde_json::to_vec(&header).expect("serialize header");
    let mut bytes = (encoded.len() as u32).to_le_bytes().to_vec();
    bytes.extend_from_slice(&encoded);
    bytes.extend_from_slice(body);
    bytes
}

fn vector_bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn write_entry(key: &str, dimensions: i64, byte_length: usize) -> serde_json::Value {
    json!({
        "key": key,
        "producer": "hypa-v2",
        "model": "MiniLM",
        "endpoint": null,
        "preprocessVersion": 1,
        "dimensions": dimensions,
        "byteLength": byte_length,
        "metadata": null,
    })
}

#[test]
fn a_write_frame_splits_the_vector_region_in_header_order() {
    let body = [vector_bytes(&[1.0, 2.0]), vector_bytes(&[3.0])].concat();
    let entries = decode_write_frame(&frame(
        json!({ "entries": [write_entry("key-a", 2, 8), write_entry("key-b", 1, 4)] }),
        &body,
    ))
    .expect("decode write frame");

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].cache_key, "key-a");
    assert_eq!(entries[0].vector, vector_bytes(&[1.0, 2.0]));
    assert_eq!(entries[1].cache_key, "key-b");
    assert_eq!(entries[1].vector, vector_bytes(&[3.0]));
}

#[test]
fn a_write_frame_with_a_short_or_long_vector_region_is_rejected() {
    let header = json!({ "entries": [write_entry("key-a", 2, 8)] });
    assert!(decode_write_frame(&frame(header.clone(), &vector_bytes(&[1.0]))).is_err());
    assert!(decode_write_frame(&frame(header, &vector_bytes(&[1.0, 2.0, 3.0]))).is_err());
}

#[test]
fn a_write_frame_whose_byte_length_contradicts_the_dimensions_is_rejected() {
    assert!(decode_write_frame(&frame(
        json!({ "entries": [write_entry("key-a", 2, 4)] }),
        &vector_bytes(&[1.0]),
    ))
    .is_err());
}

#[test]
fn a_truncated_or_oversized_header_length_is_rejected() {
    assert!(decode_write_frame(&[0, 0, 0]).is_err());
    assert!(decode_write_frame(&[255, 255, 255, 255]).is_err());
}

#[test]
fn a_read_frame_reports_a_miss_as_an_empty_vector() {
    let built = build_frame(
        &ReadHeader {
            entries: vec![
                ReadHeaderEntry {
                    key: "key-a".to_owned(),
                    dimensions: 2,
                    byte_length: 8,
                },
                ReadHeaderEntry {
                    key: "missing".to_owned(),
                    dimensions: 0,
                    byte_length: 0,
                },
            ],
        },
        vec![vector_bytes(&[1.0, 2.0]), Vec::new()],
    )
    .expect("build read frame");

    let (header_bytes, body) = split_frame(&built).expect("split read frame");
    let header: serde_json::Value =
        serde_json::from_slice(header_bytes).expect("parse read header");
    assert_eq!(
        header,
        json!({ "entries": [
            { "key": "key-a", "dimensions": 2, "byteLength": 8 },
            { "key": "missing", "dimensions": 0, "byteLength": 0 },
        ]})
    );
    assert_eq!(body, vector_bytes(&[1.0, 2.0]));
}

#[test]
fn a_base64_json_body_decodes_to_the_same_frame_as_a_raw_body() {
    let body = vector_bytes(&[1.0, 2.0]);
    let bytes = frame(json!({ "entries": [write_entry("key-a", 2, 8)] }), &body);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let decoded: EncodedWrite =
        serde_json::from_value(json!({ "payload": encoded })).expect("parse encoded write");
    let recovered = base64::engine::general_purpose::STANDARD
        .decode(decoded.payload.as_bytes())
        .expect("decode payload");
    assert_eq!(recovered, bytes);
    assert_eq!(
        decode_write_frame(&recovered).expect("decode write frame")[0].vector,
        body
    );
}

pub(super) fn frame_sets() -> (Value, Value) {
    use sha2::{Digest, Sha256};
    let cases = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/memory/hypa-native-cache/frames.cases.json"));
    let expected: Value = serde_json::from_slice(include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/fixtures/rust-core/memory/hypa-native-cache/frames.expected.json"))).unwrap();
    assert_eq!(expected["fixtureSha256"], hex::encode(Sha256::digest(cases)));
    (serde_json::from_slice(cases).unwrap(), expected)
}
pub(super) fn fixture_text(value: &Value) -> Result<String, std::string::FromUtf16Error> {
    if let Some(value) = value.as_str() { return Ok(value.to_owned()); }
    String::from_utf16(&value["u"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u16).collect::<Vec<_>>())
}

#[test]
fn actual_ts_frame_bytes_reach_the_native_string_decoder() {
    let (cases, expected) = frame_sets();
    for (case, observed) in cases["cases"].as_array().unwrap().iter().zip(expected["cases"].as_array().unwrap()) {
        assert_eq!(case["id"], observed["id"]);
        if case["input"]["mode"] != "write" { continue; }
        let entries = case["input"]["entries"].as_array().unwrap();
        let bytes = hex::decode(observed["result"]["frameHex"].as_str().unwrap()).unwrap();
        assert_eq!(bytes.len() as u64, observed["result"]["frameBytes"].as_u64().unwrap());
        let expected_valid = entries.iter().all(|entry| fixture_text(&entry["model"]).is_ok()
            && (entry["endpoint"].is_null() || fixture_text(&entry["endpoint"]).is_ok()));
        let result = decode_write_frame(&bytes);
        if !expected_valid {
            assert!(result.is_err(), "{}", case["id"]);
            continue;
        }
        let decoded = result.unwrap();
        assert_eq!(decoded.len(), entries.len());
        for (row, entry) in decoded.iter().zip(entries) {
            assert_eq!(row.cache_key, entry["key"].as_str().unwrap());
            assert_eq!(row.producer, "hypa-v1-text");
            assert_eq!(row.model, fixture_text(&entry["model"]).unwrap());
            assert_eq!(row.endpoint, (!entry["endpoint"].is_null()).then(|| fixture_text(&entry["endpoint"]).unwrap()));
            assert_eq!(row.preprocess_version, 1);
            assert!(row.metadata.is_none());
            let bits = entry["bits"].as_array().unwrap().iter().map(|value| value.as_u64().unwrap() as u32).collect::<Vec<_>>();
            assert_eq!(row.dimensions as usize, bits.len());
            assert_eq!(row.vector, bits.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<_>>());
        }
    }
}

#[test]
fn write_frame_admission_is_inclusive_and_only_applies_at_ingress() {
    let bytes = frame(json!({"entries":[]}), &[]);
    assert!(split_frame(&bytes).is_ok());
    assert!(validate_frame_length(bytes.len(), bytes.len() - 1).is_err());
    assert!(validate_frame_length(bytes.len(), bytes.len()).is_ok());
    assert!(validate_frame_length(bytes.len(), bytes.len() + 1).is_ok());
    assert!(validate_frame_length(MAX_FRAME_BYTES, MAX_FRAME_BYTES).is_ok());
    assert!(validate_frame_length(MAX_FRAME_BYTES + 1, MAX_FRAME_BYTES).is_err());
    assert!(validate_frame_length(HEADER_LENGTH_BYTES - 1, MAX_FRAME_BYTES).is_err());
}
