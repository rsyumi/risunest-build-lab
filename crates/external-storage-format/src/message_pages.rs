//! Deterministic content-defined message pages and their ordered manifest.
use crate::logical_records::{
    encoded_object, invalid, EncodedLogicalObject, LogicalRecordError, LOGICAL_MESSAGE_PAGE_SIZE,
};
use risunest_sync_wire::{payload_value, stamp::DecimalU64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ops::Range;

pub const MAX_PAGE_BYTES: usize = 256 * 1024;
pub const MIN_PAGE_MESSAGES: usize = 16;
pub const MANIFEST_SCHEMA: &str = "risunest.message-manifest/v1";
pub const PAGE_PREFIX: &[u8] = b"{\"schema\":\"risunest.logical-message-page/v1\",\"messages\":[";
pub const PAGE_SUFFIX: &[u8] = b"]}";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageHash {
    pub hash: String,
    pub byte_length: u64,
}

impl MessageHash {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            hash: hex::encode(Sha256::digest(bytes)),
            byte_length: bytes.len() as u64,
        }
    }
    pub fn boundary(&self) -> Result<bool, LogicalRecordError> {
        crate::logical_records::validate_hash(&self.hash, "message hash")?;
        let first = u16::from_str_radix(&self.hash[..4], 16)
            .map_err(|_| invalid("invalid message hash"))?;
        Ok(first & 31 == 0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestPage {
    pub hash: String,
    #[serde(with = "decimal_u32")]
    pub message_count: u32,
    pub byte_length: DecimalU64,
}

mod decimal_u32 {
    use risunest_sync_wire::stamp::DecimalU64;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &u32, serializer: S) -> Result<S::Ok, S::Error> {
        DecimalU64(*value as u64).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        u32::try_from(DecimalU64::deserialize(deserializer)?.0).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageManifest {
    pub schema: String,
    pub message_count: DecimalU64,
    pub pages: Vec<ManifestPage>,
}

impl MessageManifest {
    pub fn encode(&self) -> Result<EncodedLogicalObject, LogicalRecordError> {
        self.validate()?;
        encoded_object(
            payload_value::encode(&serde_json::to_value(self).map_err(|e| invalid(e.to_string()))?)
                .map_err(|e| invalid(e.to_string()))?,
        )
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, LogicalRecordError> {
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| invalid("invalid message manifest"))?;
        if value.encode()?.bytes != bytes {
            return Err(invalid("noncanonical message manifest"));
        }
        Ok(value)
    }
    pub fn validate(&self) -> Result<(), LogicalRecordError> {
        if self.schema != MANIFEST_SCHEMA {
            return Err(invalid("unsupported message manifest schema"));
        }
        let mut count = 0u64;
        for page in &self.pages {
            crate::logical_records::validate_hash(&page.hash, "message page hash")?;
            if page.message_count == 0
                || page.message_count as usize > LOGICAL_MESSAGE_PAGE_SIZE
                || page.byte_length.0 < (PAGE_PREFIX.len() + PAGE_SUFFIX.len()) as u64
                || (page.byte_length.0 > MAX_PAGE_BYTES as u64 && page.message_count != 1)
            {
                return Err(invalid("invalid message manifest page"));
            }
            count = count
                .checked_add(page.message_count as u64)
                .ok_or_else(|| invalid("message count overflow"))?;
        }
        if count != self.message_count.0 {
            return Err(invalid("message manifest count mismatch"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageBoundary {
    pub start: usize,
    pub page: ManifestPage,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageIndex {
    pub messages: Vec<MessageHash>,
    pub pages: Vec<PageBoundary>,
}

impl PageIndex {
    pub fn manifest(&self) -> MessageManifest {
        MessageManifest {
            schema: MANIFEST_SCHEMA.into(),
            message_count: DecimalU64(self.messages.len() as u64),
            pages: self.pages.iter().map(|p| p.page.clone()).collect(),
        }
    }
    pub fn validate(&self) -> Result<(), LogicalRecordError> {
        self.manifest().validate()?;
        let mut start = 0;
        for boundary in &self.pages {
            if boundary.start != start {
                return Err(invalid("noncontiguous message page index"));
            }
            start += boundary.page.message_count as usize;
        }
        for message in &self.messages {
            message.boundary()?;
        }
        Ok(())
    }
}

pub struct Repaged {
    pub index: PageIndex,
    pub objects: Vec<EncodedLogicalObject>,
}

/// The caller supplies hashes for the new list and the exact replaced old range.
/// Bodies are requested only for pages rebuilt between the retained boundaries.
pub fn repage<E: From<LogicalRecordError>>(
    previous: &PageIndex,
    messages: Vec<MessageHash>,
    replaced: Range<usize>,
    replacement_end: usize,
    mut read: impl FnMut(usize) -> Result<Vec<u8>, E>,
) -> Result<Repaged, E> {
    previous.validate()?;
    if replaced.start > replaced.end
        || replaced.end > previous.messages.len()
        || replacement_end < replaced.start
        || replacement_end > messages.len()
        || previous.messages[..replaced.start] != messages[..replaced.start]
        || previous.messages[replaced.end..] != messages[replacement_end..]
    {
        return Err(invalid("message page affected range does not match unchanged regions").into());
    }
    // A size cut depends on the following message, so restart before an exact boundary too.
    let prefix = previous
        .pages
        .partition_point(|p| p.start < replaced.start)
        .saturating_sub(1);
    let mut pages = previous.pages[..prefix].to_vec();
    let mut position = previous.pages.get(prefix).map_or(0, |p| p.start);
    let mut objects = Vec::new();
    while position < messages.len() {
        if position >= replacement_end {
            let old_position = replaced.end + (position - replacement_end);
            if let Ok(page) = previous
                .pages
                .binary_search_by_key(&old_position, |p| p.start)
            {
                for boundary in &previous.pages[page..] {
                    let mut boundary = boundary.clone();
                    boundary.start = replacement_end + (boundary.start - replaced.end);
                    pages.push(boundary);
                }
                break;
            }
        }
        let start = position;
        let mut bytes = PAGE_PREFIX.to_vec();
        loop {
            let body = read(position)?;
            if MessageHash::from_bytes(&body) != messages[position] {
                return Err(invalid("message body hash mismatch").into());
            }
            if position > start {
                bytes.push(b',');
            }
            bytes.extend(&body);
            position += 1;
            let count = position - start;
            let cut = count == LOGICAL_MESSAGE_PAGE_SIZE
                || (count >= MIN_PAGE_MESSAGES && messages[position - 1].boundary()?)
                || bytes.len() + PAGE_SUFFIX.len() > MAX_PAGE_BYTES
                || position == messages.len()
                || (bytes.len() as u128
                    + 1
                    + messages[position].byte_length as u128
                    + PAGE_SUFFIX.len() as u128
                    > MAX_PAGE_BYTES as u128);
            if cut {
                break;
            }
        }
        bytes.extend(PAGE_SUFFIX);
        let object = encoded_object(bytes)?;
        pages.push(PageBoundary {
            start,
            page: ManifestPage {
                hash: object.hash.clone(),
                message_count: (position - start) as u32,
                byte_length: DecimalU64(object.size),
            },
        });
        objects.push(object);
    }
    let index = PageIndex { messages, pages };
    index.validate()?;
    Ok(Repaged { index, objects })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logical_records::{decode_message_page, encode_message_page};
    use serde_json::{json, Value};

    #[derive(Debug, Default)]
    struct PageWork {
        messages_read: usize,
        bytes_read: u64,
        pages_written: usize,
        prefix_reused: usize,
        suffix_reused: usize,
    }
    struct Measured {
        index: PageIndex,
        objects: Vec<EncodedLogicalObject>,
        work: PageWork,
    }
    fn repage<E: From<LogicalRecordError>>(
        previous: &PageIndex,
        messages: Vec<MessageHash>,
        replaced: Range<usize>,
        replacement_end: usize,
        mut read: impl FnMut(usize) -> Result<Vec<u8>, E>,
    ) -> Result<Measured, E> {
        let mut work = PageWork::default();
        work.prefix_reused = previous
            .pages
            .partition_point(|p| p.start < replaced.start)
            .saturating_sub(1);
        let result = super::repage::<E>(previous, messages, replaced, replacement_end, |i| {
            let body = read(i)?;
            work.messages_read += 1;
            work.bytes_read += body.len() as u64;
            Ok(body)
        })?;
        work.pages_written = result.objects.len();
        work.suffix_reused = result.index.pages.len() - work.prefix_reused - work.pages_written;
        Ok(Measured {
            index: result.index,
            objects: result.objects,
            work,
        })
    }
    fn bodies(messages: &[Value]) -> Vec<Vec<u8>> {
        messages
            .iter()
            .map(|m| payload_value::encode(m).unwrap())
            .collect()
    }
    fn initial(messages: &[Vec<u8>]) -> Measured {
        repage::<LogicalRecordError>(
            &PageIndex::default(),
            messages
                .iter()
                .map(|m| MessageHash::from_bytes(m))
                .collect(),
            0..0,
            messages.len(),
            |i| Ok(messages[i].clone()),
        )
        .unwrap()
    }
    fn synthetic() -> Vec<Vec<u8>> {
        (0..1024)
            .map(|i| {
                payload_value::encode(
                    &json!({"chatId":format!("id-{i}"),"data":format!("synthetic-{i}")}),
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn page_codec_matches_javascript_golden_literals() {
        let cases = [
            (
                r#"{"z":"😀","a":"한글","10":0.1,"2":-0.0,"":"empty"}"#,
                r#"{"2":0,"10":0.1,"":"empty","a":"한글","z":"😀"}"#,
            ),
            (
                r#"{"fraction":0.30000000000000004,"small":1e-7,"whole":1.0,"large":1e21}"#,
                r#"{"fraction":0.30000000000000004,"large":1e+21,"small":1e-7,"whole":1}"#,
            ),
            (
                r#"{"𐀀":true,"\ue000":false,"nested":{"b":2,"a":1}}"#,
                r#"{"nested":{"a":1,"b":2},"𐀀":true,"":false}"#,
            ),
        ];
        for (input, expected) in cases {
            let value: Value = serde_json::from_str(input).unwrap();
            let page = encode_message_page(&[value]).unwrap();
            let bytes = [PAGE_PREFIX, expected.as_bytes(), PAGE_SUFFIX].concat();
            assert_eq!(page.bytes, bytes);
            assert_eq!(
                encode_message_page(&decode_message_page(&bytes).unwrap())
                    .unwrap()
                    .bytes,
                bytes
            );
        }
        let left: Value = serde_json::from_str(r#"{"b":2,"a":1}"#).unwrap();
        let right: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).unwrap();
        assert_eq!(
            encode_message_page(&[left]).unwrap(),
            encode_message_page(&[right]).unwrap()
        );
        assert!(decode_message_page(
            br#"{"schema":"risunest.logical-message-page/v1","messages":[{"b":2,"a":1}]}"#
        )
        .is_err());
    }

    #[test]
    fn incremental_append_edit_insert_delete_equal_fresh_pages_and_reuse() {
        let original = synthetic();
        let old = initial(&original);
        for (name, range, replacement) in [
            (
                "append",
                1024..1024,
                bodies(&[json!({"chatId":"appended","data":"new"})]),
            ),
            (
                "middle-edit",
                500..501,
                bodies(&[json!({"chatId":"id-500","data":"edited"})]),
            ),
            (
                "insertion",
                500..500,
                bodies(&[json!({"chatId":"inserted","data":"new"})]),
            ),
            ("deletion", 500..501, Vec::new()),
        ] {
            let mut messages = original.clone();
            messages.splice(range.clone(), replacement.clone());
            let updated = repage::<LogicalRecordError>(
                &old.index,
                messages
                    .iter()
                    .map(|m| MessageHash::from_bytes(m))
                    .collect(),
                range.clone(),
                range.start + replacement.len(),
                |i| Ok(messages[i].clone()),
            )
            .unwrap();
            assert_eq!(updated.index, initial(&messages).index, "{name}");
            assert!(updated.work.prefix_reused > 0, "{name}");
            assert!(
                updated.work.messages_read < 256,
                "{name}: {:?}",
                updated.work
            );
            if name != "append" {
                assert!(updated.work.suffix_reused > 0, "{name}");
            }
            println!("{name}: {:?}", updated.work);
        }
    }

    #[test]
    fn byte_caps_include_envelopes_and_oversized_messages_are_indivisible() {
        let overhead = PAGE_PREFIX.len() + PAGE_SUFFIX.len();
        let exact = vec![b' '; MAX_PAGE_BYTES - overhead];
        // A JSON string sized so one complete envelope lands exactly on the cap.
        let mut exact = exact;
        exact[0] = b'"';
        let end = exact.len() - 1;
        exact[end] = b'"';
        let huge =
            payload_value::encode(&json!({"chatId":"huge","data":"x".repeat(MAX_PAGE_BYTES)}))
                .unwrap();
        let list = vec![
            b"0".to_vec(),
            exact.clone(),
            b"1".to_vec(),
            huge.clone(),
            b"2".to_vec(),
        ];
        let result = initial(&list);
        assert_eq!(result.index.pages.len(), 5);
        assert_eq!(
            result.index.pages[1].page.byte_length.0,
            MAX_PAGE_BYTES as u64
        );
        assert!(result.index.pages[3].page.byte_length.0 > MAX_PAGE_BYTES as u64);
        assert_eq!(result.index.pages[3].page.message_count, 1);
        for object in &result.objects {
            assert!(decode_message_page(&object.bytes).is_ok());
        }
        assert!(initial(&[]).index.manifest().pages.is_empty());
        println!("oversized: {:?}", result.work);
    }

    #[test]
    fn sparse_boundaries_have_measured_worst_case_and_count_cap() {
        let body = (0..100)
            .map(|i| payload_value::encode(&json!({"chatId":"preserved","data":i})).unwrap())
            .find(|b| !MessageHash::from_bytes(b).boundary().unwrap())
            .unwrap();
        let original = vec![body.clone(); 1024];
        let old = initial(&original);
        assert_eq!(old.index.pages.len(), 8);
        assert!(old.index.pages.iter().all(|p| p.page.message_count == 128));
        let mut messages = original.clone();
        messages.insert(512, body);
        let result = repage::<LogicalRecordError>(
            &old.index,
            messages
                .iter()
                .map(|m| MessageHash::from_bytes(m))
                .collect(),
            512..512,
            513,
            |i| Ok(messages[i].clone()),
        )
        .unwrap();
        assert_eq!(result.index, initial(&messages).index);
        assert_eq!(result.work.suffix_reused, 0);
        assert_eq!(result.work.messages_read, 641);
        println!("sparse-insertion: {:?}", result.work);
    }

    #[test]
    fn affected_range_and_manifest_integrity_are_checked() {
        let original = synthetic();
        let old = initial(&original);
        let mut bad = old.index.messages.clone();
        bad[900].hash = "00".repeat(32);
        assert!(
            repage::<LogicalRecordError>(&old.index, bad, 500..501, 501, |_| panic!(
                "must validate before body reads"
            ))
            .is_err()
        );
        let mut manifest = old.index.manifest();
        manifest.message_count.0 += 1;
        assert!(manifest.encode().is_err());
        let encoded = old.index.manifest().encode().unwrap();
        assert_eq!(
            MessageManifest::decode(&encoded.bytes).unwrap(),
            old.index.manifest()
        );
        assert!(MessageManifest::decode(br#"{"schema":"risunest.message-manifest/v1","messageCount":"0","pages":[],"unknown":true}"#).is_err());
        for count in ["1", "\"01\"", "\"4294967296\""] {
            let bytes = format!("{{\"schema\":\"risunest.message-manifest/v1\",\"messageCount\":\"1\",\"pages\":[{{\"hash\":\"{}\",\"messageCount\":{count},\"byteLength\":\"100\"}}]}}","00".repeat(32));
            assert!(
                MessageManifest::decode(bytes.as_bytes()).is_err(),
                "{count}"
            );
        }
        assert_eq!(
            serde_json::to_value(ManifestPage {
                hash: "00".repeat(32),
                message_count: 1,
                byte_length: DecimalU64(100)
            })
            .unwrap()["messageCount"],
            "1"
        );
        assert!(repage::<LogicalRecordError>(
            &old.index,
            old.index.messages.clone(),
            500..501,
            501,
            |_| Ok(b"null".to_vec())
        )
        .is_err());
    }
}
