use super::*;

#[test]
fn blank_and_partial_credentials_reach_the_wire_without_an_extra_mode() {
    runtime().block_on(async {
        for (account, password) in [("", ""), ("user", ""), ("", "password"), ("user", "password")] {
            let server = WireServer::start(vec![established_root()]);
            let test = loopback_dependencies(MemoryVault::with(SECRET_REF, password.as_bytes()), NOW_MS);
            let provider = create(test.dependencies.clone()).unwrap();
            let config = ConnectionConfig {
                provider: PROVIDER_ID.into(), profile: None, endpoint: server.url.to_string(),
                account_id: account.into(), location: BTreeMap::from([("root".into(), ROOT.into())]), oauth_profile: None,
            };
            provider.open_repository(&config, &SecretRef(SECRET_REF.into()), OpenMode::Existing, &Cancellation::default()).await.unwrap();
            let requests = server.requests.lock().unwrap();
            let authorization = requests[0].headers.lines().filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("authorization")).map(|(_, value)| value.trim().to_owned());
            let expected = (!(account.is_empty() && password.is_empty()))
                .then(|| format!("Basic {}", STANDARD.encode(format!("{account}:{password}"))));
            assert_eq!(authorization, expected);
        }
    });
}

#[test]
fn metadata_redirects_replay_propfind_and_resolve_the_final_collection() {
    runtime().block_on(async {
        for status in [301, 302, 307, 308] {
            let harness = Harness::start(vec![
                reply(status, &[("Location", "/moved/repository/")], b""),
                multistatus_reply(&[collection_response("/moved/repository/"), collection_response("/moved/repository/descriptors/")]),
            ]);
            harness.opened().await;
            assert_eq!(harness.count(), 2);
            assert_eq!(harness.line(1), "PROPFIND /moved/repository/ HTTP/1.1");
            assert_eq!(harness.body(1), PROPFIND_BODY);
            assert_eq!(harness.header(0, "authorization"), harness.header(1, "authorization"));
            assert_eq!(harness.header(1, "depth").as_deref(), Some("1"));
        }
    });
}

#[test]
fn streamed_put_redirects_reopen_the_source_and_keep_conditional_headers() {
    runtime().block_on(async {
        for status in [301, 302, 307, 308] {
            let harness = Harness::start(vec![established_root(), reply(status, &[("Location", "uploaded-object")], b""), reply(201, &[], b"")]);
            let repository = harness.opened().await;
            let directory = tempfile::tempdir().unwrap();
            let bytes = vec![42; 256 * 1024 + 7];
            let source = source(&directory, "upload", &bytes);
            let intent = intent(&repository, "redirected-object", ObjectRole::Descriptor, &bytes);
            let result = harness.provider.create_object(&repository, &intent, &source, None, &Cancellation::default()).await.unwrap();
            assert!(result.complete);
            assert_eq!(result.byte_length, bytes.len() as u64);
            assert_eq!(harness.body(1), bytes);
            assert_eq!(harness.body(2), bytes);
            assert!(harness.line(2).contains("/descriptors/uploaded-object "));
            assert_eq!(harness.header(2, "if-none-match").as_deref(), Some("*"));
        }
    });
}

#[test]
fn redirected_reads_keep_conditional_headers_and_stream_the_final_body() {
    runtime().block_on(async {
        let bytes = b"redirected data";
        let harness = Harness::start(vec![established_root(), reply(303, &[("Location", "/moved/data")], b""), reply(200, &[], bytes)]);
        let repository = harness.opened().await;
        let directory = tempfile::tempdir().unwrap();
        let mut sink = sink(&directory, "download");
        let locator = locator(&repository, Some("descriptors"), "descriptors/test");
        let result = harness.provider.read_object(&repository, &locator, Some(&VersionToken("\"before\"".into())), &mut sink, &Cancellation::default()).await.unwrap();
        assert!(matches!(result, ReadReceipt::Body(_)));
        assert_eq!(std::fs::read(directory.path().join("download")).unwrap(), bytes);
        assert_eq!(harness.line(2), "GET /moved/data HTTP/1.1");
        assert_eq!(harness.header(2, "if-none-match").as_deref(), Some("\"before\""));
    });
}

#[test]
fn redirect_to_another_origin_drops_credentials() {
    runtime().block_on(async {
        let destination = WireServer::start(vec![multistatus_reply(&[
            collection_response("/moved/"), collection_response("/moved/descriptors/"),
        ])]);
        let url = destination.url.join("/moved/").unwrap().to_string();
        let harness = Harness::start(vec![reply(307, &[("Location", &url)], b"")]);
        harness.opened().await;
        assert!(harness.header(0, "authorization").is_some());
        let requests = destination.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].headers.to_ascii_lowercase().contains("authorization:"));
        assert_eq!(requests[0].body, PROPFIND_BODY);
    });
}

#[test]
fn redirected_head_writes_keep_the_cas_body_and_collection_operations_keep_the_method() {
    runtime().block_on(async {
        let harness = Harness::start(vec![established_root(),
            reply(307, &[("Location", "/moved/head")], b""), reply(204, &[("ETag", "\"after\"")], b""),
            reply(301, &[("Location", "/moved/collection/")], b""), reply(201, &[], b""),
            reply(308, &[("Location", "/moved/deleted")], b""), reply(204, &[], b""),
        ]);
        let repository = harness.opened().await;
        let target = locator(&repository, None, HEAD_OBJECT);
        let head = head();
        let receipt = harness.provider.compare_exchange_head(&repository, &target,
            &ExpectedHead::Exact(VersionToken("\"before\"".into())), &head, &Cancellation::default()).await.unwrap();
        assert_eq!(receipt.version, Some(VersionToken("\"after\"".into())));
        assert_eq!(harness.body(2), head.as_bytes());
        assert_eq!(harness.header(2, "if-match").as_deref(), Some("\"before\""));
        let context = context_of(&repository).unwrap();
        let provider = WebdavProvider { dependencies: harness.test.dependencies.clone(), listings: Mutex::new(BTreeMap::new()) };
        provider.mkcol_new(context, context.base.join("collection/").unwrap(), &Cancellation::default()).await.unwrap();
        provider.delete(context, context.base.join("deleted").unwrap(), &Cancellation::default()).await.unwrap();
        assert_eq!(harness.line(4), "MKCOL /moved/collection/ HTTP/1.1");
        assert_eq!(harness.line(6), "DELETE /moved/deleted HTTP/1.1");
    });
}

#[test]
fn a_303_result_page_does_not_count_as_a_successful_write() {
    runtime().block_on(async {
        let harness = Harness::start(vec![established_root(), reply(303, &[("Location", "/result")], b"")]);
        let repository = harness.opened().await;
        let target = locator(&repository, None, HEAD_OBJECT);
        let error = harness.provider.compare_exchange_head(&repository, &target, &ExpectedHead::Absent,
            &head(), &Cancellation::default()).await.unwrap_err();
        assert_eq!(error.http_status, Some(303));
        assert!(error.cause.0.unwrap().contains("cannot confirm"));
        assert_eq!(harness.count(), 2);
    });
}

#[test]
fn cancellation_after_a_redirect_stops_the_pending_response_body() {
    runtime().block_on(async {
        let harness = Harness::start(vec![established_root(), reply(307, &[("Location", "/moved/data")], b""), Reply::DelayedBody]);
        let repository = harness.opened().await;
        let directory = tempfile::tempdir().unwrap();
        let mut sink = sink(&directory, "cancelled");
        let cancel = Cancellation::default();
        let target = locator(&repository, None, HEAD_OBJECT);
        let read = harness.provider.read_object(&repository, &target, None, &mut sink, &cancel);
        let trigger = async {
            while harness.count() < 3 { tokio::time::sleep(std::time::Duration::from_millis(1)).await; }
            cancel.cancel();
        };
        let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), futures::future::join(read, trigger)).await.unwrap();
        assert_eq!(outcome.unwrap_err().kind, ErrorKind::Cancelled);
        assert!(!sink.is_verified());
    });
}

#[test]
fn loops_invalid_targets_and_excessive_redirects_report_the_specific_reason() {
    runtime().block_on(async {
        let looped = Harness::start(vec![reply(302, &[("Location", &format!("{}/", encoded_root()))], b"")]);
        let error = looped.open(OpenMode::Existing).await.err().unwrap();
        assert!(error.cause.0.unwrap().contains("loop"));
        assert_eq!(looped.count(), 1);
        for (headers, reason) in [(vec![], "Location header"), (vec![("Location", "file:///tmp/data")], "HTTP or HTTPS")] {
            let harness = Harness::start(vec![reply(302, &headers, b"")]);
            let error = harness.open(OpenMode::Existing).await.err().unwrap();
            assert_eq!(error.http_status, Some(302));
            assert!(error.cause.0.unwrap().contains(reason));
            assert_eq!(harness.count(), 1);
        }
        let harness = Harness::start((0..11).map(|index| reply(307, &[("Location", &format!("/redirect/{index}"))], b"")).collect());
        let error = harness.open(OpenMode::Existing).await.err().unwrap();
        assert!(error.cause.0.unwrap().contains("10 redirects"));
        assert_eq!(harness.count(), 11);
    });
}
