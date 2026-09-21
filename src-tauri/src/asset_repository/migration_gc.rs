use super::owner_manifest_codec::{decode_owner_manifest, owner_manifest_identity};
use super::payload_cas::{PayloadCas, PayloadCasReadScan};
use crate::trust_boundary::is_lower_hex_256;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{self, ErrorKind},
};

fn validate_hash(hash: &str, context: &str) -> io::Result<()> {
    if !is_lower_hex_256(hash) {
        return invalid_data(format!("{context} is invalid"));
    }
    Ok(())
}

fn invalid_data<T>(message: impl Into<String>) -> io::Result<T> {
    Err(io::Error::new(ErrorKind::InvalidData, message.into()))
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AssetRootSet {
    pub manifest_hashes: BTreeSet<String>,
    pub object_hashes: BTreeSet<String>,
    pub legacy_asset_keys: BTreeSet<String>,
    pub inlay_ids: BTreeSet<String>,
    pub cold_keys: BTreeSet<String>,
    pub blockers: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub retain_all_objects: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn validate_root_set(roots: &AssetRootSet) -> io::Result<()> {
    for hash in roots.manifest_hashes.iter().chain(&roots.object_hashes) {
        validate_hash(hash, "asset root hash")?;
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetGcCandidate {
    pub object_hash: String,
    pub byte_size: u64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetGcDryRunReport {
    pub marked_hashes: Vec<String>,
    pub grace_retained_hashes: Vec<String>,
    pub potential_delete_hashes: Vec<String>,
    pub potential_delete_bytes: u64,
    pub deleted_hashes: Vec<String>,
    pub deleted_bytes: u64,
    pub blockers: Vec<String>,
    pub deletion_enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AssetGcDeleteHookPoint {
    AfterInitialScan,
    AfterTombstone,
    AfterUnlink,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssetGcDryRunPage {
    pub(crate) report: AssetGcDryRunReport,
    pub(crate) next_cursor: Option<String>,
}

pub fn dry_run_mark_and_sweep(
    cas: &PayloadCas,
    candidates: impl IntoIterator<Item = AssetGcCandidate>,
    roots: impl IntoIterator<Item = AssetRootSet>,
    now_ms: i64,
    minimum_grace_ms: i64,
) -> io::Result<AssetGcDryRunReport> {
    dry_run_mark_and_sweep_with_remote(cas, candidates, roots, now_ms, minimum_grace_ms, |_| {
        Ok(None)
    })
}
pub(crate) fn dry_run_mark_and_sweep_with_remote(
    cas: &PayloadCas,
    candidates: impl IntoIterator<Item = AssetGcCandidate>,
    roots: impl IntoIterator<Item = AssetRootSet>,
    now_ms: i64,
    minimum_grace_ms: i64,
    remote: impl Fn(&str) -> io::Result<Option<u64>>,
) -> io::Result<AssetGcDryRunReport> {
    if now_ms < 0 || minimum_grace_ms < 0 {
        return invalid_data("asset GC timestamps must be nonnegative");
    }
    let marks = mark_asset_roots_with_remote(cas, roots, &remote)?;
    sweep_asset_candidates_with_remote(
        cas,
        candidates,
        &marks,
        now_ms,
        minimum_grace_ms,
        remote,
    )
}

pub(crate) struct AssetGcMarks {
    marked_hashes: BTreeSet<String>,
    blockers: BTreeSet<String>,
    retain_all_objects: bool,
}

pub(crate) fn mark_asset_roots_with_remote(
    cas: &PayloadCas,
    roots: impl IntoIterator<Item = AssetRootSet>,
    remote: impl Fn(&str) -> io::Result<Option<u64>>,
) -> io::Result<AssetGcMarks> {
    let marks = collect_asset_root_marks(cas, roots)?;
    for hash in &marks.marked_hashes {
        if cas.stat_object(hash)?.is_none() && remote(hash)?.is_none() {
            return invalid_data("marked CAS object is missing");
        }
    }
    Ok(marks)
}

pub(crate) fn mark_asset_roots_with_remote_scan(
    scan: &PayloadCasReadScan,
    roots: impl IntoIterator<Item = AssetRootSet>,
    remote: impl Fn(&str) -> io::Result<Option<u64>>,
) -> io::Result<AssetGcMarks> {
    let marks = collect_asset_root_marks_scan(scan, roots)?;
    let mut hashes = marks.marked_hashes.iter().map(String::as_str);
    loop {
        let batch = hashes.by_ref().take(128).collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }
        let local_sizes = scan.stat_objects(batch.iter().copied())?;
        for (hash, local_size) in batch.into_iter().zip(local_sizes) {
            if local_size.is_none() && remote(hash)?.is_none() {
                return invalid_data("marked CAS object is missing");
            }
        }
    }
    Ok(marks)
}

fn collect_asset_root_marks(
    cas: &PayloadCas,
    roots: impl IntoIterator<Item = AssetRootSet>,
) -> io::Result<AssetGcMarks> {
    collect_asset_root_marks_with(roots, |hash| cas.read_object(hash))
}

fn collect_asset_root_marks_scan(
    scan: &PayloadCasReadScan,
    roots: impl IntoIterator<Item = AssetRootSet>,
) -> io::Result<AssetGcMarks> {
    collect_asset_root_marks_with(roots, |hash| scan.read_object(hash))
}

fn collect_asset_root_marks_with(
    roots: impl IntoIterator<Item = AssetRootSet>,
    read_object: impl Fn(&str) -> io::Result<Option<Vec<u8>>>,
) -> io::Result<AssetGcMarks> {
    let mut manifest_hashes = BTreeSet::new();
    let mut marked_hashes = BTreeSet::new();
    let mut blockers = BTreeSet::new();
    let mut retain_all_objects = false;
    for roots in roots {
        validate_root_set(&roots)?;
        retain_all_objects |= roots.retain_all_objects
            || roots.blockers.contains("plugin-storage-opaque")
            || roots.blockers.contains("cold-payload-unscanned");
        manifest_hashes.extend(roots.manifest_hashes);
        marked_hashes.extend(roots.object_hashes);
        blockers.extend(roots.blockers);
        if !roots.legacy_asset_keys.is_empty() {
            blockers.insert("legacy-asset-roots-unresolved".to_owned());
        }
        if !roots.inlay_ids.is_empty() {
            blockers.insert("inlay-roots-unresolved".to_owned());
        }
        if !roots.cold_keys.is_empty() {
            blockers.insert("cold-payload-unscanned".to_owned());
        }
    }
    for manifest_hash in manifest_hashes {
        let canonical = read_object(&manifest_hash)?
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "root manifest is missing"))?;
        if owner_manifest_identity(&canonical) != manifest_hash {
            return invalid_data("root manifest content hash mismatch");
        }
        let entries = decode_owner_manifest(&canonical)
            .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
        marked_hashes.insert(manifest_hash);
        for entry in entries {
            if let Some(hash) = entry.payload_hash {
                marked_hashes.insert(hex::encode(hash));
            }
        }
    }
    Ok(AssetGcMarks {
        marked_hashes,
        blockers,
        retain_all_objects,
    })
}

pub(crate) fn sweep_asset_candidates_with_remote_scan(
    scan: &PayloadCasReadScan,
    candidates: impl IntoIterator<Item = AssetGcCandidate>,
    marks: &AssetGcMarks,
    now_ms: i64,
    minimum_grace_ms: i64,
    remote: impl Fn(&str) -> io::Result<Option<u64>>,
) -> io::Result<AssetGcDryRunReport> {
    let candidates = candidates.into_iter().collect::<Vec<_>>();
    let local_sizes = scan.stat_objects(
        candidates
            .iter()
            .map(|candidate| candidate.object_hash.as_str()),
    )?;
    let mut local_sizes = local_sizes.into_iter();
    sweep_asset_candidates_with_local(
        candidates,
        marks,
        now_ms,
        minimum_grace_ms,
        |_| {
            local_sizes.next().ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidData,
                    "asset GC local size scan ended before its candidate page",
                )
            })
        },
        remote,
    )
}

fn sweep_asset_candidates_with_local(
    candidates: impl IntoIterator<Item = AssetGcCandidate>,
    marks: &AssetGcMarks,
    now_ms: i64,
    minimum_grace_ms: i64,
    mut local: impl FnMut(&str) -> io::Result<Option<u64>>,
    remote: impl Fn(&str) -> io::Result<Option<u64>>,
) -> io::Result<AssetGcDryRunReport> {
    if now_ms < 0 || minimum_grace_ms < 0 {
        return invalid_data("asset GC timestamps must be nonnegative");
    }
    let cutoff = now_ms.saturating_sub(minimum_grace_ms);
    let mut marked_hashes = marks.marked_hashes.clone();

    let mut seen_candidates = BTreeSet::new();
    let mut grace_retained_hashes = Vec::new();
    let mut potential_delete_hashes = Vec::new();
    let mut potential_delete_bytes = 0_u64;
    for candidate in candidates {
        validate_hash(&candidate.object_hash, "asset GC candidate hash")?;
        if candidate.created_at_ms < 0 || !seen_candidates.insert(candidate.object_hash.clone()) {
            return invalid_data("asset GC candidate is invalid or duplicated");
        }
        let physical = local(&candidate.object_hash)?;
        let actual_size = physical.or(remote(&candidate.object_hash)?).ok_or_else(|| {
            io::Error::new(ErrorKind::InvalidData, "asset GC candidate is missing")
        })?;
        if actual_size != candidate.byte_size {
            return invalid_data("asset GC candidate size mismatch");
        }
        // Server custody cleanup owns removal of remote-only catalog entries.
        // Physical GC must not claim disk space was freed for these entries.
        if physical.is_none() {
            continue;
        }
        if marks.retain_all_objects {
            marked_hashes.insert(candidate.object_hash);
            continue;
        }
        if marked_hashes.contains(&candidate.object_hash) {
            continue;
        }
        if candidate.created_at_ms > cutoff {
            grace_retained_hashes.push(candidate.object_hash);
        } else {
            potential_delete_bytes = potential_delete_bytes
                .checked_add(candidate.byte_size)
                .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "GC byte count overflow"))?;
            potential_delete_hashes.push(candidate.object_hash);
        }
    }
    grace_retained_hashes.sort();
    potential_delete_hashes.sort();
    Ok(AssetGcDryRunReport {
        marked_hashes: marked_hashes.into_iter().collect(),
        grace_retained_hashes,
        potential_delete_hashes,
        potential_delete_bytes,
        deleted_hashes: Vec::new(),
        deleted_bytes: 0,
        blockers: marks.blockers.iter().cloned().collect(),
        deletion_enabled: false,
    })
}

pub(crate) fn sweep_asset_candidates_with_remote(
    cas: &PayloadCas,
    candidates: impl IntoIterator<Item = AssetGcCandidate>,
    marks: &AssetGcMarks,
    now_ms: i64,
    minimum_grace_ms: i64,
    remote: impl Fn(&str) -> io::Result<Option<u64>>,
) -> io::Result<AssetGcDryRunReport> {
    sweep_asset_candidates_with_local(
        candidates,
        marks,
        now_ms,
        minimum_grace_ms,
        |hash| cas.stat_object(hash),
        remote,
    )
}

#[cfg(test)]
mod tests {
    use super::{dry_run_mark_and_sweep, AssetGcCandidate, AssetRootSet};
    use crate::asset_repository::owner_manifest_codec::{
        encode_owner_manifest, OwnerManifestEntry,
    };
    use crate::asset_repository::payload_cas::PayloadCas;

    #[test]
    fn opaque_snapshot_blockers_retain_every_catalog_candidate() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open payload CAS");
        let candidate = cas.prepare_bytes(b"opaque-snapshot-candidate").unwrap();
        let mut roots = AssetRootSet::default();
        roots.blockers.insert("plugin-storage-opaque".to_owned());
        roots.blockers.insert("cold-payload-unscanned".to_owned());
        let report = dry_run_mark_and_sweep(
            &cas,
            [AssetGcCandidate {
                object_hash: candidate.content_hash.clone(),
                byte_size: candidate.byte_size,
                created_at_ms: 0,
            }],
            [roots],
            100,
            10,
        )
        .unwrap();

        assert_eq!(report.marked_hashes, vec![candidate.content_hash]);
        assert!(report.potential_delete_hashes.is_empty());
        assert!(!report.deletion_enabled);
    }

    #[test]
    fn dry_run_marks_manifest_payloads_and_explicit_roots_without_deleting_files() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open payload CAS");
        let kept = cas.prepare_bytes(b"kept-by-manifest").unwrap();
        let staged = cas.prepare_bytes(b"kept-by-staging").unwrap();
        let collectable = cas.prepare_bytes(b"collectable").unwrap();
        let young = cas.prepare_bytes(b"young").unwrap();
        let manifest_bytes = encode_owner_manifest(&[OwnerManifestEntry {
            tuple: [
                "kept".to_owned(),
                "assets/kept.bin".to_owned(),
                "bin".to_owned(),
            ],
            payload_hash: Some(hex::decode(&kept.content_hash).unwrap().try_into().unwrap()),
        }])
        .unwrap();
        let manifest = cas.prepare_bytes(&manifest_bytes).unwrap();
        let mut roots = AssetRootSet::default();
        roots.manifest_hashes.insert(manifest.content_hash.clone());
        roots.object_hashes.insert(staged.content_hash.clone());

        let report = dry_run_mark_and_sweep(
            &cas,
            [
                candidate(&kept, 0),
                candidate(&staged, 0),
                candidate(&collectable, 0),
                candidate(&young, 95),
                candidate(&manifest, 0),
            ],
            std::iter::once(roots),
            100,
            10,
        )
        .unwrap();

        assert_eq!(
            report.marked_hashes,
            sorted([
                kept.content_hash.clone(),
                staged.content_hash.clone(),
                manifest.content_hash.clone(),
            ])
        );
        assert_eq!(
            report.grace_retained_hashes,
            vec![young.content_hash.clone()]
        );
        assert_eq!(
            report.potential_delete_hashes,
            vec![collectable.content_hash.clone()]
        );
        assert_eq!(report.potential_delete_bytes, collectable.byte_size);
        assert!(report.blockers.is_empty());
        assert!(!report.deletion_enabled);
        assert!(cas
            .stat_object(&collectable.content_hash)
            .unwrap()
            .is_some());
    }

    #[test]
    fn dry_run_retains_candidates_for_conservative_plugin_and_cold_blockers() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open payload CAS");
        let candidate_payload = cas.prepare_bytes(b"unmarked").unwrap();
        let mut roots = AssetRootSet::default();
        roots.blockers.extend([
            "cold-payload-unscanned".to_owned(),
            "plugin-storage-opaque".to_owned(),
        ]);

        let report =
            dry_run_mark_and_sweep(&cas, [candidate(&candidate_payload, 0)], [roots], 100, 10)
                .unwrap();

        assert_eq!(report.marked_hashes, vec![candidate_payload.content_hash]);
        assert!(report.potential_delete_hashes.is_empty());
        assert_eq!(report.potential_delete_bytes, 0);
        assert_eq!(
            report.blockers,
            vec![
                "cold-payload-unscanned".to_owned(),
                "plugin-storage-opaque".to_owned(),
            ]
        );
        assert!(!report.deletion_enabled);
    }

    #[test]
    fn dry_run_fails_closed_when_a_root_manifest_is_corrupt() {
        let directory = tempfile::tempdir().expect("temporary repository");
        let cas = PayloadCas::new(directory.path()).expect("open payload CAS");
        let manifest = cas
            .prepare_bytes(&encode_owner_manifest(&[]).unwrap())
            .unwrap();
        std::fs::write(directory.path().join(&manifest.physical_key), b"corrupt").unwrap();
        let mut roots = AssetRootSet::default();
        roots.manifest_hashes.insert(manifest.content_hash.clone());

        let error = dry_run_mark_and_sweep(&cas, [], [roots], 100, 10)
            .expect_err("corrupt root manifest must abort the mark pass");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    fn candidate(
        prepared: &crate::asset_repository::payload_cas::PreparedPayload,
        created_at_ms: i64,
    ) -> AssetGcCandidate {
        AssetGcCandidate {
            object_hash: prepared.content_hash.clone(),
            byte_size: prepared.byte_size,
            created_at_ms,
        }
    }

    fn sorted<const N: usize>(values: [String; N]) -> Vec<String> {
        let mut values = values.into_iter().collect::<Vec<_>>();
        values.sort();
        values
    }
}
