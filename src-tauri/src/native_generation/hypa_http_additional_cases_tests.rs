use super::*;

async fn owner_request(input: HttpRequestSpec, configured: Limits) -> Result<WireReply, Failure> {
    let (operation, _, _) = operation();
    let mut executor = Executor::new(operation, configured, ORIGIN.into())?;
    let reply = tokio::time::timeout(Duration::from_secs(10), executor.execute(input)).await
        .expect("synthetic owner exchange did not settle")?;
    executor.resume(reply, |reply| match reply {
        HostReply::Http { status, body } => WireReply { status, body },
        _ => unreachable!(),
    })
}

fn provider_request(url: &str, budget: &Budget) -> (Service, HttpRequestSpec) {
    let target = RemoteTarget::capture(&Text::from_str("custom", budget).unwrap(), None,
        Some(&Text::from_str(url, budget).unwrap()), None, budget).unwrap();
    let config = CapturedConfig::capture(None, None, None, None, budget).unwrap();
    let mut service = Service::new();
    let Step::NeedsHost(HostRequest::Http(request)) = service.run(embeddings::Request {
        target, config, purpose: Purpose::Query, input: Input::Scalar(Text::from_str("synthetic", budget).unwrap()),
    }, budget).unwrap() else { panic!("provider did not yield HTTP") };
    (service, request)
}

#[tokio::test]
async fn native_final_response_rejections_match_actual_plugin_failure() {
    for status in [101, 103, 600] {
        let mut server = Loopback::new(vec![Reply::plain(status, JSON), Reply::plain(status, JSON)]).await;
        let url = format!("{}/v1/embeddings", server.url);
        let input = request(url.clone());
        let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body, input.headers, operation).await;
        assert!(matches!(baseline, Err("send" | "response-constructor")), "status {status}: {baseline:?}");
        server.captured().await;
        let actual = owner_request(request(url), limits()).await;
        assert!(matches!(actual, Err(Failure::Transport { .. })), "status {status}: {actual:?}");
        server.captured().await;
    }
}

#[tokio::test]
async fn null_bodies_and_disabled_codings_reach_real_provider_json_failure() {
    let mut plans: Vec<(&str, Reply, Vec<u8>)> = [204, 205, 304].into_iter()
        .map(|status| ("null-body", Reply::plain(status, JSON), Vec::new())).collect();
    for (coding, name) in [(decode::Coding::Deflate, "deflate"), (decode::Coding::Brotli, "br"), (decode::Coding::Zstd, "zstd")] {
        let bytes = decode::tests::encode(coding, JSON);
        plans.push((name, Reply::encoded(bytes.clone(), name), bytes));
    }
    for (name, reply, expected_bytes) in plans {
        let mut server = Loopback::new(vec![reply.clone(), reply]).await;
        let url = format!("{}/v1/embeddings", server.url);
        let input = request(url.clone()); let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body, input.headers, operation).await.unwrap();
        server.captured().await;
        assert_eq!(baseline.body, expected_bytes, "{name} native bytes");
        let (operation, budget, slots) = self::operation();
        let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
        let (mut service, input) = provider_request(&url, &budget);
        let reply = executor.execute(input).await.unwrap();
        assert_eq!(reply.body.as_ref().unwrap(), &baseline.body, "{name} owner bytes");
        let result = executor.resume(reply, |reply| service.resume(reply, &budget)).unwrap();
        assert!(matches!(result, Err(Failure::Decode { .. })), "{name} must fail JSON decoding");
        server.captured().await;
        drop(result); drop(service); drop(executor);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn partially_truncated_gzip_footer_matches_actual_plugin_error() {
    let mut gzip = decode::tests::encode(decode::Coding::Gzip, JSON);
    gzip.truncate(gzip.len() - 3);
    let reply = Reply::encoded(gzip, "gzip");
    let mut server = Loopback::new(vec![reply.clone(), reply]).await;
    let url = format!("{}/v1/embeddings", server.url);
    let input = request(url.clone()); let (operation, _, _) = operation();
    assert!(matches!(native_plugin(url.clone(), input.body, input.headers, operation).await, Err("read")));
    server.captured().await;
    assert!(matches!(owner_request(request(url), limits()).await, Err(Failure::Transport { .. })));
    server.captured().await;
}

#[tokio::test]
async fn cross_origin_redirects_strip_credentials_and_preserve_referer() {
    for status in [302, 307] {
        let mut destination = Loopback::new(vec![Reply::plain(200, JSON), Reply::plain(200, JSON)]).await;
        let mut redirect = Reply::plain(status, b"");
        redirect.headers.push(("Location".into(), format!("{}/destination?q=synthetic", destination.url).into_bytes()));
        let mut source = Loopback::new(vec![redirect.clone(), redirect]).await;
        let url = format!("{}/start?q=original", source.url);
        let mut input = request(url.clone());
        input.headers.push(("Authorization".into(), "Bearer synthetic-secret".into()));
        let original_body = input.body.clone();
        let mut baseline_headers = input.headers.clone(); baseline_headers.push(("Origin".into(), ORIGIN.into()));
        let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body, baseline_headers, operation).await.unwrap();
        let first = source.captured().await; let redirected = destination.captured().await;
        assert_eq!(first.headers["authorization"], ["Bearer synthetic-secret"]);
        assert!(!redirected.headers.contains_key("authorization"));
        assert_eq!(redirected.headers["referer"], [url.clone()]);
        assert_eq!(redirected.method, if status == 302 { "GET" } else { "POST" });
        assert_eq!(redirected.body, if status == 302 { Vec::new() } else { original_body });
        let mut input = request(url);
        input.headers.push(("Authorization".into(), "Bearer synthetic-secret".into()));
        assert_eq!(owner_request(input, limits()).await.unwrap(), baseline);
        assert_eq!(source.captured().await, first);
        assert_eq!(destination.captured().await, redirected);
    }
}

#[tokio::test]
async fn malformed_locations_and_redirect_hop_limit_match_actual_plugin() {
    let mut malformed = Reply::plain(302, JSON);
    malformed.headers.push(("Location".into(), b"http://[:::1".to_vec()));
    let mut server = Loopback::new(vec![malformed.clone(), malformed]).await;
    let url = format!("{}/start", server.url);
    let input = request(url.clone()); let (operation, _, _) = operation();
    let baseline = native_plugin(url.clone(), input.body, input.headers, operation).await.unwrap();
    server.captured().await;
    assert_eq!(baseline, WireReply { status: 302, body: JSON.to_vec() });
    assert_eq!(owner_request(request(url), limits()).await.unwrap(), baseline);
    server.captured().await;

    for hops in [9, 10, 11] {
        let mut observations = Vec::new();
        for plugin in [true, false] {
            let mut redirect = Reply::plain(307, b"");
            redirect.headers.push(("Location".into(), b"/next".to_vec()));
            let mut replies = vec![redirect; hops]; replies.push(Reply::plain(200, JSON));
            let mut server = Loopback::new(replies).await;
            let input = request(format!("{}/start", server.url));
            let success = if plugin {
                let (operation, _, _) = self::operation();
                match native_plugin(input.url, input.body, input.headers, operation).await {
                    Ok(reply) => { assert_eq!(reply, WireReply { status: 200, body: JSON.to_vec() }); true }
                    Err(error) => { assert_eq!(error, "send"); false }
                }
            } else {
                match owner_request(input, limits()).await {
                    Ok(reply) => { assert_eq!(reply, WireReply { status: 200, body: JSON.to_vec() }); true }
                    Err(error) => { assert!(matches!(error, Failure::Transport { .. })); false }
                }
            };
            let mut count = 0;
            while let Ok(wire) = server.requests.try_recv() {
                assert_eq!(wire.method, "POST"); assert_eq!(wire.body, br#"{"input":"synthetic"}"#);
                count += 1;
            }
            assert!(count > 0 && count <= 11);
            observations.push((success, count));
        }
        assert_eq!(observations[0], observations[1], "{hops} redirect attempts");
        assert_eq!(observations[0].0, hops <= 10, "native ten-hop boundary");
    }
}

#[tokio::test]
async fn aggregate_request_and_response_header_limits_fail_at_their_own_boundary() {
    for response_limit in [false, true] {
        for generous in [false, true] {
            let mut reply = Reply::plain(200, JSON);
            if response_limit {
                reply.headers.push(("X-Synthetic-One".into(), vec![b'x'; 160]));
                reply.headers.push(("X-Synthetic-Two".into(), vec![b'y'; 160]));
                assert!(reply.headers.iter().all(|(name, value)| name.len() + value.len() < 256));
                assert!(reply.headers.iter().map(|(name, value)| name.len() + value.len()).sum::<usize>() > 256);
            }
            let mut server = Loopback::new(vec![reply]).await;
            let (operation, budget, slots) = operation();
            let mut configured = limits(); configured.header_bytes = if generous { 8192 } else { 256 };
            let mut executor = Executor::new(operation, configured, ORIGIN.into()).unwrap();
            let mut input = request(format!("{}/v1/embeddings", server.url));
            if !response_limit {
                input.headers.push(("Authorization".into(), format!("Bearer {}", "x".repeat(224))));
                assert!(input.headers.iter().all(|(name, value)| name.len() + value.len() < 256));
                assert!(input.headers.iter().map(|(name, value)| name.len() + value.len()).sum::<usize>() > 256);
            }
            let result = executor.execute(input).await;
            if generous { let reply = result.unwrap(); executor.resume(reply, drop).unwrap(); }
            else { assert!(matches!(result, Err(Failure::Limit("native-http-header-bytes")))); }
            if response_limit || generous { server.captured().await; }
            else { assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty))); }
            drop(executor);
            assert_eq!(slots.available_permits(), 1);
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }
}

#[tokio::test]
async fn native_total_decoded_and_gzip_window_caps_have_generous_controls() {
    for total_cap in [true, false] {
        for generous in [false, true] {
            let mut server = Loopback::new(vec![Reply::encoded(decode::tests::encode(decode::Coding::Gzip, JSON), "gzip")]).await;
            let (operation, budget, slots) = operation();
            let mut configured = limits();
            if total_cap { configured.decode.max_total_decoded_bytes = if generous { 4096 } else { 8 }; }
            else { configured.decode.max_window_sum = if generous { 32768 } else { 32767 }; }
            let mut executor = Executor::new(operation, configured, ORIGIN.into()).unwrap();
            let result = executor.execute(request(format!("{}/v1/embeddings", server.url))).await;
            if generous {
                executor.resume(result.unwrap(), |reply| { let HostReply::Http { body, .. } = reply else { panic!() }; assert_eq!(body, JSON); }).unwrap();
            } else {
                let expected = if total_cap { "total-decoded-bytes" } else { "codec-windows" };
                assert!(matches!(result, Err(Failure::Limit(kind)) if kind == expected));
                assert_eq!(executor.phase, Phase::Terminal);
                assert!(executor.decoder.pipeline.is_none());
            }
            server.captured().await; drop(executor);
            assert_eq!(slots.available_permits(), 1);
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }
}

#[test]
fn payload_and_native_transfer_admission_preserve_storage_and_release_failed_moves() {
    for header in [false, true] {
        for generous in [false, true] {
            let (operation, budget, slots) = operation();
            let mut input = String::with_capacity(128); input.push_str("Bearer synthetic");
            let base = budget.owned_bytes();
            let headroom = input.capacity() + if generous { 4096 } else { 0 };
            let held = budget.limits().max_owned_bytes - base - headroom;
            let caller = budget.reserve_owned(held).unwrap();
            let pointer = input.as_ptr();
            let result = if header { ownership::header_payload(input, &operation) }
                else { let bytes = input.into_bytes(); let length = bytes.len(); ownership::payload(bytes, 0, length, &operation) };
            if generous { let output = result.as_ref().unwrap(); assert_eq!(output.as_ptr(), pointer); assert_eq!(output.as_ref(), b"Bearer synthetic"); }
            else { assert!(matches!(result, Err(Failure::Limit("owned-bytes")))); }
            drop(result);
            assert_eq!(budget.owned_bytes(), base + held);
            drop(caller); drop(operation);
            assert_eq!(slots.available_permits(), 1);
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }
    for generous in [false, true] {
        let (operation, budget, slots) = operation();
        let base = budget.owned_bytes();
        let mut input = OwnedVec::new(&budget).unwrap();
        let mut pointers = Vec::new();
        for _ in 0..64 {
            let mut vector = OwnedVec::new(&budget).unwrap(); vector.push(1.00000001).unwrap();
            pointers.push(vector.as_slice().as_ptr()); input.push(Some(vector)).unwrap();
        }
        let headroom = if generous { 3 * 64 * size_of::<Option<EmbeddingVector>>() } else { size_of::<Option<EmbeddingVector>>() };
        let held = budget.limits().max_owned_bytes - budget.owned_bytes() - headroom;
        let caller = budget.reserve_owned(held).unwrap();
        let result = Adapter::<fn(&Budget) -> Result<CapturedConfig, Failure>>::transfer(input, &budget);
        if generous {
            let output = result.as_ref().unwrap(); assert_eq!(output.as_slice().len(), pointers.len());
            for (row, pointer) in output.as_slice().iter().zip(pointers) {
                let Some(EmbeddingVector::F64(values)) = row else { panic!() };
                assert_eq!(values.as_slice().as_ptr(), pointer); assert_eq!(values.as_slice(), [1.00000001]);
            }
        } else { assert!(matches!(result, Err(StageError::Limit(ref kind)) if kind == "owned-bytes")); }
        drop(result);
        assert_eq!(budget.owned_bytes(), base + held);
        drop(caller); drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn collector_active_cancellation_and_caller_drop_release_decode_state() {
    for abandon in [false, true] {
        let mut bytes = decode::tests::encode(decode::Coding::Gzip, JSON); bytes.truncate(bytes.len() - 8);
        let mut reply = Reply::encoded(bytes, "gzip"); reply.held = true;
        let mut server = Loopback::new(vec![reply]).await;
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let progress = Arc::new(Notify::new()); let observed = progress.clone();
        let url = format!("{}/v1/embeddings", server.url);
        let job = tokio::spawn(async move {
            let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
            executor.decoder.body_progress = Some(observed);
            executor.execute(request(url)).await
        });
        tokio::time::timeout(Duration::from_secs(5), progress.notified()).await.expect("collector never appended a real body chunk");
        server.captured().await;
        assert!(budget.owned_bytes() >= Budget::context_bytes() + limits().decode_owned);
        assert_eq!(slots.available_permits(), 0);
        if abandon {
            job.abort(); assert!(matches!(job.await, Err(error) if error.is_cancelled()));
            assert!(cancel.is_cancelled()); assert_eq!(cancel.origin(), decode::CancelOrigin::Owner);
        } else {
            cancel.cancel();
            assert!(matches!(tokio::time::timeout(Duration::from_secs(5), job).await.unwrap().unwrap(), Err(Failure::Cancelled)));
        }
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn cancellation_after_real_provider_resume_discards_completed_vectors() {
    let mut server = Loopback::new(vec![Reply::plain(200, JSON)]).await;
    let (operation, budget, slots) = operation(); let cancel = operation.cancel().clone();
    let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
    let (mut service, input) = provider_request(&format!("{}/v1/embeddings", server.url), &budget);
    let reply = executor.execute(input).await.unwrap(); server.captured().await;
    let result = executor.resume(reply, |reply| {
        let output = service.resume(reply, &budget).unwrap();
        assert!(matches!(output, Step::Done(_)));
        cancel.cancel();
        output
    });
    assert!(matches!(result, Err(Failure::Cancelled)));
    drop(result); drop(service); drop(executor);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[test]
fn cancellation_before_operation_admission_keeps_the_slot_free() {
    let cancel = Cancellation::new();
    let budget = Budget::new(risunest_ir::Limits { max_owned_bytes: 4096, max_json_bytes: 1024,
        max_text_units: 1024, max_values: 128, max_depth: 16, max_work: 4096 }, cancel.flag()).unwrap();
    let slots = Arc::new(Semaphore::new(1)); cancel.cancel();
    assert!(matches!(Operation::admit(slots.clone(), budget.clone(), cancel), Err(Failure::Cancelled)));
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn fragmented_identity_and_gzip_wait_for_real_intake_before_remainder() {
    for gzip in [false, true] {
        let bytes = if gzip { decode::tests::encode(decode::Coding::Gzip, JSON) } else { JSON.to_vec() };
        let split = if gzip { bytes.len() - 8 } else { 1 };
        let mut reply = if gzip { Reply::encoded(bytes.clone(), "gzip") } else { Reply::plain(200, &bytes) };
        reply.chunks = vec![bytes[..split].to_vec(), bytes[split..].to_vec()];
        let release = Arc::new(Notify::new());
        let mut paused = reply.clone(); paused.pause_after_first = Some(release.clone());
        let mut server = Loopback::new(vec![reply, paused]).await;
        let url = format!("{}/v1/embeddings", server.url);
        let input = request(url.clone()); let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body, input.headers, operation).await.unwrap();
        server.captured().await;
        assert_eq!(baseline.body, JSON);
        let (operation, budget, slots) = self::operation();
        let progress = Arc::new(Notify::new()); let observed = progress.clone();
        let job = tokio::spawn(async move {
            let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
            executor.decoder.body_progress = Some(observed);
            let reply = executor.execute(request(url)).await.unwrap();
            executor.resume(reply, |reply| {
                let HostReply::Http { status, body } = reply else { panic!() };
                WireReply { status, body }
            }).unwrap()
        });
        tokio::time::timeout(Duration::from_secs(5), progress.notified()).await.expect("first fragment was not decoded before remainder release");
        server.captured().await;
        assert!(!job.is_finished(), "owner completed before the held remainder");
        assert_eq!(slots.available_permits(), 0);
        release.notify_one();
        let actual = tokio::time::timeout(Duration::from_secs(5), job).await.unwrap().unwrap();
        assert_eq!(actual, baseline);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}
