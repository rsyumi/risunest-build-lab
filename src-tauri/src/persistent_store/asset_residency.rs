//! Physical per-device storage policy. Logical aliases and snapshot roots are
//! preserved when a verified server copy replaces a local payload file.
use super::{snapshot_archive, PersistentStore};
use crate::{
    asset_repository::{object_physical_key, PayloadCas},
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
struct Inventory {
    referenced: BTreeSet<String>,
    local: BTreeSet<String>,
    release_blocked: bool,
}
impl ResidencyStatus {
    pub(crate) fn has_remote_or_missing(&self) -> bool {
        self.remote_objects > 0 || self.unavailable_objects > 0
    }
    pub(crate) fn has_remote(&self) -> bool {
        self.remote_objects > 0
    }
}

impl PersistentStore {
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
            for hash in orphans {
                if cas.stat_object(hash)?.is_some() {
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
        let mut local = BTreeSet::new();
        let mut manifests = BTreeSet::new();
        let mut release_blocked = false;
        for (label, root) in roots {
            release_blocked |= root.retain_all_objects || !root.blockers.is_empty();
            if label == "external-conflict" {
                local.extend(root.object_hashes.iter().cloned());
                local.extend(root.manifest_hashes.iter().cloned());
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
        // External captures require their payloads, held locally or in custody,
        // through publication, even when the source alias was removed by a
        // later edit.
        local.extend(
            crate::external_storage::capture::registered_roots(
                &self.connection,
                &self.repository_root,
            )?
            .object_hashes,
        );
        let jobs = if guarded {
            crate::asset_repository::job_pins::collect_durable_cas_job_roots_already_guarded(
                &self.repository_root,
            )
        } else {
            crate::asset_repository::job_pins::collect_durable_cas_job_roots(&self.repository_root)
        };
        if !jobs.blockers.is_empty() {
            return Err(SyncError::new("asset-jobs-unresolved", 409));
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
        &self,
        residency: &Residency,
        remote: &mut RemoteBodies,
        connections: &mut Connections,
        hashes: &[String],
    ) -> Result<Vec<Holder>> {
        let mut server = Vec::with_capacity(hashes.len());
        let mut external = Vec::new();
        for hash in hashes {
            let object = residency.object(hash, None)?;
            if object.is_none() {
                external.push(hash.as_str());
            }
            server.push(object.map(|object| Holder::Server(object.size)));
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
        let residency = Residency::open(&self.repository_root)?;
        let inventory = self.residency_inventory(false)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let mut connections = Connections::default();
        let mut status = ResidencyStatus {
            policy: self.device_store()?.asset_residency_policy()?,
            local_bytes: 0,
            remote_bytes: 0,
            remote_objects: 0,
            server_bytes: 0,
            server_objects: 0,
            external_objects: Vec::new(),
            unavailable_objects: 0,
            previous_storage_objects: None,
            evicted_bytes: 0,
        };
        let mut external = std::collections::BTreeMap::<String, u64>::new();
        let mut missing = Vec::new();
        for hash in inventory.referenced {
            if let Some(size) = cas.stat_object(&hash)? {
                status.local_bytes += size;
                continue;
            }
            missing.push(hash);
        }
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        for page in missing.chunks(HOLDER_PAGE) {
            for holder in self.residency_holders(&residency, &mut remote, &mut connections, page)? {
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
    pub(crate) fn asset_residency_status_for_target(&self, target: Option<&PreviousStorageTarget>) -> Result<ResidencyStatus> {
        let mut status = self.asset_residency_status()?;
        if let Some(target) = target {
            status.previous_storage_objects = Some(self.previous_storage_assets(target, &|| Ok(()))?.len() as u64);
        }
        Ok(status)
    }
    fn previous_storage_assets(&self, target: &PreviousStorageTarget, check: &impl Fn() -> Result<()>) -> Result<BTreeSet<String>> {
        let inventory = self.residency_inventory(false)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        let residency = Residency::open(&self.repository_root)?;
        let mut missing = Vec::new();
        for hash in inventory.referenced {
            check()?;
            if cas.stat_object(&hash)?.is_none() { missing.push(hash); }
        }
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        let mut connections = Connections::default();
        let mut wanted = BTreeSet::new();
        for page in missing.chunks(HOLDER_PAGE) {
            check()?;
            let available = self.residency_holders(&residency, &mut remote, &mut connections, page)?;
            let mut external = if matches!(target, PreviousStorageTarget::External { .. }) {
                remote.holders(&page.iter().map(String::as_str).collect::<Vec<_>>())
                    .map_err(|_| std::io::Error::other("external-source-invalid"))?
            } else { Default::default() };
            for (hash, holder) in page.iter().zip(available) {
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
                if !held { wanted.insert(hash.clone()); }
            }
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
        let mut missing = Vec::new();
        for hash in inventory.referenced {
            check()?;
            if cas.stat_object(&hash)?.is_none() {
                missing.push(hash);
            }
        }
        let mut remote = RemoteBodies::deferred(&self.repository_root);
        for page in missing.chunks(HOLDER_PAGE) {
            check()?;
            let holders = self.residency_holders(&residency, &mut remote, &mut connections, page)?;
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
        if let Some(character_id) = selected_character_id {
            let selected = self.selected_character_asset_hashes(character_id)?.into_iter()
                .filter(|hash| targets.contains(hash)).collect::<Vec<_>>();
            if !hydration.hydrate_many(&selected, &check)?.is_empty() {
                return Err(SyncError::new("required-asset-unavailable", 409));
            }
        }
        if !hydration.hydrate_many(&targets.into_iter().collect::<Vec<_>>(), &check)?.is_empty() {
            return Err(SyncError::new("required-asset-unavailable", 409));
        }
        let status = self.asset_residency_status()?;
        check()?;
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
        let inventory = self.residency_inventory(false)?;
        let candidates = inventory
            .referenced
            .difference(&inventory.local)
            .cloned()
            .collect::<Vec<_>>();
        let cas = PayloadCas::new(&self.repository_root)?;
        let mut evicted = 0;
        let mut retained = std::collections::BTreeMap::new();
        let directory = tempfile::Builder::new().prefix("asset-offload-").tempdir_in(&self.repository_root)?;
        let cache = crate::server_sync::cache::Cache::open(directory.path())?.with_library(&self.repository_root)?;
        for page in candidates.chunks(128) {
            check()?;
            let mut objects = std::collections::BTreeMap::new();
            for hash in page {
                if let Some(size) = cas.stat_object(hash)? {
                    objects.insert(hash.clone(), size);
                }
            }
            if objects.is_empty() {
                continue;
            }
            let identities = objects
                .iter()
                .map(|(hash, size)| serde_json::json!({"hash":hash,"size":size.to_string()}))
                .collect::<Vec<_>>();
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Missing {
                missing: Vec<String>,
            }
            let (_, missing): (_, Missing) = client.json(
                reqwest::Method::POST,
                "objects/missing",
                &[],
                Some(&identities),
                &[],
            )?;
            for hash in missing.missing {
                check()?;
                let size = *objects
                    .get(&hash)
                    .ok_or_else(|| SyncError::new("invalid-missing-response", 502))?;
                // A snapshot-only object may never have been published.
                if cache.stat_object(&hash)? != Some(size) {
                    return Err(SyncError::new("asset-changed", 409));
                }
                cache.verify(&hash, &check)?;
                crate::server_sync::transfer::Transfer::new(&client, &cache)?
                    .with_check(&check)
                    .upload_with_hints(&[hash], &[], false, &Default::default())?;
            }
            check()?;
            residency.retain(
                &client,
                &config,
                &head,
                &objects
                    .iter()
                    .map(|(hash, size)| (hash.clone(), Some(*size)))
                    .collect::<Vec<_>>(),
            )?;
            retained.extend(objects);
        }
        if !retained.is_empty() {
            check()?;
            let _guard = crate::asset_repository::coordinator::lock_repository_mutation()?;
            let fresh = self.residency_inventory(true)?;
            let sync_cache_root =
                self.repository_root
                    .join("server-sync")
                    .join(risunest_sync_wire::hash(
                        format!("{}:{}", config.library_id, config.device_id).as_bytes(),
                    ));
            for (hash, size) in retained {
                check()?;
                if fresh.local.contains(&hash) {
                    continue;
                }
                // Keep aliases and their object catalog row. Offloading is
                // neither a logical deletion nor an asset GC tombstone.
                if let crate::asset_repository::ExactObjectUnlink::Removed { .. } =
                    cas.unlink_exact_object(&hash, size, &object_physical_key(&hash))?
                {
                    evicted += size;
                }
                if sync_cache_root.is_dir() {
                    PayloadCas::new(&sync_cache_root)?.unlink_exact_object(
                        &hash,
                        size,
                        &object_physical_key(&hash),
                    )?;
                }
            }
        }
        self.asset_residency_release_unused_cancelled(cancellation, &check)?;
        let mut status = self.asset_residency_status()?;
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
        let mut residency = Residency::open(&self.repository_root)?;
        let cas = PayloadCas::new(&self.repository_root)?;
        // Custody the library still uses is kept without asking the server.
        // What remains is decided again under the mutation lock below.
        let revision = self.revision()?;
        let used = self.residency_inventory(false)?;
        if used.release_blocked {
            return Ok(());
        }
        let mut after = String::new();
        loop {
            let page = residency.page(&after)?;
            if page.is_empty() {
                break;
            }
            let mut candidates = std::collections::BTreeMap::<String, Vec<String>>::new();
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
                    let inventory = self.residency_inventory(true)?;
                    if inventory.release_blocked {
                        return Ok(());
                    }
                    let mut objects = Vec::new();
                    for hash in hashes {
                        if inventory.referenced.contains(&hash) || inventory.local.contains(&hash) {
                            continue;
                        }
                        let Some(object) = residency.begin_latest_release(&hash, &context)? else {
                            continue;
                        };
                        if cas.stat_object(&hash)?.is_none() {
                            self.connection.execute(
                                "DELETE FROM asset_objects WHERE object_hash=?1",
                                [&hash],
                            )?;
                        }
                        objects.push(object);
                    }
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
