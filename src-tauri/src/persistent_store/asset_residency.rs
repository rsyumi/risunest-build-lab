//! Physical per-device storage policy. Logical aliases and snapshot roots are
//! preserved when a verified server copy replaces a local payload file.
use super::{snapshot_archive, PersistentStore};
use crate::{
    asset_repository::{job_pins, object_physical_key, PayloadCas},
    external_storage::lww_residency::RemoteBodies,
    server_sync::{
        client::ServerClient,
        residency::{AssetPolicy, Residency},
        Result, SyncError,
    },
};
use std::collections::BTreeSet;
use rusqlite::OptionalExtension;

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum PreviousStorageTarget {
    Server { #[serde(rename = "libraryId")] library_id: String, #[serde(rename = "targetId")] target_id: String },
    External { #[serde(rename = "connectionId")] connection_id: String },
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResidencyStatus {
    policy: AssetPolicy,
    local_bytes: u64,
    remote_bytes: u64,
    remote_objects: u64,
    server_bytes: u64,
    server_objects: u64,
    external_objects: Vec<ExternalObjects>,
    unavailable_objects: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_storage_objects: Option<u64>,
    pub evicted_bytes: u64,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExternalObjects {
    connection_id: String,
    objects: u64,
}
/// Where a body this device does not hold can be fetched from. The server
/// comes first, as in hydration. A body counts for an external connection
/// only while no other live connection holds it.
enum Holder {
    Server(u64),
    External(String, u64),
    Shared(u64),
    Unavailable,
}
/// Bodies classified per registry question.
const HOLDER_PAGE: usize = 1024;
/// Connection IDs per connection root, read once per status or download.
#[derive(Default)]
struct Connections(std::collections::BTreeMap<std::path::PathBuf, BTreeSet<String>>);
impl Connections {
    fn contains(&mut self, root: &std::path::Path, id: &str) -> Result<bool> {
        if !self.0.contains_key(root) {
            // Opening the store would create it; a root without one holds no connection.
            let ids = if root.join("external-connections.sqlite").is_file() {
                crate::external_storage::connection_store::ConnectionStore::open(root)
                    .and_then(|store| store.ids())
                    .map_err(|_| SyncError::new("local-storage", 503))?
                    .into_iter()
                    .collect()
            } else {
                BTreeSet::new()
            };
            self.0.insert(root.to_owned(), ids);
        }
        Ok(self.0[root].contains(id))
    }
}
/// Whether the Sync server or a live external storage connection holds a body
/// this device does not, by the same rules as the residency status. It reads
/// only this device's records and creates none.
pub(crate) struct RemoteHolders {
    residency: Option<Residency>,
    remote: RemoteBodies,
    connections: Connections,
}
impl RemoteHolders {
    pub(crate) fn open(root: &std::path::Path) -> Result<Self> {
        Ok(Self {
            residency: Residency::exists(root).then(|| Residency::open(root)).transpose()?,
            remote: RemoteBodies::deferred(root),
            connections: Connections::default(),
        })
    }
    /// One answer per hash, in order. None of `hashes` may be held here.
    pub(crate) fn held(&mut self, hashes: &[String]) -> Result<Vec<bool>> {
        // Server custody is asked per page; `residency_holders` asks per body.
        let server = match &self.residency {
            Some(residency) => residency.active_among(hashes)?,
            None => BTreeSet::new(),
        };
        let rest = hashes.iter().filter(|hash| !server.contains(*hash)).cloned().collect::<Vec<_>>();
        let mut external = Vec::with_capacity(rest.len());
        for page in rest.chunks(HOLDER_PAGE) {
            let holders = PersistentStore::residency_holders(None, &mut self.remote, &mut self.connections, page)?;
            external.extend(holders.iter().map(|holder| !matches!(holder, Holder::Unavailable)));
        }
        let mut external = external.into_iter();
        Ok(hashes
            .iter()
            .map(|hash| server.contains(hash) || external.next().unwrap_or(false))
            .collect())
    }
}
struct Inventory {
    referenced: BTreeSet<String>,
    local: BTreeSet<String>,
    release_blocked: bool,
}
#[derive(Default)]
struct InventoryCache {
    library: Option<crate::asset_repository::migration_gc::AssetRootSet>,
    roots: Vec<(&'static str, crate::asset_repository::migration_gc::AssetRootSet)>,
    data_version: i64,
    changes: i64,
    device_version: i64,
    device_changes: i64,
    inventory: Option<Inventory>,
}
impl InventoryCache {
    /// Recheck auxiliary roots and pins under repository exclusion. Only the immutable
    /// library roots and their resolved inventory survive an unchanged observation.
    fn current(&mut self, store: &PersistentStore) -> Result<&Inventory> {
        let read = store.connection.unchecked_transaction()?;
        let data_version = read.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?;
        let changes = read.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?;
        let library_changed = self.library.is_none() || self.data_version != data_version || self.changes != changes;
        if library_changed {
            #[cfg(test)]
            super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.set(count.get() + 1));
            self.library = Some(super::snapshot::collect_asset_roots(&store.connection)?);
        }
        let roots = store.collect_auxiliary_asset_gc_roots(true, false, None)?;
        let device = store.device_store()?.connection();
        let device_version = device.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?;
        let device_changes = device.query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))?;
        if self.inventory.is_none() || self.roots != roots
            || self.data_version != data_version || self.changes != changes
            || self.device_version != device_version || self.device_changes != device_changes
            || roots.iter().any(|(_, roots)| !roots.blockers.is_empty()) {
            let mut complete = vec![("library", self.library.as_ref().unwrap().clone())];
            complete.extend(roots.iter().cloned());
            self.inventory = Some(store.residency_inventory_from(
                PayloadCas::new(&store.repository_root)?, complete, BTreeSet::new(), true)?);
        }
        read.commit()?;
        self.roots = roots;
        self.data_version = data_version;
        self.changes = changes;
        self.device_version = device_version;
        self.device_changes = device_changes;
        Ok(self.inventory.as_ref().unwrap())
    }
}
impl ResidencyStatus {
    pub(crate) fn has_remote_or_missing(&self) -> bool {
        self.remote_objects > 0 || self.unavailable_objects > 0
    }
    pub(crate) fn has_remote(&self) -> bool {
        self.remote_objects > 0
    }
}
/// The bytes of the referenced bodies this device holds, and the hashes of those it does not.
/// The inventory is sorted, so each page reaches only a few shard folders.
fn split_local(cas: &PayloadCas, referenced: BTreeSet<String>, check: &impl Fn() -> Result<()>) -> Result<(u64, Vec<String>)> {
    let referenced = referenced.into_iter().collect::<Vec<_>>();
    let mut local_bytes = 0;
    let mut missing = Vec::new();
    for page in referenced.chunks(HOLDER_PAGE) {
        check()?;
        for (hash, size) in page.iter().zip(cas.stat_objects(page.iter().map(String::as_str))?) {
            match size {
                Some(size) => local_bytes += size,
                None => missing.push(hash.clone()),
            }
        }
    }
    Ok((local_bytes, missing))
}
/// Of `page`, bodies this device does not hold, those another storage holds and `target` does not.
fn previous_storage_wanted(
    target: &PreviousStorageTarget,
    residency: &Residency,
    remote: &mut RemoteBodies,
    connections: &mut Connections,
    page: &[String],
    holders: &[Holder],
) -> Result<Vec<String>> {
    let mut external = if matches!(target, PreviousStorageTarget::External { .. }) {
        remote.holders(&page.iter().map(String::as_str).collect::<Vec<_>>())
            .map_err(|_| std::io::Error::other("external-source-invalid"))?
    } else { Default::default() };
    let mut wanted = Vec::new();
    for (hash, holder) in page.iter().zip(holders) {
        if matches!(holder, Holder::Unavailable) { continue; }
        let held = match target {
            PreviousStorageTarget::Server { library_id, target_id } => residency.target_holds(hash, library_id, target_id)?,
            PreviousStorageTarget::External { connection_id: id } => {
                let mut held = false;
                if let Some(registration) = external.remove(hash) {
                    for (root, candidate) in registration.connections {
                        if candidate == *id && connections.contains(&root, &candidate)? { held = true; break; }
                    }
                }
                held
            }
        };
        if !held { wanted.push(hash.clone()); }
    }
    Ok(wanted)
}

#[derive(Default)]
pub(super) struct CheckedRepresentativeImages {
    generation: String,
    removal_epoch: u64,
    removal_source: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    hashes: BTreeSet<String>,
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RepresentativeImagePriority {
    #[serde(default)]
    pub(crate) ordered: Vec<String>,
    #[serde(default)]
    pub(crate) visible: Vec<String>,
}

impl PersistentStore {
    fn representative_image_plan(&self, selected_character_id: Option<&str>, requested: Option<&RepresentativeImagePriority>) -> Result<(Vec<String>, BTreeSet<String>)> {
        let generation = super::active_generation(&self.connection)?;
        let mut aliases = std::collections::BTreeMap::new();
        let mut characters = std::collections::BTreeMap::new();
        let mut query = self.connection.prepare_cached(
            "SELECT c.character_id, c.image, a.object_hash FROM characters c
             JOIN asset_aliases a ON a.generation=c.generation AND a.kind='asset' AND a.logical_key=c.image
             WHERE c.generation=?1 AND c.trashed=0 AND a.object_hash IS NOT NULL",
        )?;
        let rows = query.query_map([&generation], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?;
        for row in rows {
            let (id, alias, hash) = row?;
            characters.insert(id, hash.clone());
            aliases.insert(alias, hash);
        }
        let order: Option<String> = self.connection.query_row(
            "SELECT CASE WHEN json_type(value, '$.characterOrder')='array'
             THEN json_extract(value, '$.characterOrder') END FROM root WHERE generation=?1", [&generation], |row| row.get(0),
        ).optional()?.flatten();
        let order: Vec<serde_json::Value> = order.map(|order| serde_json::from_str(&order)).transpose()
            .map_err(|_| SyncError::new("invalid-character-order", 409))?.unwrap_or_default();
        let mut display = Vec::new();
        let mut folder_alias = self.connection.prepare_cached(
            "SELECT object_hash FROM asset_aliases WHERE generation=?1 AND kind='asset' AND logical_key=?2",
        )?;
        for item in &order {
            if let Some(id) = item.as_str() {
                if let Some(hash) = characters.get(id) { display.push(hash.clone()); }
            } else {
                if let Some(alias) = item.get("imgFile").and_then(serde_json::Value::as_str) {
                    let hash: Option<String> = folder_alias.query_row(rusqlite::params![generation, alias], |row| row.get(0)).optional()?.flatten();
                    if let Some(hash) = hash { display.push(hash.clone()); aliases.insert(alias.to_owned(), hash); }
                }
                if let Some(children) = item.get("data").and_then(serde_json::Value::as_array) {
                    display.extend(children.iter().filter_map(|id| id.as_str().and_then(|id| characters.get(id))).cloned());
                }
            }
        }
        let mut priority = BTreeSet::new();
        let mut images = Vec::new();
        if let Some(requested) = requested {
            for alias in &requested.visible {
                if let Some(hash) = aliases.get(alias) { priority.insert(hash.clone()); images.push(hash.clone()); }
            }
            images.extend(requested.ordered.iter().filter_map(|alias| aliases.get(alias)).cloned());
        }
        if priority.is_empty() {
            if let Some(hash) = selected_character_id.and_then(|id| characters.get(id)) { priority.insert(hash.clone()); }
        }
        images.extend(display);
        images.extend(characters.into_values());
        let mut seen = BTreeSet::new();
        images.retain(|hash| seen.insert(hash.clone()));
        Ok((images, priority))
    }

    fn representative_image_hashes(&self, selected_character_id: Option<&str>) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
        let (images, priority) = self.representative_image_plan(selected_character_id, None)?;
        Ok((images.into_iter().collect(), priority))
    }

    fn check_representative_image_pages(
        &self,
        cas: &PayloadCas,
        generation: &str,
        images: &[String],
        retained: &BTreeSet<String>,
        page_size: usize,
        check: &impl Fn() -> Result<()>,
    ) -> Result<Vec<String>> {
        let removal_source = cas.removal_epoch();
        let mut pass_epoch = None;
        let mut offset = 0;
        let mut missing = Vec::new();
        loop {
            check()?;
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let current_generation = super::active_generation(&self.connection)?;
            let epoch = removal_source.load(std::sync::atomic::Ordering::Acquire);
            let mut cache = self.checked_representative_images.lock().map_err(|_| std::io::Error::other("checked-images-poisoned"))?;
            if current_generation != generation {
                cache.hashes.clear();
                return Err(SyncError::new("binding-authority-changed", 409));
            }
            if cache.generation != generation || cache.removal_epoch != epoch { cache.hashes.clear(); }
            cache.generation = generation.to_owned();
            cache.removal_source = Some(std::sync::Arc::clone(&removal_source));
            cache.removal_epoch = epoch;
            if pass_epoch.is_some_and(|previous| previous != epoch) {
                // A body from an earlier page may have been removed while the lock was released.
                offset = 0;
                missing.clear();
            }
            pass_epoch = Some(epoch);
            if offset == 0 { cache.hashes.retain(|hash| retained.contains(hash)); }
            if offset == images.len() { return Ok(missing); }
            let end = (offset + page_size).min(images.len());
            let page = images[offset..end].iter().filter(|hash| !cache.hashes.contains(*hash)).collect::<Vec<_>>();
            let sizes = if page.is_empty() { Vec::new() } else { cas.stat_objects(page.iter().map(|hash| hash.as_str()))? };
            if super::active_generation(&self.connection)? != generation {
                cache.hashes.clear();
                return Err(SyncError::new("binding-authority-changed", 409));
            }
            if removal_source.load(std::sync::atomic::Ordering::Acquire) != epoch {
                cache.hashes.clear();
                continue;
            }
            for (hash, size) in page.into_iter().zip(sizes) {
                if size.is_some() { cache.hashes.insert(hash.clone()); } else { missing.push(hash.clone()); }
            }
            offset = end;
        }
    }

    pub(crate) fn hydrate_representative_images(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        selected_character_id: Option<&str>,
        requested: Option<&RepresentativeImagePriority>,
        check: impl Fn() -> Result<()>,
        on_object_done: impl Fn(),
    ) -> Result<()> {
        check()?;
        let generation = super::active_generation(&self.connection)?;
        let (images, priority) = self.representative_image_plan(selected_character_id, requested)?;
        let check_authority = || {
            check()?;
            if super::active_generation(&self.connection)? != generation {
                return Err(SyncError::new("binding-authority-changed", 409));
            }
            Ok(())
        };
        let cas = PayloadCas::new(&self.repository_root)?;
        let retained = images.iter().cloned().collect::<BTreeSet<_>>();
        let mut missing = self.check_representative_image_pages(&cas, &generation, &images, &retained, HOLDER_PAGE, &check)?;
        if missing.is_empty() { return check_authority(); }
        // An icon no storage holds remains retryable and is reported by the data check.
        let held = RemoteHolders::open(&self.repository_root)?.held(&missing)?;
        missing = missing.into_iter().zip(held).filter_map(|(hash, held)| held.then_some(hash)).collect();
        if missing.is_empty() { return check_authority(); }
        let mut hydration = crate::server_sync::residency::HydrationSession::new(&self.repository_root, cancellation)?;
        let mut completed = Vec::new();
        hydration.hydrate_many_outcomes_prioritized(&missing, &priority, &check_authority, |hash, _| {
            completed.push(hash.to_owned()); on_object_done();
        })?;
        // Recheck completed images under the offload lock before remembering them.
        self.check_representative_image_pages(&cas, &generation, &completed, &retained, HOLDER_PAGE, &check)?;
        check_authority()
    }

    pub(crate) fn asset_object_byte_size(&self, hash: &str) -> super::StoreResult<Option<u64>> {
        let size: Option<i64> = self.connection.query_row(
            "SELECT byte_size FROM asset_objects WHERE object_hash=?1", [hash], |row| row.get(0),
        ).optional()?;
        Ok(size.and_then(|size| u64::try_from(size).ok()))
    }
    /// Forgets the bodies a removed external connection held, with the catalog
    /// rows of those that no local file, other source or server custody holds
    /// any more, which GC would otherwise find missing.
    pub(crate) fn forget_external_connection_bodies(&self, connection_id: &str) -> Result<()> {
        let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let residency = match Residency::exists(&self.repository_root) {
            true => Some(Residency::open(&self.repository_root)?),
            false => None,
        };
        crate::external_storage::lww_residency::remove_connection_sources(&self.repository_root, connection_id, |orphans| {
            let rows = self.connection.unchecked_transaction().map_err(std::io::Error::other)?;
            let sizes = cas.stat_objects(orphans.iter().map(String::as_str))?;
            for (hash, size) in orphans.iter().zip(sizes) {
                if size.is_some() {
                    continue;
                }
                if let Some(residency) = &residency {
                    if residency.custody_size(hash).map_err(|error| std::io::Error::other(error.code))?.is_some() {
                        continue;
                    }
                }
                rows.execute("DELETE FROM asset_objects WHERE object_hash=?1", [hash]).map_err(std::io::Error::other)?;
            }
            rows.commit().map_err(std::io::Error::other)
        })
        .map_err(|error| SyncError { cause: Some(format!("{:?}", error.kind)), ..SyncError::new("local-storage", 503) })
    }
    pub(crate) fn selected_character_asset_hashes(&self, character_id: &str) -> super::StoreResult<Vec<String>> {
        let generation = super::active_generation(&self.connection)?;
        let mut roots = crate::asset_repository::migration_gc::AssetRootSet::default();
        for query in [
            "SELECT detail FROM characters WHERE generation=?1 AND character_id=?2",
            "SELECT detail FROM conversations WHERE generation=?1 AND character_id=?2",
            "SELECT value FROM messages WHERE generation=?1 AND character_id=?2",
        ] {
            let mut statement = self.connection.prepare(query)?;
            let rows = statement.query_map(rusqlite::params![generation, character_id], |row| row.get::<_,String>(0))?;
            for row in rows { super::snapshot::observe_json_value(&serde_json::from_str(&row?)?, None, &mut roots); }
        }
        let image: Option<String> = self.connection.query_row(
            "SELECT image FROM characters WHERE generation=?1 AND character_id=?2",
            rusqlite::params![generation, character_id], |row| row.get(0),
        ).optional()?.flatten();
        if let Some(image) = image { super::snapshot::observe_json_value(&serde_json::Value::String(image), None, &mut roots); }
        let mut hashes = std::mem::take(&mut roots.object_hashes);
        let mut candidates = roots.legacy_asset_keys.into_iter().chain(roots.inlay_ids).collect();
        super::snapshot::collect_alias_key_candidates(&self.connection, &generation, character_id, &mut candidates, false)?;
        for candidate in candidates {
            let mut statement = self.connection.prepare_cached(
                "SELECT object_hash FROM asset_aliases WHERE generation=?1 AND logical_key=?2 AND object_hash IS NOT NULL",
            )?;
            for hash in statement.query_map(rusqlite::params![generation, candidate], |row| row.get::<_,String>(0))? { hashes.insert(hash?); }
        }
        let manifest: Option<String> = self.connection.query_row(
            "SELECT manifest_hash FROM asset_owner_heads WHERE generation=?1 AND owner_kind='character-additional-assets' AND owner_locator=?2 AND present=1",
            rusqlite::params![generation, character_id], |row| row.get(0),
        ).optional()?.flatten();
        hashes.extend(manifest);
        if let Some(archived) = super::archive::read_archived_object(&self.connection, &generation, character_id)? {
            hashes.extend(archived.object_roots().map(str::to_owned));
        }
        Ok(hashes.into_iter().collect())
    }
    pub(crate) fn hydrate_registered_remote_assets(
        &self,
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.hydrate_registered_remote_assets_cancelled(None, check)
    }
    pub(crate) fn hydrate_registered_remote_assets_cancelled(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.hydrate_registered_remote_assets_observed(cancellation, check, || {})
    }
    pub(crate) fn hydrate_registered_remote_assets_observed(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: impl Fn() -> Result<()>,
        on_object_done: impl Fn(),
    ) -> Result<()> {
        self.hydrate_registered_remote_assets_prioritized(cancellation, None, check, on_object_done)
    }
    pub(crate) fn hydrate_registered_remote_assets_prioritized(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        selected_character_id: Option<&str>,
        check: impl Fn() -> Result<()>,
        on_object_done: impl Fn(),
    ) -> Result<()> {
        if let Some(lane) = crate::server_sync::progress::current() { lane.asset_plan(None); }
        let residency = Residency::exists(&self.repository_root).then(||Residency::open(&self.repository_root)).transpose()?;
        let mut hydration = crate::server_sync::residency::HydrationSession::new(&self.repository_root, cancellation)?;
        let priority=selected_character_id.map(|id|self.selected_character_asset_hashes(id)).transpose()?.unwrap_or_default().into_iter().collect::<BTreeSet<_>>();
        let mut inventory=priority.iter().cloned().collect::<Vec<_>>();
        let mut included=priority.clone();
        let mut cursor = None;
        loop {
            check()?;
            let page = self.query_asset_object_catalog(128, cursor.as_deref())?;
            for object in page.items {if included.insert(object.object_hash.clone()) {inventory.push(object.object_hash);}}
            cursor = page.next_cursor;
            if cursor.is_none() {break;}
        }
        // Restoring an older snapshot can roll back the live catalog while
        // newer retained snapshots still own remote payloads. GC protects these
        // roots independently of catalog membership; backup must do the same.
        let roots = snapshot_archive::Archive::open(&self.snapshots_dir)?.roots()?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let mut remote = crate::external_storage::lww_residency::RemoteBodies::open(&self.repository_root)?;
        let mut historical = BTreeSet::new();
        let mut manifests = BTreeSet::new();
        for root in roots {
            historical.extend(root.object_hashes);
            manifests.extend(root.manifest_hashes);
        }
        for manifest in manifests {
            // Ownership manifests must be available before their payload inventory is known.
            let registered=residency.as_ref().map(|residency|residency.object(&manifest,None)).transpose()?.flatten().is_some()
                || remote.stat(&manifest)?.is_some();
            if registered && !hydration.hydrate_many_before_plan(std::slice::from_ref(&manifest),&check,&on_object_done)?.is_empty() {
                return Err(SyncError::new("required-asset-unavailable",409));
            }
            // Missing or damaged local files remain for the backup's existing
            // preservation path. Only verified manifests supply dependencies.
            if let Some(bytes) = cas.read_object(&manifest)? {
                if risunest_sync_wire::hash(&bytes) == manifest {
                    if let Ok(entries) =
                        crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&bytes)
                    {
                        historical.extend(
                            entries
                                .into_iter()
                                .filter_map(|entry| entry.payload_hash.map(hex::encode)),
                        );
                    }
                }
            }
        }
        for hash in historical {if included.insert(hash.clone()) {inventory.push(hash);}}
        let mut registered=Vec::new();
        for hash in inventory {
            check()?;
            if residency.as_ref().map(|residency|residency.object(&hash,None)).transpose()?.flatten().is_some()
                || remote.stat(&hash)?.is_some() {registered.push(hash);}
        }
        if !hydration.hydrate_many_outcomes_prioritized(&registered,&priority,&check,|_,_|on_object_done())?.is_empty() {
            return Err(SyncError::new("required-asset-unavailable",409));
        }
        check()
    }
    fn residency_inventory(&self, guarded: bool) -> Result<Inventory> {
        let cas = PayloadCas::new(&self.repository_root)?;
        let mut referenced = BTreeSet::new();
        let roots = self.collect_asset_gc_roots_with_backup_references(guarded, false, Some(&mut referenced))?;
        self.residency_inventory_from(cas, roots, referenced, guarded)
    }
    /// The inventory a residency status reports, which may reuse the library roots an
    /// earlier status read.
    fn residency_status_inventory(&self) -> Result<Inventory> {
        let cas = PayloadCas::new(&self.repository_root)?;
        let roots = self.collect_status_asset_gc_roots()?;
        self.residency_inventory_from(cas, roots, BTreeSet::new(), false)
    }
    fn residency_inventory_from(
        &self,
        cas: PayloadCas,
        roots: Vec<(&'static str, crate::asset_repository::migration_gc::AssetRootSet)>,
        mut referenced: BTreeSet<String>,
        guarded: bool,
    ) -> Result<Inventory> {
        let mut local = BTreeSet::new();
        // This is offload protection, not evidence that the body exists on this device.
        local.extend(self.representative_image_hashes(None)?.0);
        let mut manifests = BTreeSet::new();
        let mut release_blocked = false;
        for (label, root) in roots {
            release_blocked |= root.retain_all_objects || !root.blockers.is_empty();
            if label == "external-conflict" {
                local.extend(root.object_hashes.iter().cloned());
                local.extend(root.manifest_hashes.iter().cloned());
            }
            // External captures require their payloads, held locally or in
            // custody, through publication, even when the source alias was
            // removed by a later edit.
            if label == "remote" {
                local.extend(root.object_hashes.iter().cloned());
            }
            if label == "server-conflict" {
                local.extend(root.object_hashes);
            } else {
                referenced.extend(root.object_hashes);
            }
            referenced.extend(root.manifest_hashes.iter().cloned());
            manifests.extend(root.manifest_hashes.iter().cloned());
            local.extend(root.manifest_hashes);
        }
        for hash in &manifests {
            let bytes = cas
                .read_object(hash)?
                .ok_or_else(|| SyncError::new("local-manifest-missing", 409))?;
            if risunest_sync_wire::hash(&bytes) != *hash {
                return Err(SyncError::new("local-manifest-corrupt", 409));
            }
            for entry in
                crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&bytes)
                    .map_err(|_| SyncError::new("local-manifest-corrupt", 409))?
            {
                if let Some(hash) = entry.payload_hash {
                    referenced.insert(hex::encode(hash));
                }
            }
        }
        // Detached consumers still require their physical inputs.
        for root in self.active_readers.detached_asset_roots()? {
            local.extend(root.object_hashes);
            local.extend(root.manifest_hashes);
        }
        let jobs = if guarded {
            crate::asset_repository::job_pins::collect_durable_cas_job_roots_already_guarded(
                &self.repository_root,
            )
        } else {
            crate::asset_repository::job_pins::collect_durable_cas_job_roots(&self.repository_root)
        };
        if !jobs.blockers.is_empty() {
            let busy = jobs.blockers.iter().all(|blocker| {
                let Some(id) = blocker.strip_prefix("job-pin-unsealed:") else {
                    return false;
                };
                if job_pins::durable_cas_job_held(&self.repository_root, id).unwrap_or(false) {
                    return true;
                }
                // The worker may have sealed or released its job since collection.
                match job_pins::inspect_durable_cas_job(&self.repository_root, id) {
                    Ok(None) => true,
                    Ok(Some(job)) => job.sealed,
                    Err(_) => false,
                }
            });
            return Err(SyncError::new(if busy { "asset-jobs-busy" } else { "asset-jobs-unresolved" }, 409));
        }
        local.extend(jobs.object_hashes);
        local.extend(jobs.manifest_hashes);
        Ok(Inventory {
            referenced,
            local,
            release_blocked,
        })
    }
    /// Where each of `hashes`, none of them held here, can be fetched from.
    fn residency_holders(
        residency: Option<&Residency>,
        remote: &mut RemoteBodies,
        connections: &mut Connections,
        hashes: &[String],
    ) -> Result<Vec<Holder>> {
        let sizes = match residency {
            Some(residency) => residency.active_sizes(hashes)?,
            None => vec![None; hashes.len()],
        };
        let mut server = Vec::with_capacity(hashes.len());
        let mut external = Vec::new();
        for (hash, size) in hashes.iter().zip(sizes) {
            if size.is_none() {
                external.push(hash.as_str());
            }
            server.push(size.map(Holder::Server));
        }
        let mut registered = remote
            .holders(&external)
            .map_err(|_| std::io::Error::other("external-source-invalid"))?;
        let mut holders = Vec::with_capacity(hashes.len());
        for (hash, server) in hashes.iter().zip(server) {
            if let Some(holder) = server {
                holders.push(holder);
                continue;
            }
            let Some(held) = registered.remove(hash) else {
                holders.push(Holder::Unavailable);
                continue;
            };
            let mut live = Vec::new();
            for (root, id) in held.connections {
                if connections.contains(&root, &id)? {
                    live.push(id);
                }
            }
            holders.push(match live.len() {
                0 => Holder::Unavailable,
                1 => Holder::External(live.remove(0), held.byte_length),
                _ => Holder::Shared(held.byte_length),
            });
        }
        Ok(holders)
    }
    pub(crate) fn asset_residency_status(&self) -> Result<ResidencyStatus> {
        self.asset_residency_status_for_target(None)
    }
    /// The status, with what moving to `target` would download when one is given, read from
    /// one inventory.
    pub(crate) fn asset_residency_status_for_target(&self, target: Option<&PreviousStorageTarget>) -> Result<ResidencyStatus> {
        self.residency_status_from(self.residency_status_inventory()?, target)
    }
    fn residency_status_from(&self, inventory: Inventory, target: Option<&PreviousStorageTarget>) -> Result<ResidencyStatus> {
        let residency = Residency::open(&self.repository_root)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let mut connections = Connections::default();
        let (local_bytes, missing) = split_local(&cas, inventory.referenced, &|| Ok(()))?;
        let mut status = ResidencyStatus {
            policy: self.device_store()?.asset_residency_policy()?,
            local_bytes,
            remote_bytes: 0,
            remote_objects: 0,
            server_bytes: 0,
            server_objects: 0,
            external_objects: Vec::new(),
            unavailable_objects: 0,
            previous_storage_objects: target.map(|_| 0),
            evicted_bytes: 0,
        };
        let mut external = std::collections::BTreeMap::<String, u64>::new();
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        for page in missing.chunks(HOLDER_PAGE) {
            let holders = Self::residency_holders(Some(&residency), &mut remote, &mut connections, page)?;
            if let (Some(target), Some(objects)) = (target, status.previous_storage_objects.as_mut()) {
                *objects += previous_storage_wanted(target, &residency, &mut remote, &mut connections, page, &holders)?.len() as u64;
            }
            for holder in holders {
                match holder {
                    Holder::Server(size) => {
                        status.server_bytes += size;
                        status.server_objects += 1;
                        status.remote_bytes += size;
                        status.remote_objects += 1;
                    }
                    Holder::External(connection_id, size) => {
                        *external.entry(connection_id).or_default() += 1;
                        status.remote_bytes += size;
                        status.remote_objects += 1;
                    }
                    Holder::Shared(size) => {
                        status.remote_bytes += size;
                        status.remote_objects += 1;
                    }
                    Holder::Unavailable => status.unavailable_objects += 1,
                }
            }
        }
        status.external_objects = external
            .into_iter()
            .map(|(connection_id, objects)| ExternalObjects { connection_id, objects })
            .collect();
        Ok(status)
    }
    /// The status's `externalObjects` count for one connection. Only the
    /// bodies registered to that connection are classified, and the library
    /// is read only when one of them is held by that connection alone.
    pub(crate) fn asset_residency_connection_objects(&self, connection_id: &str) -> Result<u64> {
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        let registered = remote
            .connection_hashes(connection_id)
            .map_err(|_| std::io::Error::other("external-source-invalid"))?;
        if registered.is_empty() {
            return Ok(0);
        }
        let (_, missing) = split_local(&PayloadCas::new(&self.repository_root)?, registered, &|| Ok(()))?;
        let residency = Residency::exists(&self.repository_root)
            .then(|| Residency::open(&self.repository_root))
            .transpose()?;
        let mut connections = Connections::default();
        let mut alone = Vec::new();
        for page in missing.chunks(HOLDER_PAGE) {
            let holders = Self::residency_holders(residency.as_ref(), &mut remote, &mut connections, page)?;
            for (hash, holder) in page.iter().zip(holders) {
                if matches!(holder, Holder::External(holder, _) if holder == connection_id) {
                    alone.push(hash);
                }
            }
        }
        if alone.is_empty() {
            return Ok(0);
        }
        let referenced = self.residency_status_inventory()?.referenced;
        Ok(alone.into_iter().filter(|hash| referenced.contains(*hash)).count() as u64)
    }
    fn previous_storage_assets(&self, target: &PreviousStorageTarget, check: &impl Fn() -> Result<()>) -> Result<BTreeSet<String>> {
        let inventory = self.residency_inventory(false)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let residency = Residency::open(&self.repository_root)?;
        let (_, missing) = split_local(&cas, inventory.referenced, check)?;
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        let mut connections = Connections::default();
        let mut wanted = BTreeSet::new();
        for page in missing.chunks(HOLDER_PAGE) {
            check()?;
            let available = Self::residency_holders(Some(&residency), &mut remote, &mut connections, page)?;
            wanted.extend(previous_storage_wanted(target, &residency, &mut remote, &mut connections, page, &available)?);
        }
        Ok(wanted)
    }
    pub(crate) fn asset_residency_download_previous(
        &self, target: &PreviousStorageTarget,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        selected_character_id: Option<&str>, check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        check()?;
        let targets = self.previous_storage_assets(target, &check)?;
        self.hydrate_residency_targets(targets, cancellation, selected_character_id, check)
    }
    /// Downloads the bodies another storage holds without changing the policy:
    /// every one, or only those `connection_id` holds and no other live
    /// connection does. A body no storage holds is left as it is.
    pub(crate) fn asset_residency_download_remote(
        &self,
        connection_id: Option<&str>,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        selected_character_id: Option<&str>,
        check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        check()?;
        let inventory = self.residency_inventory(false)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let residency = Residency::open(&self.repository_root)?;
        let mut connections = Connections::default();
        let mut targets = BTreeSet::new();
        let (_, missing) = split_local(&cas, inventory.referenced, &check)?;
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        for page in missing.chunks(HOLDER_PAGE) {
            check()?;
            let holders = Self::residency_holders(Some(&residency), &mut remote, &mut connections, page)?;
            for (hash, holder) in page.iter().zip(holders) {
                let wanted = match holder {
                    Holder::Server(_) | Holder::Shared(_) => connection_id.is_none(),
                    Holder::External(holder, _) => connection_id.is_none_or(|wanted| wanted == holder),
                    Holder::Unavailable => false,
                };
                if wanted {
                    targets.insert(hash.clone());
                }
            }
        }
        self.hydrate_residency_targets(targets, cancellation, selected_character_id, check)
    }
    fn hydrate_residency_targets(
        &self, targets: BTreeSet<String>, cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        selected_character_id: Option<&str>, check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        let mut hydration = crate::server_sync::residency::HydrationSession::new(&self.repository_root, cancellation)?;
        let progress = crate::server_sync::asset_download_progress::plan(targets.len());
        let mut completed = BTreeSet::new();
        let mut opened = |hash: &str, _| { if completed.insert(hash.to_owned()) { progress.completed(0); } };
        if let Some(character_id) = selected_character_id {
            let selected = self.selected_character_asset_hashes(character_id)?.into_iter()
                .filter(|hash| targets.contains(hash)).collect::<Vec<_>>();
            if !hydration.hydrate_many_outcomes(&selected, &check, &mut opened)?.is_empty() {
                return Err(SyncError::new("required-asset-unavailable", 409));
            }
        }
        if !hydration.hydrate_many_outcomes(&targets.into_iter().collect::<Vec<_>>(), &check, &mut opened)?.is_empty() {
            return Err(SyncError::new("required-asset-unavailable", 409));
        }
        let status = self.asset_residency_status()?;
        check()?;
        progress.flush();
        Ok(status)
    }
    pub(crate) fn asset_residency_set_policy(
        &self,
        policy: AssetPolicy,
        check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        self.asset_residency_set_policy_cancelled(policy, None, check)
    }
    pub(crate) fn asset_residency_set_policy_cancelled(
        &self,
        policy: AssetPolicy,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        self.asset_residency_set_policy_prioritized(policy, cancellation, None, check)
    }
    pub(crate) fn asset_residency_set_policy_prioritized(
        &self,
        policy: AssetPolicy,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        selected_character_id: Option<&str>,
        check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        check()?;
        if policy == AssetPolicy::Remote && self.server_stored_config()?.is_none() {
            return Err(SyncError::new("server-not-bound", 409));
        }
        self.device_store()?.set_asset_residency_policy(policy)?;
        if policy == AssetPolicy::Full {
            let inventory = self.residency_inventory(false)?;
            let mut hydration = crate::server_sync::residency::HydrationSession::new(&self.repository_root, cancellation)?;
            if let Some(character_id) = selected_character_id {
                let selected = self.selected_character_asset_hashes(character_id)?.into_iter()
                    .filter(|hash| inventory.referenced.contains(hash)).collect::<Vec<_>>();
                if !hydration.hydrate_many(&selected, &check)?.is_empty() {
                    return Err(SyncError::new("required-asset-unavailable", 409));
                }
            }
            let unavailable = hydration.hydrate_many(&inventory.referenced.into_iter().collect::<Vec<_>>(), &check)?;
            if !unavailable.is_empty() {
                return Err(SyncError::new("required-asset-unavailable", 409));
            }
        }
        let status = self.asset_residency_status()?;
        check()?;
        Ok(status)
    }
    pub(crate) fn asset_residency_evict(
        &self,
        check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        self.asset_residency_evict_cancelled(None, check)
    }
    pub(crate) fn asset_residency_evict_cancelled(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: impl Fn() -> Result<()>,
    ) -> Result<ResidencyStatus> {
        self.asset_residency_evict_observed(cancellation, check, &mut crate::server_sync::asset_cleanup_progress::Progress::new(|_| {}))
    }
    pub(crate) fn asset_residency_evict_observed(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: impl Fn() -> Result<()>,
        progress: &mut crate::server_sync::asset_cleanup_progress::Progress,
    ) -> Result<ResidencyStatus> {
        use crate::server_sync::asset_cleanup_progress::Stage;
        progress.stage(Stage::Preparing, 0, None);
        check()?;
        let mut residency = Residency::open(&self.repository_root)?;
        if self.device_store()?.asset_residency_policy()? != AssetPolicy::Remote {
            return Err(SyncError::new("remote-asset-policy-required", 409));
        }

        let mut config = self
            .server_stored_config()?
            .ok_or_else(|| SyncError::new("server-not-bound", 409))?;
        let client = ServerClient::with_cancellation(config.resolve(&self.repository_root)?, cancellation.clone())?;
        let head = client.resolve_identity()?;
        check()?;
        config.endpoint = client.config().endpoint.clone();
        let mut inventory = InventoryCache::default();
        let candidates = {
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let current = inventory.current(self)?;
            current.referenced.difference(&current.local).cloned().collect::<Vec<_>>()
        };
        let cas = PayloadCas::new(&self.repository_root)?;
        let mut evicted = 0;
        let mut objects = Vec::new();
        for page in candidates.chunks(1024) {
            check()?;
            for (hash, size) in page.iter().zip(cas.stat_objects(page.iter().map(String::as_str))?) {
                if let Some(size) = size { objects.push((hash.clone(), size)); }
            }
        }
        let total = objects.len();
        progress.stage(Stage::Checking, 0, Some(total));
        for (index, page) in objects.chunks(1024).enumerate() {
            check()?;
            let requested = page.iter().map(|(hash, size)| (hash.clone(), Some(*size))).collect::<Vec<_>>();
            // Retention checks current server presence and size in the same
            // transaction as the custody grant. Most pages need only this call.
            if let Err(error) = residency.retain(&client, &config, &head, &requested) {
                // A missing body can also surface as storage-io while its
                // catalog row still exists. Probe once, then verify any repair.
                if !matches!(error.code.as_str(), "object-not-found" | "corrupt-object" | "storage-io") { return Err(error); }
                let identities = page.iter().map(|(hash, size)| serde_json::json!({"hash":hash,"size":size.to_string()})).collect::<Vec<_>>();
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Missing { missing: Vec<String> }
                let (_, missing): (_, Missing) = client.json(reqwest::Method::POST, "objects/missing", &[], Some(&identities), &[])?;
                let directory = tempfile::Builder::new().prefix("asset-offload-").tempdir_in(&self.repository_root)?;
                let cache = crate::server_sync::cache::Cache::open(directory.path())?.with_library(&self.repository_root)?;
                progress.stage(Stage::Uploading, 0, Some(missing.missing.len()));
                let mut expected = page.iter().cloned().collect::<std::collections::BTreeMap<_, _>>();
                for hash in missing.missing {
                    check()?;
                    let size = expected.remove(&hash).ok_or_else(|| SyncError::new("invalid-missing-response", 502))?;
                    // A snapshot-only object may never have been published.
                    if cache.stat_object(&hash)? != Some(size) { return Err(SyncError::new("asset-changed", 409)); }
                    cache.verify(&hash, &check)?;
                    crate::server_sync::transfer::Transfer::new(&client, &cache)?
                        .with_check(&check).upload_with_hints(&[hash], &[], false, &Default::default())?;
                    progress.completed(None);
                }
                check()?;
                residency.retain(&client, &config, &head, &requested)?;
            }
            progress.stage(Stage::Checking, index * 1024 + page.len(), Some(total));
        }
        if !objects.is_empty() {
            check()?;
            progress.stage(Stage::Preparing, 0, None);
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let fresh = inventory.current(self)?;
            progress.stage(Stage::Deleting, 0, Some(total));
            for (hash, size) in objects {
                check()?;
                if fresh.local.contains(&hash) {
                    progress.completed(None);
                    continue;
                }
                // Keep aliases and their object catalog row. Offloading is
                // neither a logical deletion nor an asset GC tombstone.
                if let crate::asset_repository::ExactObjectUnlink::Removed { .. } =
                    cas.unlink_retained_object(&hash, size, &object_physical_key(&hash))?
                {
                    evicted += size;
                    progress.completed(Some(size));
                } else { progress.completed(None); }
            }
        }
        progress.stage(Stage::Finalizing, total, None);
        self.release_unused_with_inventory(cancellation, &check, &mut inventory)?;
        {
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
            inventory.current(self)?;
        }
        let mut status = self.residency_status_from(inventory.inventory.take().unwrap(), None)?;
        check()?;
        status.evicted_bytes = evicted;
        Ok(status)
    }
    pub(crate) fn asset_residency_release_unused(
        &self,
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.asset_residency_release_unused_cancelled(None, check)
    }
    pub(crate) fn asset_residency_release_unused_cancelled(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.release_unused_with_inventory(cancellation, &check, &mut InventoryCache::default())
    }
    fn release_unused_with_inventory(
        &self,
        cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        check: &impl Fn() -> Result<()>,
        inventory: &mut InventoryCache,
    ) -> Result<()> {
        let mut residency = Residency::open(&self.repository_root)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let revision = self.revision()?;
        let mut after = String::new();
        loop {
            let page = residency.page(&after)?;
            if page.is_empty() {
                break;
            }
            let mut candidates = std::collections::BTreeMap::<String, Vec<String>>::new();
            {
                let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                let used = inventory.current(self)?;
                if used.release_blocked { return Ok(()); }
                for (cursor, hash, _) in page {
                    check()?;
                    after = cursor.clone();
                    if used.referenced.contains(&hash) || used.local.contains(&hash) {
                        continue;
                    }
                    let context = cursor
                        .split_once('/')
                        .ok_or_else(|| SyncError::new("invalid-custody-cursor", 409))?
                        .0;
                    candidates.entry(context.to_owned()).or_default().push(hash);
                }
            }
            for (context, hashes) in candidates {
                check()?;
                let mut proof = None;
                for hash in &hashes {
                    if let Some(object) = residency.release_object(hash, &context)? {
                        proof = Some(object);
                        break;
                    }
                }
                let Some(proof) = proof else {
                    continue;
                };
                let client = ServerClient::with_cancellation(proof.config.resolve(&self.repository_root)?, cancellation.clone())?;
                let head = client.resolve_identity()?;
                check()?;
                let objects = {
                    let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                    if self.revision()? != revision {
                        return Ok(());
                    }
                    let inventory = inventory.current(self)?;
                    if inventory.release_blocked {
                        return Ok(());
                    }
                    let mut objects = Vec::new();
                    let sizes = cas.stat_objects(hashes.iter().map(String::as_str))?;
                    let rows = self.connection.unchecked_transaction()?;
                    for (hash, size) in hashes.into_iter().zip(sizes) {
                        if inventory.referenced.contains(&hash) || inventory.local.contains(&hash) {
                            continue;
                        }
                        let Some(object) = residency.begin_latest_release(&hash, &context)? else {
                            continue;
                        };
                        if size.is_none() {
                            rows.execute(
                                "DELETE FROM asset_objects WHERE object_hash=?1",
                                [&hash],
                            )?;
                        }
                        objects.push(object);
                    }
                    rows.commit()?;
                    objects
                };
                if objects.is_empty() {
                    continue;
                }
                let reply=client.request(reqwest::Method::POST,"objects/retention/release",&[],
                    Some(risunest_sync_wire::canonical::encode(&serde_json::json!({"epoch":head.epoch,"objects":objects.iter().map(|object|serde_json::json!({"deviceId":object.device_id,"hash":object.hash,"retentionId":object.retention_id})).collect::<Vec<_>>()}))?),&[],risunest_sync_wire::MAX_METADATA_BYTES)?;
                if reply.status != 204 {
                    return Err(crate::server_sync::client::response_error(reply));
                }
                for object in &objects {
                    residency.finish_release(object)?;
                }
            }
        }
        check()
    }
}

#[cfg(test)]
impl PersistentStore {
    pub(crate) fn residency_inventory_classification_test(
        &self,
        guarded: bool,
    ) -> Result<(BTreeSet<String>, BTreeSet<String>, bool)> {
        let inventory = self.residency_inventory(guarded)?;
        Ok((inventory.referenced, inventory.local, inventory.release_blocked))
    }
    pub(crate) fn residency_backup_classification_test(
        &self,
        guarded: bool,
    ) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
        let mut referenced = BTreeSet::new();
        let roots = self.collect_asset_gc_roots_with_backup_references(guarded, false, Some(&mut referenced))?;
        let local = roots.into_iter().filter(|(label, _)| *label == "server-conflict")
            .flat_map(|(_, roots)| roots.object_hashes).collect();
        Ok((referenced, local))
    }
}

#[cfg(test)]
mod representative_image_cache_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn representative_pages_release_the_media_lock_between_stats() {
        let directory = tempfile::tempdir().unwrap();
        let store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let images = [b"first synthetic icon", b"other synthetic icon"].iter()
            .map(|bytes| cas.prepare_bytes(*bytes).unwrap().content_hash).collect::<Vec<_>>();
        let retained = images.iter().cloned().collect();
        let generation = super::super::active_generation(&store.connection).unwrap();
        let (start_tx, start_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let media = std::thread::spawn(move || {
            if start_rx.recv().is_ok() {
                let guard = crate::asset_repository::coordinator::lock_repository_mutation().unwrap();
                drop(guard);
                let _ = done_tx.send(());
            }
        });
        let boundary = Cell::new(0);
        let result = store.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| {
            boundary.set(boundary.get() + 1);
            if boundary.get() == 2 {
                start_tx.send(()).unwrap();
                done_rx.recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| SyncError::new("media-lock-blocked-between-pages", 500))?;
            }
            Ok(())
        });
        drop(start_tx);
        media.join().unwrap();
        assert!(result.unwrap().is_empty());
        assert!(boundary.get() >= 2);
        assert!(store.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| Ok(())).unwrap().is_empty());
    }

    #[test]
    fn representative_pages_discard_cached_bodies_after_an_intervening_removal() {
        let directory = tempfile::tempdir().unwrap();
        let store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let first = cas.prepare_bytes(b"first synthetic icon").unwrap();
        let other = cas.prepare_bytes(b"other synthetic icon").unwrap();
        let images = vec![first.content_hash.clone(), other.content_hash.clone()];
        let retained = images.iter().cloned().collect();
        let generation = super::super::active_generation(&store.connection).unwrap();
        let boundary = Cell::new(0);
        let missing = store.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| {
            boundary.set(boundary.get() + 1);
            if boundary.get() == 2 {
                let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                cas.unlink_exact_object(&first.content_hash, first.byte_size, &first.physical_key)?;
            }
            Ok(())
        }).unwrap();
        assert_eq!(missing, [first.content_hash.clone()]);
        let job = store.open_native_job_store().unwrap();
        assert_eq!(job.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| Ok(())).unwrap(), missing);
        cas.prepare_bytes(b"first synthetic icon").unwrap();
        assert!(job.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| Ok(())).unwrap().is_empty());
        crate::asset_repository::body_io::reset_body_io();
        assert!(store.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| Ok(())).unwrap().is_empty());
        assert_eq!(crate::asset_repository::body_io::take_body_io().batch_stat_requests, 0);
    }

    #[test]
    fn representative_pages_discard_cached_bodies_after_a_generation_change() {
        let directory = tempfile::tempdir().unwrap();
        let store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let images = [b"first synthetic icon", b"other synthetic icon"].iter()
            .map(|bytes| cas.prepare_bytes(*bytes).unwrap().content_hash).collect::<Vec<_>>();
        let retained = images.iter().cloned().collect();
        let generation = super::super::active_generation(&store.connection).unwrap();
        let boundary = Cell::new(0);
        let result = store.check_representative_image_pages(&cas, &generation, &images, &retained, 1, &|| {
            boundary.set(boundary.get() + 1);
            if boundary.get() == 2 {
                let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
                store.connection.execute("UPDATE meta SET value='\"replacement-generation\"' WHERE key='activeGeneration'", [])?;
            }
            Ok(())
        });
        assert_eq!(result.unwrap_err().code, "binding-authority-changed");
        assert!(store.checked_representative_images.lock().unwrap().hashes.is_empty());
    }
}

#[cfg(test)]
mod inventory_cache_tests {
    use super::*;
    use crate::asset_repository::job_pins::{CasJobKind, CasJobOwner, CasObjectRole, CasReleaseOutcome, DurableCasJob};

    #[test]
    fn an_operation_reuses_library_roots_but_observes_auxiliary_roots_and_job_pins() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let payload = cas.prepare_bytes(b"synthetic cached inventory body").unwrap();
        let mut cache = InventoryCache::default();
        let read = |cache: &mut InventoryCache, store: &PersistentStore| {
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation().unwrap();
            cache.current(store).map(|inventory| (inventory.referenced.clone(), inventory.local.clone()))
        };
        super::super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.set(0));
        assert!(!read(&mut cache, &store).unwrap().0.contains(&payload.content_hash));
        read(&mut cache, &store).unwrap();
        assert_eq!(super::super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.get()), 1);

        let mut archive = snapshot_archive::Archive::open(&store.snapshots_dir).unwrap();
        let scratch = archive.scratch().unwrap();
        std::fs::write(&scratch.path, b"synthetic archived bytes").unwrap();
        let mut roots = crate::asset_repository::migration_gc::AssetRootSet::default();
        roots.object_hashes.insert(payload.content_hash.clone());
        let snapshot = archive.insert(&scratch.path, 0, "synthetic", roots).unwrap();
        drop(archive);
        assert!(read(&mut cache, &store).unwrap().0.contains(&payload.content_hash));
        snapshot_archive::Archive::open(&store.snapshots_dir).unwrap().delete(&snapshot.id).unwrap();
        assert!(!read(&mut cache, &store).unwrap().0.contains(&payload.content_hash));
        assert_eq!(super::super::ASSET_GC_LIBRARY_ROOT_COLLECTIONS.with(|count| count.get()), 1);

        let mut job = DurableCasJob::begin(directory.path(), "synthetic-cache-pin", CasJobKind::DirectAssetOrInlayWrite, CasJobOwner::for_test(), 1).unwrap();
        let error = read(&mut cache, &store).unwrap_err();
        assert_eq!(error.code, "asset-jobs-busy");
        job.pin_existing(&cas, &payload.content_hash, b"synthetic cached inventory body".len() as u64, CasObjectRole::DirectObject).unwrap();
        job.seal(&mut store, 1).unwrap();
        assert!(read(&mut cache, &store).unwrap().1.contains(&payload.content_hash));
        job.release(CasReleaseOutcome::Committed).unwrap();
        assert!(!read(&mut cache, &store).unwrap().1.contains(&payload.content_hash));

        store.device_store().unwrap().connection().execute(
            "INSERT INTO external_lww_segments VALUES('synthetic','synthetic','1','1',?1,X'',0)",
            [serde_json::json!({"assets":[{"contentHash":payload.content_hash}],"reusedAssets":[]}).to_string()],
        ).unwrap();
        let (referenced, local) = read(&mut cache, &store).unwrap();
        assert!(referenced.contains(&payload.content_hash));
        assert!(!local.contains(&payload.content_hash));
        let before = (cache.data_version, cache.changes, cache.device_version, cache.device_changes);
        let mut masked = DurableCasJob::begin(directory.path(), "synthetic-masked-pin", CasJobKind::DirectAssetOrInlayWrite, CasJobOwner::for_test(), 1).unwrap();
        masked.pin_existing(&cas, &payload.content_hash, payload.byte_size, CasObjectRole::DirectObject).unwrap();
        masked.seal(&mut store, 1).unwrap();
        assert!(read(&mut cache, &store).unwrap().1.contains(&payload.content_hash));
        assert_eq!((cache.data_version, cache.changes, cache.device_version, cache.device_changes), before, "the already registered body changes no database invalidation token");
        masked.release(CasReleaseOutcome::Committed).unwrap();
        assert!(!read(&mut cache, &store).unwrap().1.contains(&payload.content_hash));
        store.device_store().unwrap().connection().execute("DELETE FROM external_lww_segments", []).unwrap();

        store.connection.execute("INSERT INTO plugin_storage (generation,owner,storage_key,byte_size,ordinal,value) VALUES ('synthetic','synthetic','body',0,0,?1)",
            [serde_json::json!({"object":payload.content_hash}).to_string()]).unwrap();
        assert!(read(&mut cache, &store).unwrap().0.contains(&payload.content_hash));
        let other = rusqlite::Connection::open(&store.database_path).unwrap();
        other.execute("DELETE FROM plugin_storage WHERE generation='synthetic'", []).unwrap();
        assert!(!read(&mut cache, &store).unwrap().0.contains(&payload.content_hash));
    }
}
