use super::{snapshot, AssetAlias, PersistentStore, ReadTarget, RevisionResult, StoreError, StoreResult, WorkingSetCommit};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

mod usage;
pub(super) use usage::pending_fence;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GalleryQuery {
    #[serde(default = "all")]
    pub(crate) r#type: String,
    #[serde(default = "newest")]
    pub(crate) sort: String,
    #[serde(default)]
    pub(crate) offset: usize,
    #[serde(default = "page_limit")]
    pub(crate) limit: usize,
    #[serde(default = "all")]
    pub(crate) usage: String,
    pub(crate) character_id: Option<String>,
}
fn all() -> String { "all".into() }
fn newest() -> String { "newest".into() }
fn page_limit() -> usize { 128 }
impl GalleryQuery {
    fn validate(&self) -> StoreResult<()> {
        if !matches!(self.r#type.as_str(), "all" | "image" | "video" | "audio")
            || !matches!(self.sort.as_str(), "newest" | "oldest" | "largest")
            || !matches!(self.usage.as_str(), "all" | "unused" | "unknown") {
            return Err(invalid("invalid attachment gallery filter"));
        }
        Ok(())
    }
    fn order_key(&self) -> Self { Self { offset: 0, limit: 0, ..self.clone() } }
}

#[derive(Clone, Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TypeCounts { pub(crate) all: usize, pub(crate) image: usize, pub(crate) video: usize, pub(crate) audio: usize }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GalleryItem { #[serde(flatten)] pub(crate) alias: AssetAlias, pub(crate) sort_time: f64 }
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GalleryPage {
    pub(crate) scan_id: Option<String>, pub(crate) revision: i64, pub(crate) total: usize, pub(crate) total_bytes: i64,
    pub(crate) counts: TypeCounts, pub(crate) items: Vec<GalleryItem>,
}
#[derive(Serialize)]
pub(crate) struct GalleryKeys { pub(crate) revision: i64, pub(crate) keys: Vec<String> }
#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct UsageCounts { pub(crate) used: usize, pub(crate) unused: usize, pub(crate) unknown: usize }
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CharacterCount { pub(crate) character_id: String, pub(crate) name: String, pub(crate) count: usize }
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanStatus {
    pub(crate) id: String, pub(crate) revision: i64, pub(crate) state: String,
    pub(crate) completed_conversations: u64, pub(crate) total_conversations: u64,
    pub(crate) completed_items: u64, pub(crate) total_items: u64, pub(crate) phase: String,
    pub(crate) unchecked_archives: usize, pub(crate) unchecked_records: usize, pub(crate) counts: UsageCounts,
    pub(crate) characters: Vec<CharacterCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GalleryLocation {
    pub(crate) kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) character_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) character_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) conversation_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message_index: Option<i64>,
    pub(crate) archived: bool, pub(crate) trashed: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GalleryLocations {
    pub(crate) revision: Option<i64>, pub(crate) state: Option<String>,
    pub(crate) unchecked_archives: usize, pub(crate) unchecked_records: usize, pub(crate) locations: Vec<GalleryLocation>,
}
#[derive(Default)]
pub(crate) struct GallerySession {
    generation: Option<String>, orders: VecDeque<Order>,
    scan: Arc<Mutex<ScanSession>>,
}
#[derive(Default)]
struct ScanSession { status: Option<ScanStatus>, result: Option<UsageResult>, cancel: Arc<AtomicBool> }
struct UsageResult {
    id: String, revision: i64, status: ScanStatus, keys: HashSet<String>, locations: HashMap<String, Vec<GalleryLocation>>,
    unchecked_archives: usize, unchecked_records: usize,
    pending_fence: [u8; 32],
    device_fence: Option<super::device_store::plugin_gc::PluginGcFence>,
}
impl UsageResult {
    fn state(&self, key: &str) -> Option<&'static str> {
        if !self.keys.contains(key) { None }
        else if self.locations.contains_key(key) { Some("used") }
        else if self.unchecked_archives > 0 || self.unchecked_records > 0 { Some("unknown") } else { Some("unused") }
    }
    fn matches(&self, key: &str, query: &GalleryQuery) -> bool {
        (query.usage == "all" || self.state(key) == Some(query.usage.as_str()))
            && query.character_id.as_ref().is_none_or(|id| self.locations.get(key).is_some_and(|locations| locations.iter().any(|location| location.character_id.as_ref() == Some(id))))
    }
}
struct Order { revision: i64, scan_id: Option<String>, query: GalleryQuery, keys: Vec<String>, counts: TypeCounts, total_bytes: i64 }
fn invalid(message: &str) -> StoreError { StoreError::Validation { message: message.into() } }

// JSON numeric checks also exclude strings and non-positive imported dates.
const SORT_TIME: &str = "COALESCE(CASE WHEN json_type(a.metadata,'$.createdAt') IN ('integer','real') AND json_extract(a.metadata,'$.createdAt')>0 THEN json_extract(a.metadata,'$.createdAt') END, CASE WHEN json_type(a.metadata,'$.pocketRisu.createdAt') IN ('integer','real') AND json_extract(a.metadata,'$.pocketRisu.createdAt')>0 THEN json_extract(a.metadata,'$.pocketRisu.createdAt') END, o.created_at_ms, 0)";

impl PersistentStore {
    fn gallery_generation(&mut self, generation: &str) {
        if self.gallery.generation.as_deref() != Some(generation) {
            self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner()).cancel.store(true, Ordering::Release);
            self.gallery = GallerySession { generation: Some(generation.into()), ..Default::default() };
        }
    }
    fn gallery_reader(&mut self) -> StoreResult<snapshot::RevisionReadLease> {
        let (_, reader) = snapshot::acquire_revision(&self.database_path, self.revision()?, Arc::clone(&self.active_readers))?;
        self.gallery_generation(&reader.target.generation);
        Ok(reader)
    }
    fn gallery_order(&mut self, connection: &Connection, target: &ReadTarget, query: &GalleryQuery) -> StoreResult<usize> {
        query.validate()?;
        let scan = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
        let scan_id = scan.result.as_ref().map(|r| r.id.clone());
        let key = query.order_key();
        if let Some(index) = self.gallery.orders.iter().position(|order| order.revision == target.revision && order.scan_id == scan_id && order.query == key) { return Ok(index); }
        self.gallery.orders.retain(|order| order.revision == target.revision && order.scan_id == scan_id);
        let sql = format!("SELECT a.logical_key,a.inlay_type,a.size,{SORT_TIME} AS sort_time FROM asset_aliases a LEFT JOIN asset_objects o ON o.object_hash=a.object_hash WHERE a.generation=?1 AND a.kind='inlay' AND a.inlay_type IN ('image','video','audio')");
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query([&target.generation])?;
        let mut order = Order { revision: target.revision, scan_id, query: key, keys: Vec::new(), counts: TypeCounts::default(), total_bytes: 0 };
        let mut matching = Vec::new();
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            if let Some(result) = &scan.result {
                if !result.matches(&key, query) { continue; }
            } else if query.usage != "all" || query.character_id.is_some() { continue; }
            let kind: String = row.get(1)?;
            order.counts.all += 1;
            match kind.as_str() { "image" => order.counts.image += 1, "video" => order.counts.video += 1, _ => order.counts.audio += 1 }
            if query.r#type == "all" || query.r#type == kind {
                let size: i64 = row.get(2)?;
                order.total_bytes = order.total_bytes.saturating_add(size);
                matching.push((key, size, row.get::<_, f64>(3)?));
            }
        }
        matching.sort_unstable_by(|a, b| {
            let order = match query.sort.as_str() {
                "oldest" => a.2.total_cmp(&b.2),
                "largest" => b.1.cmp(&a.1),
                _ => b.2.total_cmp(&a.2),
            };
            order.then_with(|| a.0.cmp(&b.0))
        });
        order.keys = matching.into_iter().map(|item| item.0).collect();
        if self.gallery.orders.len() == 8 { self.gallery.orders.pop_front(); }
        self.gallery.orders.push_back(order);
        Ok(self.gallery.orders.len() - 1)
    }
    pub(crate) fn gallery_page(&mut self, query: &GalleryQuery) -> StoreResult<GalleryPage> {
        let reader = self.gallery_reader()?;
        let index = self.gallery_order(&reader.connection, &reader.target, query)?;
        let scan = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
        let order = &self.gallery.orders[index];
        if order.scan_id != scan.result.as_ref().map(|result| result.id.clone()) {
            drop(scan);
            return self.gallery_page(query);
        }
        let mut items = Vec::new();
        let sql = format!("SELECT {SORT_TIME} FROM asset_aliases a LEFT JOIN asset_objects o ON o.object_hash=a.object_hash WHERE a.generation=?1 AND a.kind='inlay' AND a.logical_key=?2");
        let mut times = reader.connection.prepare(&sql)?;
        for key in order.keys.iter().skip(query.offset).take(query.limit.min(512)) {
            if let Some(alias) = super::query::read_asset_alias(&reader.connection, "inlay", key, &reader.target)? {
                items.push(GalleryItem { alias: alias.value, sort_time: times.query_row(params![reader.target.generation, key], |row| row.get(0))? });
            }
        }
        Ok(GalleryPage { scan_id: order.scan_id.clone(), revision: reader.target.revision, total: order.keys.len(), total_bytes: order.total_bytes, counts: order.counts.clone(), items })
    }
    pub(crate) fn gallery_keys(&mut self, query: &GalleryQuery) -> StoreResult<GalleryKeys> {
        let reader = self.gallery_reader()?;
        let index = self.gallery_order(&reader.connection, &reader.target, query)?;
        Ok(GalleryKeys { revision: reader.target.revision, keys: self.gallery.orders[index].keys.clone() })
    }
    /// With `scan_id`, deletes only while that scan still describes the library,
    /// including device-local plugin values, which do not move the revision.
    pub(crate) fn gallery_delete(&mut self, keys: &[String], expected_revision: i64, scan_id: Option<&str>) -> StoreResult<RevisionResult> {
        let mut gallery_pending_fence = None;
        if let Some(scan_id) = scan_id {
            let current = {
                let scan = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
                match &scan.result {
                    Some(result) if result.id == scan_id && result.revision == expected_revision => {
                        gallery_pending_fence = Some(result.pending_fence);
                        match &result.device_fence {
                            Some(fence) => self.device_store()?.plugin_gc_fence_is_current(fence)?,
                            None => true,
                        }
                    },
                    _ => false,
                }
            };
            if !current { return Err(StoreError::RevisionConflict { expected: expected_revision, actual: self.revision()? }); }
        }
        let mut seen = HashSet::new();
        let mutations = keys.iter().filter(|key| seen.insert(key.as_str())).map(|key| Ok(super::lww::UnitMutation::Delete { key: super::lww::unit_key(&["inlay", key])? })).collect::<StoreResult<Vec<_>>>()?;
        self.commit_with_asset_aliases(&WorkingSetCommit { expected_revision, gallery_pending_fence, unit_mutations: Some(mutations), ..Default::default() }, &[])
    }
    pub(crate) fn gallery_scan_status(&mut self) -> StoreResult<Option<ScanStatus>> {
        self.gallery_generation(&super::active_generation(&self.connection)?);
        Ok(self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner()).status.clone())
    }
    pub(crate) fn gallery_scan_cancel(&mut self) -> StoreResult<bool> {
        self.gallery_generation(&super::active_generation(&self.connection)?);
        let scan = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
        let running = scan.status.as_ref().is_some_and(|status| status.state == "running");
        if running { scan.cancel.store(true, Ordering::Release); }
        Ok(running)
    }
    pub(crate) fn gallery_locations(&mut self, key: &str) -> StoreResult<GalleryLocations> {
        self.gallery_generation(&super::active_generation(&self.connection)?);
        let scan = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
        Ok(match &scan.result {
            Some(result) => GalleryLocations { revision: Some(result.revision), state: result.state(key).map(str::to_owned), unchecked_archives: result.unchecked_archives, unchecked_records: result.unchecked_records, locations: result.locations.get(key).cloned().unwrap_or_default() },
            None => GalleryLocations { revision: None, state: None, unchecked_archives: 0, unchecked_records: 0, locations: Vec::new() },
        })
    }
}


pub(crate) fn pinned_gallery_alias(connection: &Connection, target: &ReadTarget, kind: &str, key: &str) -> StoreResult<Option<AssetAlias>> {
    Ok(super::query::read_asset_alias(connection, kind, key, target)?.map(|value| value.value))
}

impl PersistentStore {
    /// Returns the keys that are no longer attachments in the gallery.
    pub(crate) fn gallery_missing_keys(&mut self, keys: &[String]) -> StoreResult<Vec<String>> {
        let reader = self.gallery_reader()?;
        let mut statement = reader.connection.prepare("SELECT j.value FROM json_each(?2) j WHERE NOT EXISTS (SELECT 1 FROM asset_aliases a WHERE a.generation=?1 AND a.kind='inlay' AND a.logical_key=j.value AND a.inlay_type IN ('image','video','audio')) ORDER BY j.key")?;
        let rows = statement.query_map(params![reader.target.generation, serde_json::to_string(keys)?], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
    pub(crate) fn gallery_index(&mut self, query: &GalleryQuery, key: &str) -> StoreResult<Option<usize>> {
        let reader = self.gallery_reader()?;
        let index = self.gallery_order(&reader.connection, &reader.target, query)?;
        Ok(self.gallery.orders[index].keys.iter().position(|item| item == key))
    }
}

impl PersistentStore {
    pub(crate) fn gallery_usage_count(&mut self, keys: &[String]) -> StoreResult<usize> {
        self.gallery_generation(&super::active_generation(&self.connection)?);
        let scan = self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner());
        let Some(result) = &scan.result else { return Ok(0); };
        let mut seen = HashSet::new();
        Ok(keys.iter().filter(|key| seen.insert(key.as_str())).map(|key| result.locations.get(key).map_or(0, Vec::len)).sum())
    }
}

impl PersistentStore {
    pub(crate) fn gallery_scan_result(&mut self) -> StoreResult<Option<ScanStatus>> {
        self.gallery_generation(&super::active_generation(&self.connection)?);
        Ok(self.gallery.scan.lock().unwrap_or_else(|e| e.into_inner()).result.as_ref().map(|result| result.status.clone()))
    }
}
