//! User-requested discovery of authenticated history roots, including orphan snapshots.
use super::{
    connection_commands::ConnectedRepository, connection_store::ConnectionStore, contract::*,
    control, gc_store::locator_key, packaging::{self, RemoteObject}, runtime, transfer::SpoolSink,
};
use crate::native_log::logged;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use risunest_external_storage_format::{
    content_identity::hash, crypto::derive_key, snapshot as wire,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    sync::{LazyLock, Mutex},
};
use tauri::{AppHandle, Manager};

const PAGE: u16 = 30;
const MAX_CIPHERTEXT: u64 = 16 * 1024 * 1024;
/// Snapshot pages read without downloading, beyond those the backup points'
/// own snapshots can fill, while looking for one a backup point does not show.
const READ_AHEAD_PAGES: usize = 4;
/// Snapshot objects the backup points of each connection's current listing
/// reference, so its snapshot pages leave them out.
static COVERED: LazyLock<Mutex<BTreeMap<String, BTreeSet<String>>>> =
    LazyLock::new(Default::default);
fn corrupt() -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CursorState {
    connection: String,
    phase: String,
    provider: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HistoryRequest {
    connection_id: String,
    cursor: Option<String>,
}
fn decode_cursor(request: &HistoryRequest) -> Result<CursorState> {
    let Some(value) = &request.cursor else {
        return Ok(CursorState {
            connection: request.connection_id.clone(),
            phase: "points".into(),
            provider: None,
        });
    };
    if value.len() > 8192 {
        return Err(corrupt());
    }
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| corrupt())?;
    let state: CursorState = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
    if state.connection != request.connection_id
        || !["points", "snapshots"].contains(&state.phase.as_str())
        || state
            .provider
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 4096)
    {
        return Err(corrupt());
    }
    Ok(state)
}
fn covered_snapshots(
    connection: &str,
    restart: bool,
    referenced: impl IntoIterator<Item = String>,
) -> BTreeSet<String> {
    let mut covered = COVERED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = covered.entry(connection.to_owned()).or_default();
    if restart {
        entry.clear();
    }
    entry.extend(referenced);
    entry.clone()
}
/// Where the next snapshot page holding an object no backup point references
/// starts. `Some(None)` is the first page and `None` means none remain.
async fn next_uncovered_page(
    provider: &dyn Provider,
    handle: &RepositoryHandle,
    covered: &BTreeSet<String>,
    mut cursor: Option<String>,
    cancel: &Cancellation,
) -> Result<Option<Option<String>>> {
    for _ in 0..covered.len().div_ceil(usize::from(PAGE)) + READ_AHEAD_PAGES {
        let page = provider
            .list_objects(handle, Collection::Snapshots, cursor.as_deref(), PAGE, cancel)
            .await?;
        for object in &page.objects {
            if !covered.contains(&locator_key(&object.locator)?) {
                return Ok(Some(cursor));
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(None),
        }
    }
    Ok(Some(cursor))
}
fn encode_cursor(state: CursorState) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&state).map_err(runtime::local_error)?))
}
async fn open_snapshot(
    app: &AppHandle,
    connected: &ConnectedRepository,
    receipt: ObjectReceipt,
    cancel: &Cancellation,
) -> Result<(RemoteObject, control::SnapshotView)> {
    receipt.locator.validate_for(&connected.handle)?;
    if !receipt.complete || receipt.byte_length == 0 || receipt.byte_length > MAX_CIPHERTEXT {
        return Err(corrupt());
    }
    let directory = super::leftovers::managed_scratch(&runtime::root(app)?, "history-")?;
    let path = directory.path().join("snapshot");
    let mut sink = SpoolSink::create(&path, receipt.byte_length)?;
    if !matches!(
        connected
            .provider
            .read_object(&connected.handle, &receipt.locator, None, &mut sink, cancel)
            .await?,
        ReadReceipt::Body(_)
    ) || !sink.is_verified()
    {
        return Err(corrupt());
    }
    let bytes = std::fs::read(&path).map_err(runtime::local_error)?;
    if bytes.len() as u64 != receipt.byte_length {
        return Err(corrupt());
    }
    let key = derive_key(
        &connected.root_key,
        &connected.stored.descriptor.repository_id,
        "metadata",
    )
    .map_err(|_| corrupt())?;
    let mut plaintext = Vec::new();
    let header = wire::open_envelope(
        &mut Cursor::new(&bytes),
        &mut plaintext,
        &key,
        wire::MAX_METADATA_BYTES as u64,
    )
    .map_err(|_| corrupt())?;
    if header.repository_id != connected.stored.descriptor.repository_id {
        return Err(corrupt());
    }
    let role = packaging::native_role(header.role).map_err(|_| corrupt())?;
    let document = control::SnapshotView::read(
        &plaintext,
        header.role,
        &connected.stored.descriptor.repository_id,
    )
    .map_err(|_| corrupt())?;
    if header.object_id != format!("snapshot-{}", document.snapshot_id) {
        return Err(corrupt());
    }
    Ok((
        RemoteObject {
            repository_id: header.repository_id,
            object_id: header.object_id,
            role,
            receipt,
            ciphertext_sha256: hex::encode(hash(&bytes)),
            plaintext_length: header.plaintext_length,
            plaintext_sha256: hex::encode(hash(&plaintext)),
        },
        document,
    ))
}
/// `includedSections` is what a restore may choose from, and `sameDevice` says
/// whether the values are this device's own. Both come from the authenticated
/// document, so the screen never guesses the coverage from the connection.
fn item(
    document: &control::SnapshotView,
    reference: &RemoteObject,
    kind: &str,
    pinned: bool,
    store_id: &str,
) -> Value {
    let same_device =
        !store_id.is_empty() && document.captured_by_device.as_deref() == Some(store_id);
    json!({"id":document.snapshot_id,"snapshotId":document.snapshot_id,"kind":kind,"createdAtMs":document.created_at_ms.to_string(),"logicalRevision":document.revision,"storedBytes":reference.receipt.byte_length.to_string(),"pinned":pinned,"complete":true,"verified":true,
        "includedSections":document.sections.keys().collect::<Vec<_>>(),"sameDevice":same_device})
}
fn remember_item(items: &mut BTreeMap<String, Value>, id: String, mut next: Value) {
    if let Some(previous) = items.get(&id) {
        next["pinned"] = json!(previous["pinned"] == true || next["pinned"] == true);
        let rank = |value: &Value| match value.as_str() {
            Some("backup-point") => 1,
            _ => 0,
        };
        if rank(&previous["kind"]) > rank(&next["kind"]) {
            next["kind"] = previous["kind"].clone();
        }
    }
    items.insert(id, next);
}

#[tauri::command]
pub(crate) async fn external_storage_list_history(
    app: AppHandle,
    request: HistoryRequest,
) -> Result<Value> {
    logged("external_storage_list_history", async move {
        let state = decode_cursor(&request)?;
        let connected =
            super::connection_commands::open_connected(&app, &request.connection_id).await?;
        let cancel = Cancellation::default();
        let cache = ConnectionStore::open(&runtime::root(&app)?)?;
        let store_id = crate::persistent_store::commands::with_store_mut(app.state(), |store| {
            store.external_identity()
        })
        .map_err(runtime::local_error)?
        .store_id;
        let mut items = BTreeMap::new();
        let snapshots_from = |provider: Option<String>| CursorState {
            connection: request.connection_id.clone(),
            phase: "snapshots".into(),
            provider,
        };
        let next = if state.phase == "points" {
            let page = control::list_connected_backup_points_page(
                &connected,
                state.provider.as_deref(),
                PAGE,
                &cancel,
            )
            .await?;
            let mut referenced = Vec::new();
            for point in page.points {
                let kind = match point.document.kind {
                    control::BackupPointKind::RecoveryCandidate => "recovery-candidate",
                    _ => "backup-point",
                };
                let pinned = point.document.kind == control::BackupPointKind::Manual;
                for reference in point.document.bundles().into_iter().cloned() {
                    referenced.push(locator_key(&reference.receipt.locator)?);
                    let document =
                        control::read_snapshot_document(&connected, &reference, &cancel).await?;
                    cache.remember_discovery(
                        &request.connection_id,
                        &document.snapshot_id,
                        &reference,
                    )?;
                    let mut value = item(&document, &reference, kind, pinned, &store_id);
                    let row_id = point.document.point_id.clone();
                    value["id"] = json!(row_id);
                    value["pointId"] = json!(point.document.point_id.clone());
                    value["pointObservation"] = json!(serde_json::to_string(
                        &point.reference.stored(&connected.handle)?,
                    ).map_err(runtime::local_error)?);
                    value["deletable"] = json!(matches!(
                        point.document.kind,
                        control::BackupPointKind::Automatic | control::BackupPointKind::Manual
                    ));
                    items.insert(row_id, value);
                }
            }
            let covered = covered_snapshots(&request.connection_id, state.provider.is_none(), referenced);
            match page.next_cursor {
                Some(provider) => Some(CursorState {
                    connection: request.connection_id.clone(),
                    phase: "points".into(),
                    provider: Some(provider),
                }),
                None => next_uncovered_page(
                    connected.provider.as_ref(), &connected.handle, &covered, None, &cancel,
                ).await?.map(snapshots_from),
            }
        } else {
            let covered = covered_snapshots(&request.connection_id, false, []);
            let page = connected
                .provider
                .list_objects(
                    &connected.handle,
                    Collection::Snapshots,
                    state.provider.as_deref(),
                    PAGE,
                    &cancel,
                )
                .await?;
            for receipt in page.objects {
                if covered.contains(&locator_key(&receipt.locator)?) {
                    continue;
                }
                let (reference, document) = open_snapshot(&app, &connected, receipt, &cancel).await?;
                cache.remember_discovery(&request.connection_id, &document.snapshot_id, &reference)?;
                remember_item(
                    &mut items,
                    document.snapshot_id.clone(),
                    item(&document, &reference, "recovery-candidate", false, &store_id),
                );
            }
            match page.next_cursor {
                Some(provider) => next_uncovered_page(
                    connected.provider.as_ref(), &connected.handle, &covered, Some(provider), &cancel,
                ).await?.map(snapshots_from),
                None => None,
            }
        };
        let mut output = json!({"items":items.into_values().collect::<Vec<_>>()});
        if let Some(next) = next {
            output["nextCursor"] = json!(encode_cursor(next)?);
        }
        Ok(output)
    }.await)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn older_history_starts_at_the_first_snapshot_page_no_backup_point_covers() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let provider = super::super::fake::FakeProvider::new(false);
            let handle = super::super::fake::repository();
            let cancel = Cancellation::default();
            let names: Vec<String> = (0..185).map(|index| format!("snapshot-{index:03}")).collect();
            for name in &names {
                provider.seed(name, ObjectRole::BackupBundle, vec![1; 8]);
            }
            let key = |name: &str| locator_key(&RemoteLocator {
                connection_identity: handle.connection_identity.clone(),
                collection: None,
                object: name.into(),
            }).unwrap();
            let all: BTreeSet<String> = names.iter().map(|name| key(name)).collect();
            assert_eq!(next_uncovered_page(&provider, &handle, &all, None, &cancel).await.unwrap(), None);

            let mut covered = all.clone();
            covered.remove(&key("snapshot-003"));
            assert_eq!(next_uncovered_page(&provider, &handle, &covered, None, &cancel).await.unwrap(), Some(None));

            let mut covered = all.clone();
            covered.remove(&key("snapshot-184"));
            let start = next_uncovered_page(&provider, &handle, &covered, None, &cancel).await.unwrap()
                .expect("an uncovered snapshot remains").expect("past the first page");
            let page = provider.list_objects(&handle, Collection::Snapshots, Some(&start), PAGE, &cancel).await.unwrap();
            assert!(page.objects.iter().any(|object| object.locator.object == "snapshot-184"));
        });
    }

    #[test]
    fn a_new_history_listing_forgets_the_points_an_earlier_one_saw() {
        assert_eq!(covered_snapshots("history-test", true, ["a".to_owned()]).len(), 1);
        assert_eq!(covered_snapshots("history-test", false, ["b".to_owned()]).len(), 2);
        assert_eq!(covered_snapshots("other-history-test", false, []).len(), 0);
        let fresh = covered_snapshots("history-test", true, ["c".to_owned()]);
        assert_eq!(fresh.into_iter().collect::<Vec<_>>(), vec!["c".to_owned()]);
    }

    #[test]
    fn duplicate_snapshot_keeps_pinning_and_backup_point_evidence() {
        let mut items = BTreeMap::new();
        remember_item(
            &mut items,
            "snapshot".into(),
            json!({"kind":"backup-point","pinned":true}),
        );
        remember_item(
            &mut items,
            "snapshot".into(),
            json!({"kind":"recovery-candidate","pinned":false}),
        );
        assert_eq!(items["snapshot"]["kind"], "backup-point");
        assert_eq!(items["snapshot"]["pinned"], true);
    }
    #[test]
    fn cursor_cannot_cross_connections_or_select_arbitrary_collections() {
        let encoded = encode_cursor(CursorState {
            connection: "one".into(),
            phase: "snapshots".into(),
            provider: Some("opaque".into()),
        })
        .unwrap();
        assert!(decode_cursor(&HistoryRequest {
            connection_id: "two".into(),
            cursor: Some(encoded.clone())
        })
        .is_err());
        assert_eq!(
            decode_cursor(&HistoryRequest {
                connection_id: "one".into(),
                cursor: Some(encoded)
            })
            .unwrap()
            .provider
            .as_deref(),
            Some("opaque")
        );
        let encoded = encode_cursor(CursorState {
            connection: "one".into(),
            phase: "packs".into(),
            provider: None,
        })
        .unwrap();
        assert!(decode_cursor(&HistoryRequest {
            connection_id: "one".into(),
            cursor: Some(encoded)
        })
        .is_err());
    }
}
