mod common;
use common::*;
use risunest_sync_server::{
    connection::ConnectionOptions,
    http,
    store::{Device, DeviceCredential, Store},
    workload::Workload,
};
use risunest_sync_wire::{
    canonical,
    lww::{
        CancelOperationRequest, NewDeviceClaimReceipt, NewDeviceClaimRequest, NewDeviceClaimState,
        WriterBindingRequest,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const RESERVED: &str = "00000000-0000-4000-8000-000000000003";
const AUTHORIZATION: &str = "00000000-0000-4000-8000-000000000004";

fn actor(store: &Store, credential: &DeviceCredential) -> Device {
    store
        .authenticate(&credential.library_id, &credential.token)
        .unwrap()
}
fn claim(former_token: Option<&str>) -> NewDeviceClaimRequest {
    NewDeviceClaimRequest {
        writer_id: RESERVED.into(),
        authorization_id: AUTHORIZATION.into(),
        former_token: former_token.map(str::to_owned),
    }
}
fn metadata(root: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(root.path().join("metadata.sqlite")).unwrap()
}

#[test]
fn claim_lookup_is_registration_scoped_and_rechecks_revocation_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let former = store.add_device().unwrap();
    let candidate = store.add_device().unwrap();
    let unrelated = store.add_device().unwrap();
    let new = actor(&store, &candidate);
    assert_eq!(
        store.new_device_writer_claim(&new).unwrap(),
        NewDeviceClaimState {
            claim: None,
            used: false
        }
    );
    let request = claim(Some(&former.token));
    let receipt = store.claim_new_device_writer(&new, &request).unwrap();
    assert_eq!(
        store
            .new_device_writer_claim(&actor(&store, &unrelated))
            .unwrap(),
        NewDeviceClaimState {
            claim: None,
            used: false
        }
    );
    drop(store);

    let store = Store::open(root.path()).unwrap();
    let new = actor(&store, &candidate);
    let state = store.new_device_writer_claim(&new).unwrap();
    assert!(state.used);
    let saved = state.claim.unwrap();
    assert_eq!(saved.request_digest, request.digest().unwrap());
    assert_eq!(saved.receipt, receipt);
    let serialized = serde_json::to_string(&saved).unwrap();
    assert!(!serialized.contains(&former.token));
    assert!(!serialized.contains(&candidate.token));
    assert_eq!(
        store
            .new_device_writer_claim(&actor(&store, &unrelated))
            .unwrap(),
        NewDeviceClaimState {
            claim: None,
            used: false
        }
    );
    metadata(&root)
        .execute("UPDATE devices SET revoked=1 WHERE id=?1", [&new.id])
        .unwrap();
    assert_eq!(
        store.new_device_writer_claim(&new).unwrap_err().code,
        "unauthorized"
    );
}

#[test]
fn management_registration_can_inspect_then_claim_and_forward_original_stamps() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    store
        .configure_connection(ConnectionOptions {
            endpoint: Some("https://sync.example.com".into()),
            cloudflared: None,
            registry_url: None,
        })
        .unwrap();
    let uri = store
        .issue_named_registration(
            "Synthetic new device",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            None,
        )
        .unwrap();
    let registration = risunest_sync_connect::Registration::parse_uri(&uri).unwrap();
    let new = store
        .authenticate(&registration.library_id, &registration.token)
        .unwrap();
    let head = store.device_head(&new).unwrap();
    store.device_session(&new).unwrap();
    store.time_sample().unwrap();
    let pin = store.create_state_pin(&new).unwrap();
    store.state_page(&new, &pin.pin_id, None, 10).unwrap();
    let receipt = store.claim_new_device_writer(&new, &claim(None)).unwrap();
    assert_eq!(receipt.authorization_id, AUTHORIZATION);
    assert_eq!(receipt.writer_id, RESERVED);
    assert_eq!(receipt.device_id, registration.device_id);
    assert_eq!(receipt.library_id, head.library_id);
    assert_eq!(receipt.epoch, head.epoch);
    assert!(!receipt.former_credential_inactive);
    let changes = vec![inline("a", WRITER_A, 1, "a"), inline("b", WRITER_B, 2, "b")];
    let publication = request(&store, RESERVED, "forwarded", changes.clone());
    assert_eq!(
        store.push(&new, &publication).unwrap().accepted_keys.len(),
        2
    );
    let db = metadata(&root);
    for change in changes {
        let body: String = db
            .query_row(
                "SELECT body FROM units WHERE key=?1",
                [change.key.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<risunest_sync_wire::lww::UnitChange>(&body).unwrap(),
            change
        );
    }
    assert_eq!(
        store.claim_new_device_writer(&new, &claim(None)).unwrap(),
        receipt
    );
    let wrong = request(
        &store,
        WRITER_A,
        "wrong-publisher",
        vec![inline("c", WRITER_A, 3, "c")],
    );
    assert_eq!(
        store.push(&new, &wrong).unwrap_err().code,
        "writer-collision"
    );
    assert_eq!(store.head().unwrap().seq.as_str(), "2");
    let other = device(&store);
    let reused = request(
        &store,
        RESERVED,
        "other-device",
        vec![inline("c", WRITER_A, 3, "c")],
    );
    assert_eq!(
        store.push(&other, &reused).unwrap_err().code,
        "writer-collision"
    );
}

#[test]
fn published_rejected_and_cancelled_credentials_are_not_fresh() {
    for kind in ["accepted", "rejected", "cancelled"] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(root.path()).unwrap();
        let former = store.add_device().unwrap();
        let candidate = store.add_device().unwrap();
        let new = actor(&store, &candidate);
        let mut publication = request(&store, WRITER_A, kind, vec![inline("a", WRITER_A, 1, "a")]);
        match kind {
            "accepted" => {
                store.push(&new, &publication).unwrap();
            }
            "rejected" => {
                publication.changes.clear();
                assert_eq!(
                    store.push(&new, &publication).unwrap_err().code,
                    "empty-push"
                );
            }
            "cancelled" => {
                store
                    .cancel_operation(
                        &new,
                        kind,
                        &CancelOperationRequest {
                            body_digest: publication.digest().unwrap(),
                        },
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            store.new_device_writer_claim(&new).unwrap(),
            NewDeviceClaimState {
                claim: None,
                used: true
            },
            "{kind}"
        );
        assert_eq!(
            store
                .claim_new_device_writer(&new, &claim(Some(&former.token)))
                .unwrap_err()
                .code,
            "registration-used"
        );
        actor(&store, &former);
        assert_eq!(
            metadata(&root)
                .query_row("SELECT count(*) FROM device_writer_claims", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn reserved_writer_must_be_unknown_as_publisher_and_as_forwarded_issuer() {
    for publisher in [RESERVED, WRITER_A] {
        let root = tempfile::tempdir().unwrap();
        let store = Store::init(root.path()).unwrap();
        let existing = device(&store);
        let publication = request(
            &store,
            publisher,
            "existing",
            vec![inline("a", RESERVED, 1, "a")],
        );
        store.push(&existing, &publication).unwrap();
        let new = device(&store);
        assert_eq!(
            store
                .claim_new_device_writer(&new, &claim(None))
                .unwrap_err()
                .code,
            "writer-collision"
        );
        if publisher != RESERVED {
            assert_eq!(
                metadata(&root)
                    .query_row(
                        "SELECT count(*) FROM writers WHERE writer=?1",
                        [RESERVED],
                        |r| r.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
    }
}

#[test]
fn authorization_is_device_bound_and_exact_intent_cannot_be_altered() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let new = device(&store);
    let receipt = store.claim_new_device_writer(&new, &claim(None)).unwrap();
    for altered in [
        NewDeviceClaimRequest {
            writer_id: WRITER_B.into(),
            ..claim(None)
        },
        NewDeviceClaimRequest {
            authorization_id: "different-authorization".into(),
            ..claim(None)
        },
        claim(Some(&"b".repeat(64))),
    ] {
        assert_eq!(
            store
                .claim_new_device_writer(&new, &altered)
                .unwrap_err()
                .code,
            "registration-integrity"
        );
    }
    let other = device(&store);
    let reused = NewDeviceClaimRequest {
        writer_id: WRITER_B.into(),
        ..claim(None)
    };
    assert_eq!(
        store
            .claim_new_device_writer(&other, &reused)
            .unwrap_err()
            .code,
        "registration-integrity"
    );
    assert_eq!(
        store.claim_new_device_writer(&new, &claim(None)).unwrap(),
        receipt
    );
}

#[test]
fn lost_claim_response_survives_restart_and_preserves_prior_operation_proofs() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let former = store.add_device().unwrap();
    let old = actor(&store, &former);
    let accepted = request(
        &store,
        WRITER_A,
        "accepted",
        vec![inline("a", WRITER_A, 1, "a")],
    );
    store.push(&old, &accepted).unwrap();
    let accepted_proof = store.operation(&old, "accepted").unwrap();
    let cancelled = store
        .cancel_operation(
            &old,
            "cancelled",
            &CancelOperationRequest {
                body_digest: "a".repeat(64),
            },
        )
        .unwrap();
    let candidate = store.add_device().unwrap();
    let new = actor(&store, &candidate);
    let intent = claim(Some(&former.token));
    let receipt = store.claim_new_device_writer(&new, &intent).unwrap();
    assert!(receipt.former_credential_inactive);
    assert_eq!(
        store
            .authenticate(&former.library_id, &former.token)
            .unwrap_err()
            .code,
        "unauthorized"
    );
    assert_eq!(
        store.push(&old, &accepted).unwrap_err().code,
        "unauthorized"
    );
    let db = metadata(&root);
    for (operation, proof) in [("accepted", accepted_proof), ("cancelled", cancelled)] {
        let body: String = db
            .query_row(
                "SELECT body FROM operations WHERE device=?1 AND operation=?2",
                rusqlite::params![old.id, operation],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<risunest_sync_wire::lww::OperationReceipt>(&body).unwrap(),
            proof
        );
    }
    let stored: String = db
        .query_row("SELECT body FROM device_writer_claims", [], |r| r.get(0))
        .unwrap();
    assert!(!stored.contains(&former.token));
    assert!(!stored.contains(&candidate.token));
    drop(db);
    drop(store);
    let reopened = Store::open(root.path()).unwrap();
    let new = actor(&reopened, &candidate);
    assert_eq!(
        reopened.claim_new_device_writer(&new, &intent).unwrap(),
        receipt
    );
    assert_eq!(
        reopened
            .authenticate(&former.library_id, &former.token)
            .unwrap_err()
            .code,
        "unauthorized"
    );
    let publication = request(
        &reopened,
        RESERVED,
        "new-publication",
        vec![inline("b", WRITER_B, 2, "b")],
    );
    reopened.push(&new, &publication).unwrap();
}

#[test]
fn revocation_failure_rolls_back_writer_and_claim_together() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let former = store.add_device().unwrap();
    let new = device(&store);
    let db = metadata(&root);
    db.execute_batch("CREATE TRIGGER synthetic_revoke_failure BEFORE UPDATE OF revoked ON devices WHEN NEW.revoked=1 BEGIN SELECT RAISE(ABORT,'synthetic revocation failure'); END").unwrap();
    assert!(store
        .claim_new_device_writer(&new, &claim(Some(&former.token)))
        .is_err());
    actor(&store, &former);
    for table in ["writers", "device_writer_claims"] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    db.execute_batch("DROP TRIGGER synthetic_revoke_failure")
        .unwrap();
    assert!(
        store
            .claim_new_device_writer(&new, &claim(Some(&former.token)))
            .unwrap()
            .former_credential_inactive
    );
}

#[test]
fn former_credential_must_differ_but_can_already_be_inactive_or_absent() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let candidate = store.add_device().unwrap();
    let new = actor(&store, &candidate);
    assert_eq!(
        store
            .claim_new_device_writer(&new, &claim(Some(&candidate.token)))
            .unwrap_err()
            .code,
        "registration-not-new"
    );
    assert_eq!(
        store
            .claim_new_device_writer(&new, &claim(Some("invalid")))
            .unwrap_err()
            .code,
        "invalid-former-token"
    );
    let unknown = "f".repeat(64);
    assert!(
        store
            .claim_new_device_writer(&new, &claim(Some(&unknown)))
            .unwrap()
            .former_credential_inactive
    );
    assert!(store.authenticate(&candidate.library_id, &unknown).is_err());
}

#[test]
fn administrative_restore_requires_fresh_registration_and_claim_uses_current_epoch() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let former = store.add_device().unwrap();
    let pre_restore = store.head().unwrap().epoch;
    store.rotate_restored_epoch().unwrap();
    assert!(store
        .authenticate(&former.library_id, &former.token)
        .is_err());
    let new = device(&store);
    let receipt = store
        .claim_new_device_writer(&new, &claim(Some(&former.token)))
        .unwrap();
    assert!(receipt.former_credential_inactive);
    assert_ne!(receipt.epoch, pre_restore);
    assert_eq!(receipt.epoch, store.head().unwrap().epoch);
    assert!(store
        .authenticate(&former.library_id, &former.token)
        .is_err());
}

#[tokio::test]
async fn claim_revokes_a_request_already_authenticated_before_its_body_arrives() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::init(root.path()).unwrap());
    let former = store.add_device().unwrap();
    let candidate = store.add_device().unwrap();
    let workload = Workload::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = http::router_with_workload(store.clone(), workload.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let publication = request(
        &store,
        WRITER_A,
        "delayed",
        vec![inline("a", WRITER_A, 1, "a")],
    );
    let body = canonical::encode(&publication).unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let headers = format!("POST /push HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nX-Risu-Library: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", former.token, former.library_id, body.len());
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(&body[..body.len() / 2]).await.unwrap();
    // Admission follows authentication, and the partial body holds that request open.
    tokio::time::timeout(Duration::from_secs(3), async {
        while workload.status().unwrap().active_requests != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{address}/session/claim-writer"))
        .bearer_auth(&candidate.token)
        .header("x-risu-library", &candidate.library_id)
        .json(&claim(Some(&former.token)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let receipt = response.json::<NewDeviceClaimReceipt>().await.unwrap();
    assert!(receipt.former_credential_inactive);
    assert_eq!(receipt.device_id, candidate.device_id);
    socket.write_all(&body[body.len() / 2..]).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 401"));
    assert_eq!(store.head().unwrap().seq.as_str(), "0");
    assert_eq!(
        metadata(&root)
            .query_row("SELECT count(*) FROM operations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    server.abort();
}

#[tokio::test]
async fn the_claim_route_reports_each_registration_claim_and_use_without_changing_them() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::init(root.path()).unwrap());
    let unused = store.add_device().unwrap();
    let published = store.add_device().unwrap();
    let claimed = store.add_device().unwrap();
    store
        .push(
            &actor(&store, &published),
            &request(
                &store,
                WRITER_A,
                "published",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    let receipt = store
        .claim_new_device_writer(&actor(&store, &claimed), &claim(None))
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = http::router(store.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let read = |credential: &DeviceCredential| {
        client
            .get(format!("http://{address}/session/claim-writer"))
            .bearer_auth(&credential.token)
            .header("x-risu-library", &credential.library_id)
            .send()
    };
    let response = read(&unused).await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        br#"{"claim":null,"used":false}"#
    );
    let response = read(&published).await.unwrap();
    assert_eq!(
        canonical::decode::<NewDeviceClaimState>(&response.bytes().await.unwrap(), 4096).unwrap(),
        NewDeviceClaimState {
            claim: None,
            used: true
        }
    );
    let response = read(&claimed).await.unwrap();
    let state =
        canonical::decode::<NewDeviceClaimState>(&response.bytes().await.unwrap(), 4096).unwrap();
    assert!(state.used);
    let saved = state.claim.unwrap();
    assert_eq!(saved.request_digest, claim(None).digest().unwrap());
    assert_eq!(saved.receipt, receipt);
    // Reading the state claims nothing: the unused registration still claims a writer, and the
    // claimed one still refuses any other publisher.
    let other = NewDeviceClaimRequest {
        writer_id: "00000000-0000-4000-8000-000000000005".into(),
        authorization_id: "00000000-0000-4000-8000-000000000006".into(),
        former_token: None,
    };
    store
        .claim_new_device_writer(&actor(&store, &unused), &other)
        .unwrap();
    assert_eq!(
        store
            .push(
                &actor(&store, &claimed),
                &request(
                    &store,
                    WRITER_B,
                    "other-writer",
                    vec![inline("b", WRITER_B, 2, "b")]
                ),
            )
            .unwrap_err()
            .code,
        "writer-collision"
    );
    server.abort();
}

#[test]
fn claim_identity_remains_permanent_after_revoked_device_cleanup_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let candidate = store.add_device().unwrap();
    let old = actor(&store, &candidate);
    let receipt = store.claim_new_device_writer(&old, &claim(None)).unwrap();
    store
        .cancel_operation(
            &old,
            "cancelled",
            &CancelOperationRequest {
                body_digest: "a".repeat(64),
            },
        )
        .unwrap();
    store.revoke_device(&old.id).unwrap();
    store.maintain().unwrap();
    assert!(!store
        .managed_devices()
        .unwrap()
        .iter()
        .any(|device| device.id == old.id));
    let db = metadata(&root);
    let body: String = db
        .query_row(
            "SELECT body FROM device_writer_claims WHERE device=?1",
            [&old.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<NewDeviceClaimReceipt>(&body).unwrap(),
        receipt
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM operations", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    drop(store);
    let store = Store::open(root.path()).unwrap();
    let new = device(&store);
    let reused_authorization = NewDeviceClaimRequest {
        writer_id: WRITER_B.into(),
        ..claim(None)
    };
    assert_eq!(
        store
            .claim_new_device_writer(&new, &reused_authorization)
            .unwrap_err()
            .code,
        "registration-integrity"
    );
    let reused_writer = NewDeviceClaimRequest {
        authorization_id: "different-authorization".into(),
        ..claim(None)
    };
    assert_eq!(
        store
            .claim_new_device_writer(&new, &reused_writer)
            .unwrap_err()
            .code,
        "writer-collision"
    );
    let fresh = NewDeviceClaimRequest {
        writer_id: WRITER_B.into(),
        authorization_id: "different-authorization".into(),
        former_token: Some(candidate.token.clone()),
    };
    assert!(
        store
            .claim_new_device_writer(&new, &fresh)
            .unwrap()
            .former_credential_inactive
    );
    assert!(store
        .authenticate(&candidate.library_id, &candidate.token)
        .is_err());
}

fn versions(root: &tempfile::TempDir, key: &str) -> i64 {
    metadata(root)
        .query_row(
            "SELECT count(*) FROM writer_versions WHERE key=?1",
            [key],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn a_fresh_publisher_forwards_an_entry_the_former_registration_already_published() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let former = store.add_device().unwrap();
    let old = actor(&store, &former);
    let entry = inline("language", WRITER_A, 10, "ja");
    let first = store
        .push(&old, &request(&store, WRITER_A, "old", vec![entry.clone()]))
        .unwrap();
    assert_eq!(first.accepted_keys, vec![entry.key.clone()]);
    let candidate = store.add_device().unwrap();
    let new = actor(&store, &candidate);
    store
        .claim_new_device_writer(&new, &claim(Some(&former.token)))
        .unwrap();
    let repeated = store
        .push(
            &new,
            &request(&store, RESERVED, "fresh", vec![entry.clone()]),
        )
        .unwrap();
    assert!(repeated.accepted_keys.is_empty());
    assert_eq!(repeated.seq.0, 1);
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
    assert_eq!(versions(&root, entry.key.as_str()), 1);
    let pin = store.create_state_pin(&new).unwrap();
    let state = store.state_page(&new, &pin.pin_id, None, 16).unwrap();
    assert_eq!(state.items, vec![entry]);
}

#[test]
fn a_fresh_publisher_forwards_an_entry_whose_former_publication_was_rejected() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let former = store.add_device().unwrap();
    let old = actor(&store, &former);
    store
        .push(
            &old,
            &request(
                &store,
                WRITER_A,
                "earlier",
                vec![inline("other", WRITER_A, 5, "x")],
            ),
        )
        .unwrap();
    let entry = inline("language", WRITER_A, 10, "ja");
    let lost = request(&store, WRITER_A, "lost", vec![entry.clone()]);
    assert!(matches!(
        store
            .cancel_operation(
                &old,
                "lost",
                &CancelOperationRequest {
                    body_digest: lost.digest().unwrap(),
                },
            )
            .unwrap(),
        risunest_sync_wire::lww::OperationReceipt::Rejected { .. }
    ));
    let candidate = store.add_device().unwrap();
    let new = actor(&store, &candidate);
    store
        .claim_new_device_writer(&new, &claim(Some(&former.token)))
        .unwrap();
    let receipt = store
        .push(
            &new,
            &request(&store, RESERVED, "fresh", vec![entry.clone()]),
        )
        .unwrap();
    assert_eq!(receipt.accepted_keys, vec![entry.key.clone()]);
    assert_eq!(receipt.seq.0, 2);
    assert_eq!(versions(&root, entry.key.as_str()), 1);
    let journal: Vec<String> = metadata(&root)
        .prepare("SELECT key FROM journal ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        journal,
        [
            inline("other", WRITER_A, 5, "x").key.as_str().to_owned(),
            entry.key.as_str().to_owned()
        ]
    );
}

fn binding(writer: &str) -> WriterBindingRequest {
    WriterBindingRequest {
        writer_id: writer.into(),
    }
}
fn device_writers(root: &tempfile::TempDir, device: &Device) -> Vec<String> {
    let db = metadata(root);
    let mut query = db
        .prepare("SELECT writer FROM writers WHERE device=?1 ORDER BY writer")
        .unwrap();
    let writers = query
        .query_map([&device.id], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    writers
}

#[test]
fn a_registration_takes_one_installation_writer_however_it_joined() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let joined = device(&store);
    store
        .bind_device_writer(&joined, &binding(WRITER_A))
        .unwrap();
    // The installation that joined binds again when it reconnects.
    store
        .bind_device_writer(&joined, &binding(WRITER_A))
        .unwrap();
    assert_eq!(
        store.new_device_writer_claim(&joined).unwrap(),
        NewDeviceClaimState {
            claim: None,
            used: true
        }
    );
    // Another installation is refused before it publishes, and when it publishes anyway.
    assert_eq!(
        store
            .bind_device_writer(&joined, &binding(WRITER_B))
            .unwrap_err()
            .code,
        "registration-used"
    );
    assert_eq!(
        store
            .push(
                &joined,
                &request(
                    &store,
                    WRITER_B,
                    "second",
                    vec![inline("b", WRITER_B, 1, "b")]
                )
            )
            .unwrap_err()
            .code,
        "writer-collision"
    );
    assert_eq!(
        store
            .claim_new_device_writer(&joined, &claim(None))
            .unwrap_err()
            .code,
        "registration-used"
    );
    store
        .push(
            &joined,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    store
        .bind_device_writer(&joined, &binding(WRITER_A))
        .unwrap();
    assert_eq!(device_writers(&root, &joined), [WRITER_A]);
    // A writer another registration holds cannot join with a new code, which stays unused.
    let fresh = device(&store);
    assert_eq!(
        store
            .bind_device_writer(&fresh, &binding(WRITER_A))
            .unwrap_err()
            .code,
        "writer-collision"
    );
    assert!(!store.new_device_writer_claim(&fresh).unwrap().used);
    store
        .bind_device_writer(&fresh, &binding(WRITER_B))
        .unwrap();
    assert_eq!(device_writers(&root, &fresh), [WRITER_B]);
    assert_eq!(
        store
            .bind_device_writer(&fresh, &binding("invalid"))
            .unwrap_err()
            .code,
        "invalid-writer-id"
    );
}

#[test]
fn a_registration_used_without_a_binding_refuses_every_other_writer() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    // A client that publishes without binding first takes the registration with its first push.
    let published = device(&store);
    store
        .push(
            &published,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    assert_eq!(
        store
            .push(
                &published,
                &request(
                    &store,
                    WRITER_B,
                    "second",
                    vec![inline("b", WRITER_B, 2, "b")]
                )
            )
            .unwrap_err()
            .code,
        "writer-collision"
    );
    assert_eq!(
        store
            .bind_device_writer(&published, &binding(WRITER_B))
            .unwrap_err()
            .code,
        "registration-used"
    );
    store
        .bind_device_writer(&published, &binding(WRITER_A))
        .unwrap();
    assert_eq!(device_writers(&root, &published), [WRITER_A]);
    // An operation alone marks a registration used.
    let rejected = device(&store);
    assert_eq!(
        store
            .push(&rejected, &request(&store, WRITER_B, "empty", vec![]))
            .unwrap_err()
            .code,
        "empty-push"
    );
    assert_eq!(
        store
            .bind_device_writer(&rejected, &binding(WRITER_B))
            .unwrap_err()
            .code,
        "registration-used"
    );
    // A claimed registration rejoins only with the writer it claimed.
    let claimed = device(&store);
    store
        .claim_new_device_writer(&claimed, &claim(None))
        .unwrap();
    assert_eq!(
        store
            .bind_device_writer(&claimed, &binding(WRITER_B))
            .unwrap_err()
            .code,
        "registration-used"
    );
    store
        .bind_device_writer(&claimed, &binding(RESERVED))
        .unwrap();
    assert_eq!(device_writers(&root, &claimed), [RESERVED]);
}

#[test]
fn a_registration_already_holding_two_writers_refuses_both() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let shared = device(&store);
    store
        .push(
            &shared,
            &request(
                &store,
                WRITER_A,
                "first",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    metadata(&root)
        .execute(
            "INSERT INTO writers(writer,device) VALUES(?1,?2)",
            [WRITER_B, shared.id.as_str()],
        )
        .unwrap();
    for (writer, operation) in [(WRITER_A, "again-a"), (WRITER_B, "again-b")] {
        assert_eq!(
            store
                .push(
                    &shared,
                    &request(&store, writer, operation, vec![inline("c", writer, 3, "c")])
                )
                .unwrap_err()
                .code,
            "writer-collision"
        );
        assert_eq!(
            store
                .bind_device_writer(&shared, &binding(writer))
                .unwrap_err()
                .code,
            "registration-used"
        );
    }
    assert_eq!(store.head().unwrap().seq.as_str(), "1");
}

#[tokio::test]
async fn the_writer_route_binds_one_installation_and_refuses_another() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::init(root.path()).unwrap());
    let registration = store.add_device().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = http::router(store.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let bind = |token: &str, body: Vec<u8>| {
        client
            .post(format!("http://{address}/session/writer"))
            .bearer_auth(token)
            .header("x-risu-library", &registration.library_id)
            .body(body)
            .send()
    };
    let body = |writer: &str| canonical::encode(&binding(writer)).unwrap();
    for _ in 0..2 {
        let response = bind(&registration.token, body(WRITER_A)).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }
    let response = bind(&registration.token, body(WRITER_B)).await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        response.bytes().await.unwrap().as_ref(),
        br#"{"error":"registration-used"}"#
    );
    let response = bind(&registration.token, br#"{"writerId":"invalid"}"#.to_vec())
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    let response = bind(&"0".repeat(64), body(WRITER_B)).await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        device_writers(&root, &actor(&store, &registration)),
        [WRITER_A]
    );
    server.abort();
}
