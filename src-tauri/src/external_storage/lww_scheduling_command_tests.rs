use super::*;

#[test]
fn routine_limit_is_optional_positive_and_forbidden_for_exit_drain() {
    let request = |extra: serde_json::Value| {
        let mut value = serde_json::json!({"connectionId":"synthetic", "bindingAuthority":"0", "requestId":"test"});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value::<Request>(value)
    };
    assert!(request(serde_json::json!({})).unwrap().check_publish_limit().is_ok());
    let limited = request(serde_json::json!({"turnLimit":4})).unwrap();
    assert_eq!(limited.turn_limit, Some(4));
    assert!(limited.check_publish_limit().is_ok());
    assert_eq!(request(serde_json::json!({"turnLimit":0})).unwrap().check_publish_limit().unwrap_err().kind, ErrorKind::PreconditionFailed);
    assert!(request(serde_json::json!({"turnLimit":-1})).is_err());
    assert!(request(serde_json::json!({"turnLimit":1.5})).is_err());
    let exit = serde_json::json!({"revision":"1", "libraryEpoch":"library", "selectionEpoch":"selection"});
    assert!(request(serde_json::json!({"exitTarget":exit})).unwrap().check_publish_limit().is_ok());
    assert_eq!(request(serde_json::json!({"turnLimit":4,"exitTarget":exit})).unwrap().check_publish_limit().unwrap_err().kind, ErrorKind::PreconditionFailed);
    assert_eq!(serde_json::to_value(PublicationResult::default()).unwrap()["more"], false);
}

#[test]
fn removed_connection_invalidates_inflight_cache_and_cancellation() {
    tauri::async_runtime::block_on(async {
        let id = format!("synthetic-removal-{}", uuid::Uuid::new_v4());
        let context = context(&id).unwrap();
        let cancellation = context.cancel.lock().unwrap().clone();
        invalidate_connection_cache(&id).await.unwrap();
        assert_eq!(cancellation.check().unwrap_err().kind, ErrorKind::Cancelled);
        invalidate_connection_cache("synthetic-never-opened").await.unwrap();
    });
}

#[test]
fn expired_routine_admission_renews_once_at_entry_and_rejects_fresh_clock_skew() {
    for skewed in [false, true] {
        tauri::async_runtime::block_on(async move {
            use crate::external_storage::{fake, lww_engine::Admission, lww_tests::CycleFixture, providers::webdav, wire_fixture::{Reply, WireServer}};
            let fixture = CycleFixture::new();
            let now = runtime::now_ms();
            let listing = |offset: u64, entries: &[&str]| Reply::Http {
                status: 207,
                headers: vec![("Content-Type".into(), "application/xml".into()), ("Date".into(),
                    httpdate::fmt_http_date(std::time::UNIX_EPOCH + std::time::Duration::from_millis(now + offset)))],
                body: format!("<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">{}</D:multistatus>",
                    entries.iter().map(|href| format!("<D:response><D:href>{href}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>")).collect::<String>()).into_bytes(),
            };
            let server = WireServer::start(vec![
                listing(0, &["/synthetic/sync/", "/synthetic/sync/descriptors/"]),
                listing(if skewed { 20 * 60_000 } else { 0 }, &["/synthetic/sync/segments/"]),
            ]);
            let mut dependencies = fake::loopback_dependencies(fake::MemoryVault::with("synthetic", b"password"), now);
            // Admission compares wall time with elapsed real time, including under parallel test load.
            dependencies.dependencies.clock = Arc::new(crate::external_storage::http::SystemClock);
            let provider = webdav::create(dependencies.dependencies.clone()).unwrap();
            let config = ConnectionConfig { provider: "webdav".into(), profile: None,
                endpoint: server.url.as_str().into(), account_id: "synthetic".into(),
                location: BTreeMap::from([("root".into(), "sync".into())]), oauth_profile: None };
            let (repository, _) = provider.open_repository(&config, &SecretRef("synthetic".into()), OpenMode::Existing, &Cancellation::default()).await.unwrap();
            let mut engine = fixture.receiver;
            engine.provider = provider;
            engine.repository = repository;
            let mut admission = Admission::synthetic(runtime::now_ms());
            admission.age_for_test(std::time::Duration::from_secs(16 * 60));
            engine.admission = Some(admission);
            assert_eq!(engine.admitted_upper().unwrap_err().kind, ErrorKind::ClockSkew);
            let mut session = Session::new();
            session.engine = Some(engine);
            session.dependencies = Some(dependencies.dependencies);
            session.fresh_after = Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
            let result = admit_clock(&mut session).await;
            if skewed {
                assert_eq!(result.unwrap_err().kind, ErrorKind::ClockSkew);
                assert!(session.engine.as_ref().unwrap().admitted_upper().is_err());
            } else {
                result.unwrap();
                assert!(session.engine.as_ref().unwrap().admitted_upper().is_ok());
                admit_clock(&mut session).await.unwrap();
            }
            assert_eq!(server.requests.lock().unwrap().len(), 2);
        });
    }
}
