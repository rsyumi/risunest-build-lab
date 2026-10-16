//! PocketRisu v1.10.0 and v1.12.0 export raw `inlay/<id>.<ext>` files before their
//! `inlay_sidecar/<id>` JSON. Pair staged paths, never retain media in memory.
use super::{
    check_cancelled, invalid, open_staged, to_alias_size, AssetAlias,
    CancellationProbe, CancellationReader, HashMap,
    LocalBackupError, Map,
    StagedLocalBackupEntry, Value, MAX_METADATA_BYTES,
};
use std::io::{BufReader, Read};
mod legacy;

pub(super) enum Entry<'a> {
    Payload { id: &'a str, ext: &'a str },
    Legacy { id: &'a str },
    Metadata { id: &'a str },
    Provenance { id: &'a str },
    Cache,
    Skipped,
}

pub(super) fn classify(name: &str) -> Result<Option<Entry<'_>>, LocalBackupError> {
    let Some((namespace, rest)) = name.split_once('/') else {
        return Ok(None);
    };
    if !matches!(
        namespace,
        "inlay" | "inlay_sidecar" | "inlay_info" | "inlay_meta" | "inlay_thumb"
    ) {
        return Ok(None);
    }
    if rest.is_empty() || rest.contains(['/', '\\']) {
        return Ok(Some(Entry::Skipped));
    }
    Ok(Some(match namespace {
        "inlay" => {
            let Some((id, ext)) = rest
                .rsplit_once('.')
                .filter(|(id, _)| !id.is_empty())
            else {
                return Ok(Some(Entry::Legacy { id: rest }));
            };
            if ext.is_empty() { Entry::Legacy { id } } else { Entry::Payload { id, ext } }
        }
        "inlay_sidecar" | "inlay_info" => Entry::Metadata { id: rest },
        "inlay_meta" => Entry::Provenance { id: rest },
        _ => Entry::Cache,
    }))
}

// Store only references to files. Even sidecar JSON is read one item at a time.
#[derive(Default)]
pub(super) struct MetadataIndex<'a> {
    legacy_info: HashMap<&'a str, &'a StagedLocalBackupEntry>,
    sidecars: HashMap<&'a str, &'a StagedLocalBackupEntry>,
    provenance: HashMap<&'a str, &'a StagedLocalBackupEntry>,
}

pub(super) fn index_metadata<'a>(
    entries: &'a [StagedLocalBackupEntry],
    cancellation: &dyn CancellationProbe,
) -> Result<MetadataIndex<'a>, LocalBackupError> {
    let mut index = MetadataIndex::default();
    for entry in entries {
        check_cancelled(cancellation)?;
        let target = match classify(&entry.logical_name)? {
            Some(Entry::Metadata { id }) if entry.logical_name.starts_with("inlay_info/") => Some((&mut index.legacy_info, id)),
            Some(Entry::Metadata { id }) => Some((&mut index.sidecars, id)),
            Some(Entry::Provenance { id }) => Some((&mut index.provenance, id)),
            _ => None,
        };
        if let Some((target, id)) = target {
            if target.insert(id, entry).is_some() {
                return Err(invalid(
                    "PocketRisu backup contains duplicate Inlay metadata",
                ));
            }
        }
    }
    Ok(index)
}

struct Metadata {
    ext: String,
    name: String,
    inlay_type: String,
    width: Option<i64>,
    height: Option<i64>,
}

pub(super) fn prepare(
    entry: &StagedLocalBackupEntry,
    id: &str,
    ext: &str,
    metadata: &MetadataIndex<'_>,
    cancellation: &dyn CancellationProbe,
) -> Result<Option<AssetAlias>, LocalBackupError> {
    check_cancelled(cancellation)?;
    let mut retained_metadata = Map::new();
    if let Some(provenance) = metadata.provenance.get(id) {
        let value: Map<String, Value> = read_metadata(provenance, cancellation)?;
        // RisuNest has no PocketRisu gallery UI, but retain its timestamps and
        // original character/chat ownership instead of discarding user metadata.
        retained_metadata.insert("pocketRisu".to_owned(), Value::Object(value));
    }
    let mut decoded = metadata_for(id, ext, None, metadata, cancellation)?;
    if decoded.inlay_type != "signature" && media_type(&decoded.ext).is_none() {
        let mut prefix = Vec::new();
        CancellationReader::new(open_staged(entry)?, cancellation).take(32).read_to_end(&mut prefix).map_err(|error| super::cancellation_io(error, cancellation))?;
        let Some(ext) = image_extension(&prefix) else { return Ok(None); };
        decoded.ext = ext.into();
        decoded.inlay_type = "image".into();
    }
    finish_alias(id, &entry.sha256, entry.byte_length, decoded, retained_metadata).map(Some)
}

fn metadata_for(id: &str, ext: &str, own: Option<&Map<String, Value>>, index: &MetadataIndex<'_>, cancellation: &dyn CancellationProbe) -> Result<Metadata, LocalBackupError> {
    let external = index.sidecars.get(id).or_else(|| index.legacy_info.get(id))
        .map(|entry| read_metadata::<Map<String, Value>>(entry, cancellation)).transpose()?;
    let object = external.as_ref().or(own);
    let string = |key| object.and_then(|value| value.get(key)).and_then(Value::as_str);
    Ok(Metadata {
        ext: normalize_extension(string("ext").unwrap_or(if object.is_some() { "" } else { ext })),
        name: string("name").unwrap_or(id).to_owned(),
        inlay_type: string("type").unwrap_or("image").to_owned(),
        width: object.and_then(|value| value.get("width")).and_then(Value::as_i64),
        height: object.and_then(|value| value.get("height")).and_then(Value::as_i64),
    })
}

fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some("png") }
    else if bytes.starts_with(&[0xff, 0xd8, 0xff]) { Some("jpg") }
    else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { Some("gif") }
    else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { Some("webp") }
    else if bytes.get(4..8) == Some(b"ftyp") && (matches!(bytes.get(8..12), Some(b"avif" | b"avis"))
        || bytes.get(16..).is_some_and(|brands| brands.chunks_exact(4).any(|brand| matches!(brand, b"avif" | b"avis")))) { Some("avif") }
    else { None }
}

fn finish_alias(id: &str, hash: &str, size: u64, mut metadata: Metadata, retained_metadata: Map<String, Value>) -> Result<AssetAlias, LocalBackupError> {
    metadata.ext = normalize_extension(&metadata.ext);
    if !matches!(
        metadata.inlay_type.as_str(),
        "image" | "audio" | "video" | "signature"
    ) || metadata.width.is_some_and(|n| n < 0)
        || metadata.height.is_some_and(|n| n < 0)
    {
        return Err(invalid("PocketRisu Inlay metadata is invalid"));
    }
    let mime = if metadata.inlay_type == "signature" {
        "application/json"
    } else {
        media_type(&metadata.ext)
            .map(|(_, mime)| mime)
            .unwrap_or("application/octet-stream")
    };
    Ok(AssetAlias {
        key: id.to_owned(),
        object_hash: Some(hash.to_owned()),
        kind: "inlay".to_owned(),
        size: to_alias_size(size)?,
        mime: mime.to_owned(),
        name: metadata.name,
        ext: metadata.ext,
        inlay_type: Some(metadata.inlay_type),
        width: metadata.width,
        height: metadata.height,
        metadata: Value::Object(retained_metadata),
    })
}

pub(super) fn prepare_legacy(
    entry: &StagedLocalBackupEntry, id: &str, metadata: &MetadataIndex<'_>,
    cas: &super::PayloadCas, durable: &mut super::DurableCasJob,
    cancellation: &dyn CancellationProbe,
) -> Result<Option<AssetAlias>, LocalBackupError> {
    legacy::prepare(entry, id, metadata, cas, durable, cancellation)
}

fn read_metadata<T: serde::de::DeserializeOwned>(
    entry: &StagedLocalBackupEntry,
    cancellation: &dyn CancellationProbe,
) -> Result<T, LocalBackupError> {
    if entry.byte_length > u64::from(MAX_METADATA_BYTES) {
        return Err(LocalBackupError::new(super::LocalBackupErrorCode::RecordTooLarge, "PocketRisu Inlay metadata is too large"));
    }
    let reader = CancellationReader::new(open_staged(entry)?, cancellation)
        .take(u64::from(MAX_METADATA_BYTES) + 1);
    let value: Value = serde_json::from_reader(BufReader::new(reader)).map_err(|error| {
        if let Err(cancelled) = check_cancelled(cancellation) { return cancelled; }
        match error.io_error_kind() {
            Some(kind) => LocalBackupError::io(std::io::Error::new(kind, error)),
            None => invalid("PocketRisu Inlay metadata is unreadable"),
        }
    })?;
    serde_json::from_value(value).map_err(|_| invalid("PocketRisu Inlay metadata is invalid"))
}

fn normalize_extension(ext: &str) -> String {
    let normalized: String = ext.trim().to_lowercase().trim_start_matches('.').chars()
        .filter(|value| !matches!(value, '/' | '\\' | '\0')).collect();
    if normalized.is_empty() { "bin".into() } else { normalized }
}

fn media_type(ext: &str) -> Option<(&'static str, &'static str)> {
    Some(match ext {
        "avif" => ("image", "image/avif"),
        "gif" => ("image", "image/gif"),
        "jpeg" | "jpg" => ("image", "image/jpeg"),
        "png" => ("image", "image/png"),
        "webp" => ("image", "image/webp"),
        "flac" => ("audio", "audio/flac"),
        "mp3" => ("audio", "audio/mpeg"),
        "ogg" => ("audio", "audio/ogg"),
        "wav" => ("audio", "audio/wav"),
        "mkv" => ("video", "video/x-matroska"),
        "mp4" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        _ => return None,
    })
}
