//! Rebuilding what a publication may reuse, from the parent it selected.
//!
//! Nothing here is a new authority. Every row it writes restates an
//! authenticated catalog the provider already holds, read through the same
//! verification a restore performs. A device that has never published to this
//! repository, or one whose cache was lost, arrives with nothing admissible
//! and would repackage and re-upload content the parent already carries.
use super::{
    contract::{Cancellation, ErrorKind, Provider, RepositoryHandle, Result},
    package_cache::{CatalogRange, PackageCache},
    packaging::{
        catalog_fingerprint_of, corrupt, section_fingerprint, transient, CatalogRoot, EntryPlan,
        ParentGraph, RemoteObject,
    },
    snapshot_restore,
};
use risunest_external_storage_format::snapshot as wire;
use std::{collections::BTreeMap, path::Path};

/// Teach this cache what the selected parent names, for every root it does not
/// already hold. A root that cannot be read is left unknown: the publication
/// then asks the provider about what it wants to reuse, which is what a cache
/// miss already meant.
#[allow(clippy::too_many_arguments)]
pub(super) async fn hydrate_parent(
    parent: &ParentGraph,
    format_repository_id: &str,
    cache_root: &Path,
    root_key: &[u8; 32],
    cache: &mut PackageCache,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<()> {
    let mut wanted = Vec::new();
    for (label, root) in parent.roots() {
        cancel.check()?;
        let identity = super::reachability::object_identity(root, repository)?;
        if !cache.knows_graph(format_repository_id, repository, &identity)? {
            wanted.push((label.clone(), root.clone(), identity));
        }
    }
    if wanted.is_empty() {
        return Ok(());
    }
    let staging = tempfile::tempdir_in(cache_root).map_err(transient)?;
    for (label, root, identity) in wanted {
        cancel.check()?;
        let read = snapshot_restore::read_catalog(
            &root,
            label.kind(),
            root_key,
            staging.path(),
            provider,
            repository,
            cancel,
        )
        .await;
        let (entries, packs, nodes) = match read {
            Ok(read) => read,
            // An unreadable parent catalog costs this publication its reuse
            // and nothing else. The upload that follows reports a provider
            // that is genuinely unavailable.
            Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
            Err(_) => continue,
        };
        record_catalog(
            &label,
            &identity,
            format_repository_id,
            &entries,
            &packs,
            &nodes,
            &root,
            cache,
            repository,
        )?;
    }
    Ok(())
}

/// What a receive already read, kept for the publication that follows it.
/// Recording what the provider has just answered costs nothing; reading the
/// same catalog again to publish from it would.
pub(super) fn record_read_catalog(
    cache_root: &Path,
    label: &CatalogRoot,
    root: &RemoteObject,
    entries: &[snapshot_restore::CompleteEntry],
    packs: &BTreeMap<String, RemoteObject>,
    nodes: &[(RemoteObject, CatalogRange)],
    repository: &RepositoryHandle,
) -> Result<()> {
    let cache = PackageCache::open(cache_root)?;
    let identity = super::reachability::object_identity(root, repository)?;
    record_catalog(
        label,
        &identity,
        &root.repository_id,
        entries,
        packs,
        nodes,
        root,
        &cache,
        repository,
    )
}

/// One catalog's worth of rows, in the order that keeps a partial write safe:
/// nothing is admitted until the graph naming it exists, and the shortcut that
/// skips the catalog entirely is written last.
#[allow(clippy::too_many_arguments)]
fn record_catalog(
    label: &CatalogRoot,
    identity: &str,
    format_repository_id: &str,
    entries: &[snapshot_restore::CompleteEntry],
    packs: &BTreeMap<String, RemoteObject>,
    nodes: &[(RemoteObject, CatalogRange)],
    root: &RemoteObject,
    cache: &PackageCache,
    repository: &RepositoryHandle,
) -> Result<()> {
    let kind = label.kind();
    let mut plans = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut referenced = BTreeMap::new();
        for chunk in &entry.chunks {
            let pack = packs
                .get(&chunk.pack_id)
                .ok_or_else(|| corrupt("catalog entry misses pack"))?;
            referenced.insert(chunk.pack_id.clone(), pack.clone());
        }
        let plan = EntryPlan {
            kind: entry.kind,
            key: entry.key.clone(),
            content_sha256: hex::encode(entry.content_sha256),
            byte_length: entry.byte_length,
            chunks: entry.chunks.clone(),
            packs: referenced.into_values().collect(),
        };
        plan.validate(format_repository_id, repository)?;
        plans.push(plan);
    }
    cache.put_entries(format_repository_id, repository, kind, &plans)?;
    let objects = packs.values().chain(nodes.iter().map(|(object, _)| object));
    for object in objects.clone() {
        cache.put_object(repository, object)?;
    }
    let mut members = BTreeMap::new();
    for object in objects {
        members.insert(
            super::reachability::object_identity(object, repository)?,
            object.stored(repository)?,
        );
    }
    let members: Vec<(String, wire::StoredObject)> = members.into_iter().collect();
    // Depth-first order keeps the nodes of one level in the order that level
    // was published in, which is the order the next publication assigns to.
    let mut levels: BTreeMap<u16, Vec<CatalogRange>> = BTreeMap::new();
    for (_, range) in nodes {
        levels.entry(range.level).or_default().push(range.clone());
    }
    let shape: Vec<CatalogRange> = levels.into_values().flatten().collect();
    cache.record_graph(format_repository_id, repository, kind, identity, &members, &shape)?;
    cache.put_catalog(repository, kind, &rebuilt_fingerprint(label, &plans), root)
}

/// The catalog fingerprint the next publication will compute for the same
/// content. Its sources arrive in the order each catalog is built from: a
/// record catalog sorts by key alone, and a section catalog sorts its entry
/// kinds apart first.
pub(super) fn rebuilt_fingerprint(label: &CatalogRoot, plans: &[EntryPlan]) -> String {
    let mut ordered: Vec<&EntryPlan> = plans.iter().collect();
    match label {
        CatalogRoot::Records | CatalogRoot::Assets => {
            ordered.sort_by(|a, b| a.key.cmp(&b.key));
        }
        CatalogRoot::Section(_) => {
            ordered.sort_by(|a, b| (a.kind as u8, &a.key).cmp(&(b.kind as u8, &b.key)));
        }
    }
    let digest = catalog_fingerprint_of(
        label.kind(),
        ordered
            .into_iter()
            .map(|plan| (plan.key.as_str(), plan.byte_length, plan.content_sha256.as_str())),
    );
    match label {
        CatalogRoot::Section(id) => section_fingerprint(id, &digest),
        _ => digest,
    }
}
