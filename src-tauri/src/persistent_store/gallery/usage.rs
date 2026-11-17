use super::*;
use crate::asset_repository::PayloadCas;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) struct PreparedScan {
    reader: snapshot::RevisionReadLease,
    _inventory: snapshot::DeferredAssetInventory,
    root: std::path::PathBuf,
    session: Arc<Mutex<ScanSession>>,
    cancel: Arc<AtomicBool>,
    status: ScanStatus,
    /// Device-local plugin values as asset cleanup reads them, or `None` when they could not be read.
    device: Option<(super::super::device_store::plugin_gc::PluginGcFence, crate::asset_repository::migration_gc::AssetRootSet)>,
}

impl PersistentStore {
    pub(crate) fn prepare_gallery_scan(&mut self, expected_revision: i64) -> StoreResult<(ScanStatus, PreparedScan)> {
        let inventory = self.active_readers.defer_asset_inventory();
        let (_, reader) = snapshot::acquire_revision(&self.database_path, expected_revision, Arc::clone(&self.active_readers))?;
        self.gallery_generation(&reader.target.generation);
        if self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner()).status.as_ref().is_some_and(|status| status.state == "running") { return Err(invalid("attachment usage scan is already running")); }
        let device = snapshot::collect_device_plugin_asset_roots(self).ok();
        let mut session = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
        if session.status.as_ref().is_some_and(|status| status.state == "running") { return Err(invalid("attachment usage scan is already running")); }
        let total: i64 = reader.connection.query_row("SELECT COALESCE(SUM(CASE WHEN archived_object IS NULL THEN conversation_count ELSE json_extract(archived_object,'$.conversationCount') END),0) FROM characters WHERE generation=?1", [&reader.target.generation], |row| row.get(0))?;
        let archives: i64 = reader.connection.query_row("SELECT COUNT(*) FROM characters WHERE generation=?1 AND archived_object IS NOT NULL", [&reader.target.generation], |row| row.get(0))?;
        let total = u64::try_from(total).map_err(|_| invalid("invalid conversation count"))?;
        let archives = u64::try_from(archives).map_err(|_| invalid("invalid archive count"))?;
        let status = ScanStatus { id: uuid::Uuid::new_v4().to_string(), revision: expected_revision, state: "running".into(), completed_conversations: 0, total_conversations: total, completed_items: 0, total_items: total + archives, phase: "scanning".into(), unchecked_archives: 0, unchecked_records: 0, counts: UsageCounts::default(), characters: Vec::new(), error: None };
        let cancel = Arc::new(AtomicBool::new(false));
        session.cancel = Arc::clone(&cancel);
        session.status = Some(status.clone());
        Ok((status.clone(), PreparedScan { reader, _inventory: inventory, root: self.repository_root.clone(), session: Arc::clone(&self.gallery.scan), cancel, status, device }))
    }
}

impl Drop for PreparedScan {
    fn drop(&mut self) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(status) = session.status.as_mut().filter(|status| status.id == self.status.id && status.state == "running") {
            status.state = "cancelled".into();
        }
    }
}

impl PreparedScan {
    pub(crate) fn cancellation(&self) -> Arc<AtomicBool> { Arc::clone(&self.cancel) }
    fn check(&self) -> StoreResult<()> { if self.cancel.load(Ordering::Acquire) { Err(invalid("attachment usage scan cancelled")) } else { Ok(()) } }
    fn progress(&self, completed: u64, fetched: u64, unchecked: usize) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(status) = &mut session.status {
            status.completed_conversations = completed;
            status.completed_items = completed + fetched;
            status.phase = "scanning".into();
            status.unchecked_archives = unchecked;
        }
    }
    pub(crate) fn run(mut self) {
        let result = self.scan();
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let Some(status) = session.status.as_mut() else { return; };
        if self.cancel.load(Ordering::Acquire) {
            status.state = "cancelled".into();
        } else {
            match result {
                Ok(mut result) => {
                    self.status.state = "complete".into();
                    result.status = self.status.clone();
                    *status = self.status.clone();
                    session.result = Some(result);
                }
                Err(_) => { status.state = "failed".into(); status.error = Some("attachment usage scan failed".into()); }
            }
        }
    }
    fn scan(&mut self) -> StoreResult<UsageResult> {
        self.check()?;
        let db = &self.reader.connection;
        let generation = &self.reader.target.generation;
        let mut result = UsageResult { id: self.status.id.clone(), revision: self.reader.target.revision, status: self.status.clone(), keys: HashSet::new(), locations: HashMap::new(), unchecked_archives: 0, unchecked_records: 0, pending_fence: pending_fence(db)?, device_fence: None };
        let mut aliases_by_hash: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut statement = db.prepare("SELECT logical_key,object_hash FROM asset_aliases WHERE generation=?1 AND kind='inlay' AND inlay_type IN ('image','video','audio')")?;
            let mut rows = statement.query([generation])?;
            while let Some(row) = rows.next()? {
                self.check()?;
                let key: String = row.get(0)?;
                if let Some(hash) = row.get::<_, Option<String>>(1)? { aliases_by_hash.entry(hash).or_default().push(key.clone()); }
                result.keys.insert(key);
            }
        }
        for (table, kind) in [("root", "settings"), ("bot_presets", "settings"), ("plugin_storage", "plugin")] {
            let mut statement = db.prepare(&format!("SELECT value FROM {table} WHERE generation=?1"))?;
            let mut rows = statement.query([generation])?;
            let location = GalleryLocation { kind: kind.into(), ..Default::default() };
            while let Some(row) = rows.next()? { self.check()?; observe_column(row, 0, &location, &mut result, &self.cancel)?; }
        }
        observe_pending(db, &mut result, &self.cancel)?;
        let plugin = GalleryLocation { kind: "plugin".into(), ..Default::default() };
        match self.device.take() {
            Some((fence, roots)) => {
                for key in &roots.inlay_ids { self.check()?; add_location(&mut result, key, &plugin); }
                if roots.retain_all_objects || !roots.cold_keys.is_empty() { result.unchecked_records += 1; }
                result.device_fence = Some(fence);
            }
            None => result.unchecked_records += 1,
        }
        let mut completed = 0;
        let mut fetched_archives = 0;
        let mut names = HashMap::new();
        let mut characters = db.prepare("SELECT character_id,name,trashed,detail,archived_object FROM characters WHERE generation=?1 ORDER BY configured_index,character_id")?;
        let mut rows = characters.query([generation])?;
        while let Some(row) = rows.next()? {
            self.check()?;
            let id: String = row.get(0)?;
            let name: String = row.get(1)?;
            names.insert(id.clone(), name.clone());
            let mut location = GalleryLocation { kind: "character".into(), character_id: Some(id.clone()), character_name: Some(name), trashed: row.get::<_, i64>(2)? != 0, ..Default::default() };
            observe_column(row, 3, &location, &mut result, &self.cancel)?;
            if let Some(raw) = row.get::<_, Option<String>>(4)? {
                let archived: super::super::archive::ArchivedObject = serde_json::from_str(&raw)?;
                location.archived = true;
                location.kind = "archive".into();
                let before = completed;
                if let Some(status) = self.session.lock().unwrap_or_else(|e| e.into_inner()).status.as_mut() { status.phase = "fetchingArchive".into(); }
                let fetched = super::super::archive::fetch_operation_body(db, &self.root, &id, true, &|| self.cancel.load(Ordering::Acquire), Some(Arc::clone(&self.cancel)));
                self.check()?;
                fetched_archives += 1;
                self.progress(completed, fetched_archives, result.unchecked_archives);
                let outcome = fetched.and_then(|()| {
                    let cas = PayloadCas::new(&self.root)?;
                    let unchecked = result.unchecked_archives;
                    super::super::archive::visit_gallery_payload(&cas, &archived, &id, &|| self.cancel.load(Ordering::Acquire), &mut |chat, name, message, index, value| {
                        let item = GalleryLocation { conversation_id: chat.map(str::to_owned), conversation_name: name.map(str::to_owned), message_id: message.map(str::to_owned), message_index: index, ..location.clone() };
                        observe_value(value, &item, &mut result, &self.cancel)
                    }, &mut || { completed += 1; self.progress(completed, fetched_archives, unchecked); self.check() })
                });
                self.check()?;
                if outcome.is_err() {
                    result.unchecked_archives += 1;
                    for hash in &archived.asset_hashes {
                        if let Some(keys) = aliases_by_hash.get(hash) {
                            for key in keys { add_location(&mut result, key, &location); }
                        }
                    }
                }
                completed = before + archived.conversation_count.max(0) as u64;
                self.progress(completed, fetched_archives, result.unchecked_archives);
                continue;
            }
            let mut conversations = db.prepare("SELECT conversation_id,name,detail FROM conversations WHERE generation=?1 AND character_id=?2 ORDER BY configured_index,conversation_id")?;
            let mut chats = conversations.query(params![generation, id])?;
            while let Some(chat) = chats.next()? {
                self.check()?;
                let chat_id: String = chat.get(0)?;
                let chat_location = GalleryLocation { kind: "conversation".into(), conversation_id: Some(chat_id.clone()), conversation_name: Some(chat.get(1)?), ..location.clone() };
                observe_column(chat, 2, &chat_location, &mut result, &self.cancel)?;
                let mut messages = db.prepare("SELECT message_index,message_id,value FROM messages WHERE generation=?1 AND character_id=?2 AND conversation_id=?3 ORDER BY message_index")?;
                let mut values = messages.query(params![generation, id, chat_id])?;
                while let Some(message) = values.next()? {
                    self.check()?;
                    let item = GalleryLocation { kind: "message".into(), message_index: Some(message.get(0)?), message_id: message.get(1)?, ..chat_location.clone() };
                    observe_column(message, 2, &item, &mut result, &self.cancel)?;
                }
                completed += 1;
                self.progress(completed, fetched_archives, result.unchecked_archives);
            }
        }
        let mut character_keys: HashMap<String, HashSet<&str>> = HashMap::new();
        for (key, locations) in &result.locations {
            for location in locations {
                if let Some(id) = &location.character_id { character_keys.entry(id.clone()).or_default().insert(key); }
            }
        }
        self.status.completed_conversations = completed;
        self.status.completed_items = completed + fetched_archives;
        self.status.unchecked_archives = result.unchecked_archives;
        self.status.unchecked_records = result.unchecked_records;
        self.status.counts.used = result.locations.len();
        if result.unchecked_archives == 0 && result.unchecked_records == 0 { self.status.counts.unused = result.keys.len() - result.locations.len(); }
        else { self.status.counts.unknown = result.keys.len() - result.locations.len(); }
        self.status.characters = character_keys.into_iter().map(|(id, keys)| CharacterCount { name: names.remove(&id).unwrap_or_default(), character_id: id, count: keys.len() }).collect();
        self.status.characters.sort_by(|a,b| a.name.cmp(&b.name).then_with(|| a.character_id.cmp(&b.character_id)));
        Ok(result)
    }
}
fn add_location(result: &mut UsageResult, key: &str, location: &GalleryLocation) {
    if !result.keys.contains(key) { return; }
    let locations = result.locations.entry(key.into()).or_default();
    if locations.last() != Some(location) { locations.push(location.clone()); }
}
/// A record that cannot be decoded may reference any attachment, so it keeps
/// unlocated attachments out of `unused`, as asset cleanup retains them.
fn observe_column(row: &rusqlite::Row<'_>, index: usize, location: &GalleryLocation, result: &mut UsageResult, cancel: &AtomicBool) -> StoreResult<()> {
    let bytes = match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => return Ok(()),
        rusqlite::types::ValueRef::Text(bytes) => bytes,
        _ => { result.unchecked_records += 1; return Ok(()); }
    };
    let Ok(raw) = std::str::from_utf8(bytes) else {
        result.unchecked_records += 1;
        observe_text(&String::from_utf8_lossy(bytes), location, result);
        return Ok(());
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => observe_value(&value, location, result, cancel),
        Err(_) => { result.unchecked_records += 1; observe_text(raw, location, result); Ok(()) }
    }
}
fn observe_text(text: &str, location: &GalleryLocation, result: &mut UsageResult) {
    if text.strip_prefix("\u{ef01}COLDSTORAGE\u{ef01}").is_some_and(|key| !key.is_empty()) { result.unchecked_records += 1; }
    for prefix in ["{{inlay::", "{{inlayed::", "{{inlayeddata::"] {
        let mut rest = text;
        while let Some(start) = rest.find(prefix) {
            rest = &rest[start + prefix.len()..];
            let Some(end) = rest.find("}}") else { break; };
            add_location(result, &rest[..end], location);
            rest = &rest[end + 2..];
        }
    }
}
fn observe_value(value: &Value, location: &GalleryLocation, result: &mut UsageResult, cancel: &AtomicBool) -> StoreResult<()> {
    if cancel.load(Ordering::Acquire) { return Err(invalid("attachment usage scan cancelled")); }
    match value {
        Value::String(text) => observe_text(text, location, result),
        Value::Array(values) => for value in values { observe_value(value, location, result, cancel)?; },
        Value::Object(values) => for (key, value) in values {
            if (key == "coldstorage" && value.as_str().is_some_and(|key| !key.is_empty()))
                || (key == "coldStoragedChats" && value.as_array().is_some_and(|keys| !keys.is_empty())) { result.unchecked_records += 1; }
            observe_value(value, location, result, cancel)?;
        },
        _ => {}
    }
    Ok(())
}

// Receiving rows can change without changing the projected library revision.
pub(in crate::persistent_store) fn pending_fence(db: &Connection) -> StoreResult<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut statement = db.prepare("SELECT request_id,key,stamp,value,status FROM lww_receive_rows WHERE status IN ('staged','held','deferred') ORDER BY request_id,key")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        for index in 0..5 {
            let value: String = row.get(index)?;
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    Ok(hash.finalize().into())
}

fn observe_pending(db: &Connection, result: &mut UsageResult, cancel: &AtomicBool) -> StoreResult<()> {
    use risunest_sync_wire::unit::{UnitKey, UnitValue};
    let location = GalleryLocation { kind: "pending".into(), ..Default::default() };
    let mut statement = db.prepare("SELECT key,value FROM lww_receive_rows WHERE status IN ('staged','held','deferred') ORDER BY request_id,key")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if cancel.load(Ordering::Acquire) { return Err(invalid("attachment usage scan cancelled")); }
        let outcome = (|| -> StoreResult<()> {
            let key: UnitKey = row.get::<_, String>(0)?.try_into().map_err(|_| invalid("invalid pending unit key"))?;
            let value: UnitValue = serde_json::from_str(&row.get::<_, String>(1)?)?;
            if matches!(value, UnitValue::Deleted) { return Ok(()); }
            if key.components().first().is_some_and(|kind| kind == "messages") {
                super::super::message_pages::validate_manifest(db, &value)?;
                let UnitValue::Object { descriptor, .. } = value else { return Err(invalid("invalid pending messages")); };
                let body = super::super::message_pages::object_body(db, &descriptor.object_hash)?.ok_or_else(|| invalid("pending messages unavailable"))?;
                let manifest = risunest_external_storage_format::message_pages::MessageManifest::decode(&body).map_err(|_| invalid("invalid pending manifest"))?;
                for page in manifest.pages {
                    let body = super::super::message_pages::object_body(db, &page.hash)?.ok_or_else(|| invalid("pending page unavailable"))?;
                    let messages = risunest_external_storage_format::logical_records::decode_message_page(&body).map_err(|_| invalid("invalid pending page"))?;
                    for message in messages { observe_value(&message, &location, result, cancel)?; }
                }
            } else if matches!(value, UnitValue::Inline { .. }) || super::super::external_capture::is_large_unit(&key) {
                if let Some(value) = super::super::lww::json_value_resolved(db, &value)? { observe_value(&value, &location, result, cancel)?; }
            } else {
                // Opaque or archived pending objects have not been projected into scannable records.
                result.unchecked_records += 1;
            }
            Ok(())
        })();
        if outcome.is_err() { result.unchecked_records += 1; }
    }
    Ok(())
}
