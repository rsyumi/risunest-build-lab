//! Upstream cold plugin storage. `pluginCustomStorage._coldpluginIndex` names an
//! index cold item that holds small values as JSON text and points large ones at
//! their own items; the older `_coldplugin` map points each key at its own item.
use super::{cancelled, job_error, ColdPayloadUnavailable, ReplacementSink, RestoreControl, RestoreLimits};
use super::super::NativeJobError;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read};

pub(super) const INDEX_KEY: &str = "_coldpluginIndex";
pub(super) const LEGACY_MAP_KEY: &str = "_coldplugin";
const MARKERS: [&str; 2] = [LEGACY_MAP_KEY, INDEX_KEY];
const INDEX_TYPE: &str = "risu-plugin-storage-index";
const INDEX_VERSION: u64 = 1;

/// Whether the markers name any cold item to read.
pub(super) fn references(pointer: Option<&Value>, legacy: Option<&Value>) -> bool {
    pointer.is_some_and(Value::is_string)
        || legacy.and_then(Value::as_object).is_some_and(|map| map.values().any(Value::is_string))
}

pub(super) fn storage_references(storage: &Map<String, Value>) -> bool {
    references(storage.get(INDEX_KEY), storage.get(LEGACY_MAP_KEY))
}

/// The plugin values staged so far, which inline values occupy before the import.
pub(super) trait Target {
    fn contains(&self, key: &str) -> Result<bool, NativeJobError>;
    fn insert(&mut self, key: &str, value: Value) -> Result<(), NativeJobError>;
}

impl Target for Map<String, Value> {
    fn contains(&self, key: &str) -> Result<bool, NativeJobError> {
        Ok(self.contains_key(key))
    }

    fn insert(&mut self, key: &str, value: Value) -> Result<(), NativeJobError> {
        Map::insert(self, key.to_owned(), value);
        Ok(())
    }
}

enum Item {
    Present(Value),
    Missing,
    Unavailable(ColdPayloadUnavailable),
}

enum Entry {
    Small(String),
    Large(String),
}

#[derive(Default)]
struct Omissions {
    missing: u64,
    damaged: u64,
    too_large: u64,
    unsupported: u64,
}

impl Omissions {
    fn unreadable(&mut self, item: Item) {
        match item {
            Item::Present(_) => {}
            Item::Missing => self.missing += 1,
            Item::Unavailable(ColdPayloadUnavailable::Damaged) => self.damaged += 1,
            Item::Unavailable(ColdPayloadUnavailable::TooLarge) => self.too_large += 1,
        }
    }

    fn record(&self, job: &dyn RestoreControl) -> Result<(), NativeJobError> {
        for (kind, count) in [
            ("missing", self.missing),
            ("damagedPluginStorage", self.damaged),
            ("oversizedPluginStorage", self.too_large),
            ("pluginStorage", self.unsupported),
        ] {
            if count != 0 {
                job.record_restore_omission(kind, count).map_err(|error| job_error(job, error))?;
            }
        }
        Ok(())
    }
}

/// Stages the values the markers reference. An inline value wins over an index
/// entry and an index entry over the legacy map, as upstream reads them; a value
/// that cannot be read is reported and never stored.
pub(super) fn import(
    target: &mut dyn Target,
    pointer: Option<Value>,
    legacy: Option<Value>,
    sink: &dyn ReplacementSink,
    job: &dyn RestoreControl,
    limits: RestoreLimits,
) -> Result<(), NativeJobError> {
    let mut omissions = Omissions::default();
    let mut indexed = HashSet::new();
    if let Some(Value::String(id)) = pointer {
        match read(sink, &id, limits, job)? {
            Item::Present(index) => match index_entries(index) {
                Ok(entries) => {
                    for (key, entry) in entries {
                        indexed.insert(key.clone());
                        if MARKERS.contains(&key.as_str()) {
                            omissions.unsupported += 1;
                            continue;
                        }
                        if target.contains(&key)? {
                            continue;
                        }
                        let value = match entry {
                            Entry::Small(json) => match serde_json::from_str(&json) {
                                Ok(value) => value,
                                Err(_) => {
                                    omissions.damaged += 1;
                                    continue;
                                }
                            },
                            Entry::Large(id) => match read(sink, &id, limits, job)? {
                                Item::Present(value) => value,
                                item => {
                                    omissions.unreadable(item);
                                    continue;
                                }
                            },
                        };
                        target.insert(&key, value)?;
                    }
                }
                Err(IndexError::Unsupported) => omissions.unsupported += 1,
                Err(IndexError::Corrupt) => omissions.damaged += 1,
            },
            item => omissions.unreadable(item),
        }
    }
    if let Some(Value::Object(map)) = legacy {
        for (key, id) in map {
            let Value::String(id) = id else { continue };
            if indexed.contains(&key) {
                continue;
            }
            if MARKERS.contains(&key.as_str()) {
                omissions.unsupported += 1;
                continue;
            }
            if target.contains(&key)? {
                continue;
            }
            match read(sink, &id, limits, job)? {
                Item::Present(value) => target.insert(&key, value)?,
                item => omissions.unreadable(item),
            }
        }
    }
    omissions.record(job)
}

enum IndexError {
    Corrupt,
    Unsupported,
}

/// Index entries in upstream's order. A later entry for a key replaces the earlier
/// one, and an entry that names neither JSON text nor an item is not a value.
fn index_entries(index: Value) -> Result<Vec<(String, Entry)>, IndexError> {
    let Value::Object(mut index) = index else { return Err(IndexError::Corrupt) };
    if index.get("type").and_then(Value::as_str) != Some(INDEX_TYPE)
        || !index.get("entries").is_some_and(Value::is_array)
        || !index.get("trash").is_some_and(Value::is_array)
    {
        return Err(IndexError::Corrupt);
    }
    if index.get("version").and_then(Value::as_u64) != Some(INDEX_VERSION) {
        return Err(IndexError::Unsupported);
    }
    let Some(Value::Array(entries)) = index.remove("entries") else { return Err(IndexError::Corrupt) };
    let mut ordered: Vec<(String, Entry)> = Vec::new();
    let mut positions: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        let Value::Object(mut entry) = entry else { continue };
        let Some(Value::String(key)) = entry.remove("k") else { continue };
        let value = match (entry.remove("l"), entry.remove("s")) {
            (Some(Value::String(id)), _) => Entry::Large(id),
            (_, Some(Value::String(json))) => Entry::Small(json),
            _ => continue,
        };
        match positions.get(&key) {
            Some(&position) => ordered[position].1 = value,
            None => {
                positions.insert(key.clone(), ordered.len());
                ordered.push((key, value));
            }
        }
    }
    Ok(ordered)
}

fn read(sink: &dyn ReplacementSink, key: &str, limits: RestoreLimits, job: &dyn RestoreControl) -> Result<Item, NativeJobError> {
    if job.is_cancel_requested() {
        return Err(cancelled("restore cancelled while staging plugin storage"));
    }
    let Some(path) = sink.cold_payload_path(key) else {
        return Ok(sink.unavailable_cold_payload_reason(key).map_or(Item::Missing, Item::Unavailable));
    };
    let read_error = |error: std::io::Error| NativeJobError::new("store-error", format!("staged cold payload cannot be read: {error}"));
    let file = File::open(path).map_err(read_error)?;
    if file.metadata().map_err(read_error)?.len() > limits.max_decoded_block_bytes {
        return Ok(Item::Unavailable(ColdPayloadUnavailable::TooLarge));
    }
    let reader = BufReader::with_capacity(64 * 1024, file.take(limits.max_decoded_block_bytes.saturating_add(1)));
    match serde_json::from_reader::<_, Value>(reader) {
        Ok(value) => Ok(Item::Present(value)),
        Err(error) if error.is_io() => Err(read_error(error.into())),
        Err(_) => Ok(Item::Unavailable(ColdPayloadUnavailable::Damaged)),
    }
}
