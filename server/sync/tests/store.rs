mod common;
use common::*;
use risunest_sync_server::{config::Config, store::Store};
use risunest_sync_wire::hash;

#[test]
fn generated_identifiers_are_uuids_and_credentials_remain_full_length() {
    use risunest_sync_server::store::{ObjectIdentity, UploadManifest};

    let root = tempfile::tempdir().unwrap();
    let store = Store::init(root.path()).unwrap();
    let credential = store.add_device().unwrap();
    let device = store
        .authenticate(&credential.library_id, &credential.token)
        .unwrap();
    let head = store.head().unwrap();
    let pin = store.create_state_pin(&device).unwrap();
    let body = b"synthetic retained content";
    let digest = hash(body);
    let upload = store
        .begin_upload(
            &device,
            &UploadManifest {
                hash: digest.clone(),
                size: (body.len() as u64).into(),
            },
        )
        .unwrap();
    let ids = [
        &head.library_id,
        &head.epoch,
        &credential.device_id,
        &pin.pin_id,
        &upload,
    ];
    for value in ids {
        let id = uuid::Uuid::parse_str(value).unwrap();
        assert_eq!(id.get_version(), Some(uuid::Version::Random));
        assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
        assert_eq!(id.to_string(), *value);
    }
    assert_eq!(
        ids.into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        ids.len()
    );
    store.put_object(&device, &digest, body).unwrap();
    let retained = store
        .retain_objects(
            &device,
            &head.epoch,
            &[ObjectIdentity {
                hash: digest,
                size: Some((body.len() as u64).into()),
            }],
        )
        .unwrap();
    risunest_sync_wire::validate_hash(&credential.token).unwrap();
    risunest_sync_wire::validate_hash(&retained[0].retention_id).unwrap();
    store.rotate_restored_epoch().unwrap();
    let restored = store.head().unwrap();
    assert_eq!(restored.library_id, head.library_id);
    assert_ne!(restored.epoch, head.epoch);
    assert_eq!(
        uuid::Uuid::parse_str(&restored.epoch)
            .unwrap()
            .get_version(),
        Some(uuid::Version::Random)
    );
}

#[test]
fn owner_lock_and_reinitialization_protect_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    assert_eq!(Store::open(dir.path()).err().unwrap().code, "data-dir-busy");
    let head = store.head().unwrap();
    drop(store);
    assert_eq!(
        Store::init(dir.path()).err().unwrap().code,
        "already-initialized"
    );
    assert_eq!(Store::open(dir.path()).unwrap().head().unwrap(), head);
}
#[test]
fn explicit_interface_listeners_and_absolute_storage_are_allowed() {
    let dir = tempfile::tempdir().unwrap();
    for listen in ["0.0.0.0:14319", "192.168.1.2:14319", "[::]:14319"] {
        assert!(Config {
            data_dir: dir.path().into(),
            listen: listen.parse().unwrap(),
            https_proxy: true
        }
        .validate()
        .is_ok());
    }
    assert!(Config {
        data_dir: "relative".into(),
        listen: "127.0.0.1:0".parse().unwrap(),
        https_proxy: false
    }
    .validate()
    .is_err());
}
#[test]
fn tokens_are_scoped_verifiers_and_revocation_is_checked_on_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let c = store.add_device().unwrap();
    assert_eq!(c.token.len(), 64);
    assert!(store.authenticate("other-library", &c.token).is_err());
    assert!(store.authenticate(&c.library_id, &"0".repeat(64)).is_err());
    let a = store.authenticate(&c.library_id, &c.token).unwrap();
    store.revoke_device(&a.id).unwrap();
    assert!(store.authenticate(&c.library_id, &c.token).is_err());
    assert!(store.put_object(&a, &hash(b"x"), b"x").is_err());
    assert!(store
        .push(
            &a,
            &request(
                &store,
                WRITER_A,
                "revoked",
                vec![inline("a", WRITER_A, 1, "x")]
            )
        )
        .is_err());
    drop(store);
    // Synthetic credential only. Never print token bytes.
    let bytes = std::fs::read(dir.path().join("metadata.sqlite")).unwrap();
    assert!(!bytes
        .windows(c.token.len())
        .any(|w| w == c.token.as_bytes()));
}
#[test]
fn objects_publish_before_metadata_and_raw_content_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::init(dir.path()).unwrap();
    let a = device(&store);
    let head = store.head().unwrap();
    assert!(store.put_object(&a, &hash(b"x"), b"wrong").is_err());
    assert!(store.put_object(&a, "../../metadata.sqlite", b"x").is_err());
    assert_eq!(store.head().unwrap(), head);
    for body in [b"".as_slice(), br#"{ "x":1.0 }"#, &[0xff, 0]] {
        store.put_object(&a, &hash(body), body).unwrap();
        assert_eq!(store.get_object(&hash(body)).unwrap(), body);
    }
    assert_eq!(store.head().unwrap(), head);
}
#[test]
fn linked_data_directory_and_cas_shard_cannot_escape_private_storage() {
    fn link_directory(link: &std::path::Path, target: &std::path::Path) {
        #[cfg(windows)]
        {
            // Directory junctions require no symlink privilege. Arguments refer
            // only to this test's temporary directories; no cleanup shell runs.
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "synthetic junction creation failed"
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }
    let parent = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked-root");
    link_directory(&linked, external.path());
    assert_eq!(
        Store::init(&linked).err().unwrap().code,
        "unsafe-storage-path"
    );
    assert!(!external.path().join("metadata.sqlite").exists());
    let root = parent.path().join("private");
    let store = Store::init(&root).unwrap();
    let a = device(&store);
    // Only a body large enough to become a file reaches the shard layout at
    // all, so that is what the escape has to be attempted with.
    let body = vec![b'e'; 128 * 1024];
    let digest = hash(&body);
    let shard = root.join("objects").join(&digest[..2]);
    link_directory(&shard, external.path());
    assert_eq!(
        store.put_object(&a, &digest, &body).unwrap_err().code,
        "unsafe-storage-path"
    );
    assert!(!external.path().join(&digest).exists());
}
