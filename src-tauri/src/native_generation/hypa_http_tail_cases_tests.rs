use super::*;
use std::io;

fn framed_response(frames: Vec<Result<Bytes, io::Error>>) -> Response {
    let body = reqwest::Body::wrap_stream(futures::stream::iter(frames));
    tauri::http::Response::new(body).into()
}

fn synthetic_source(response: Response, operation: &Operation, budget: &Budget, maximum: u64) -> HttpSource {
    let charge = budget.reserve_owned(size_of::<HttpSource>() + size_of::<Ready<PullResult>>()).unwrap();
    HttpSource { response, pending: None, pending_charge: None, position: 0, received: 0, maximum,
        tail_pending: None, _operation: operation.clone(), _charge: charge }
}

#[tokio::test]
async fn same_raw_frame_remainder_spanning_intake_quanta_is_ignored() {
    for payload in [JSON, b"".as_slice()] {
        let (operation, budget, slots) = operation();
        let mut configured = limits(); configured.decode.input_quantum = 7; configured.decode.output_quantum = 1;
        let mut first = decode::tests::encode(decode::Coding::Gzip, payload);
        first.extend(vec![b'x'; 3 * limits().decode.input_quantum]);
        first.extend(decode::tests::encode(decode::Coding::Gzip, b"ignored second member"));
        let mut decoder = DecoderOwner::new(&operation, configured).unwrap();
        let output = decoder.collect(framed_response(vec![Ok(Bytes::from(first))]), true, &operation, configured).await.unwrap();
        assert_eq!(output.bytes(), payload);
        assert_eq!(decoder.budget.as_ref().unwrap().owned_bytes(), Budget::context_bytes());
        drop(output); drop(decoder); drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn deterministic_later_data_errors_and_empty_frame_terminates_before_poison() {
    for kind in ["eof", "data", "member", "error", "empty-before-error"] {
        let gzip = decode::tests::encode(decode::Coding::Gzip, JSON);
        let mut frames = vec![Ok(Bytes::from(gzip))];
        match kind {
            "data" => frames.push(Ok(Bytes::from_static(b"later data"))),
            "member" => frames.push(Ok(Bytes::from(decode::tests::encode(decode::Coding::Gzip, JSON)))),
            "error" => frames.push(Err(io::Error::other("synthetic late body error"))),
            "empty-before-error" => {
                frames.push(Ok(Bytes::new()));
                frames.push(Err(io::Error::other("must remain unread after empty DATA")));
            }
            _ => {}
        }
        let (operation, budget, slots) = operation();
        let mut decoder = DecoderOwner::new(&operation, limits()).unwrap();
        let result = decoder.collect(framed_response(frames), true, &operation, limits()).await;
        if matches!(kind, "eof" | "empty-before-error") { assert_eq!(result.as_ref().unwrap().bytes(), JSON, "{kind}"); }
        else { assert!(matches!(&result, Err(Failure::Invalid("native-http-decode"))), "{kind}"); }
        drop(result);
        assert!(decoder.pipeline.is_none());
        assert_eq!(decoder.budget.as_ref().unwrap().owned_bytes(), Budget::context_bytes());
        drop(decoder); drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn later_frame_encoded_limit_has_priority_and_releases_observed_storage() {
    let gzip = decode::tests::encode(decode::Coding::Gzip, JSON);
    let (operation, budget, slots) = operation();
    let mut configured = limits(); configured.decode.max_encoded_bytes = gzip.len() as u64 + 3;
    let mut decoder = DecoderOwner::new(&operation, configured).unwrap();
    let response = framed_response(vec![Ok(Bytes::from(gzip)), Ok(Bytes::from_static(b"four"))]);
    assert!(matches!(decoder.collect(response, true, &operation, configured).await, Err(Failure::Limit("encoded-bytes"))));
    assert_eq!(decoder.budget.as_ref().unwrap().owned_bytes(), Budget::context_bytes());
    drop(decoder); drop(operation);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn finish_poll_work_is_cumulative_while_the_real_body_is_pending() {
    let (operation, budget, slots) = operation();
    let mut configured = limits(); configured.decode_work = 2;
    let decoder = DecoderOwner::new(&operation, configured).unwrap();
    let private = decoder.budget.as_ref().unwrap();
    let response = tauri::http::Response::new(reqwest::Body::wrap_stream(futures::stream::pending::<Result<Bytes, io::Error>>())).into();
    let mut source = synthetic_source(response, &operation, private, configured.decode.max_encoded_bytes);
    for _ in 0..2 {
        assert!(std::future::poll_fn(|cx| Poll::Ready(source.poll_native_finish(cx, operation.cancel(), private))).await.is_pending());
    }
    let exhausted = std::future::poll_fn(|cx| Poll::Ready(source.poll_native_finish(cx, operation.cancel(), private))).await;
    assert!(matches!(exhausted, Poll::Ready(Err(DecodeError::Limit("work")))));
    drop(source);
    assert_eq!(private.owned_bytes(), Budget::context_bytes());
    drop(decoder); drop(operation);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn cancellation_before_ready_tail_keeps_the_last_chunk_leased_until_drop() {
    let (operation, budget, slots) = operation();
    let decoder = DecoderOwner::new(&operation, limits()).unwrap();
    let private = decoder.budget.as_ref().unwrap();
    let response = framed_response(vec![Ok(Bytes::from(decode::tests::encode(decode::Coding::Gzip, JSON)))]);
    let source = synthetic_source(response, &operation, private, limits().decode.max_encoded_bytes);
    let mut pipeline = Pipeline::native_plugin(Box::new(source), true, limits().decode, private.clone()).unwrap();
    let part = pipeline.pull(operation.cancel()).await.unwrap().unwrap();
    assert_eq!(part.bytes(), JSON);
    operation.cancel().cancel();
    assert!(matches!(pipeline.pull(operation.cancel()).await, Err(DecodeError::Cancelled)));
    drop(pipeline);
    assert!(private.owned_bytes() > Budget::context_bytes());
    assert!(budget.owned_bytes() >= Budget::context_bytes() + limits().decode_owned);
    assert_eq!(slots.available_permits(), 0);
    drop(part);
    assert_eq!(private.owned_bytes(), Budget::context_bytes());
    drop(decoder); drop(operation);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

fn held_gzip(tail: Vec<u8>) -> Reply {
    let mut reply = Reply::encoded(decode::tests::encode(decode::Coding::Gzip, JSON), "gzip");
    reply.held = true; reply.tail_after_release = tail; reply
}

async fn actual_plugin_tail(reply: Reply) -> Result<WireReply, &'static str> {
    let mut server = Loopback::new(vec![reply]).await;
    let url = format!("{}/v1/embeddings", server.url);
    let input = request(url.clone()); let (operation, _, _) = operation();
    let mut job = Box::pin(native_plugin(url, input.body, input.headers, operation));
    let early = tokio::select! {
        _ = server.captured() => None,
        result = &mut job => Some(result),
    };
    server.release.notify_one();
    match early { Some(result) => result, None => job.await }
}

#[tokio::test]
async fn held_member_tail_matches_actual_plugin_for_completion_trailers_and_failures() {
    let later_member = decode::tests::encode(decode::Coding::Gzip, JSON);
    let mut member_tail = format!("{:x}\r\n", later_member.len()).into_bytes();
    member_tail.extend(later_member); member_tail.extend_from_slice(b"\r\n0\r\n\r\n");
    let plans = [
        ("eof", b"0\r\n\r\n".to_vec(), true),
        ("trailers", b"0\r\nX-Synthetic-Trailer: accepted\r\n\r\n".to_vec(), true),
        ("incomplete-trailers", b"0\r\nX-Synthetic-Trailer: incomplete\r\n".to_vec(), false),
        ("later-data", b"4\r\nlate\r\n0\r\n\r\n".to_vec(), false),
        ("later-member", member_tail, false),
        ("transport-error", Vec::new(), false),
    ];
    for (name, tail, succeeds) in plans {
        let reply = held_gzip(tail);
        let baseline = actual_plugin_tail(reply.clone()).await;
        let mut server = Loopback::new(vec![reply]).await;
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let pending = Arc::new(Notify::new());
        let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
        executor.decoder.tail_pending = Some(pending.clone());
        let before = budget.owned_bytes();
        let mut job = Box::pin(executor.execute(request(format!("{}/v1/embeddings", server.url))));
        let mut early = None;
        let reached = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! { biased;
                _ = pending.notified() => true,
                result = &mut job => { early = Some(result); false },
            }
        }).await;
        let held_owned = budget.owned_bytes(); let held_slots = slots.available_permits();
        if reached.is_err() { cancel.cancel(); }
        server.release.notify_one();
        let result = match early { Some(result) => result, None => job.as_mut().await };
        drop(job);
        let actual = result.and_then(|reply| executor.resume(reply, |reply| {
            let HostReply::Http { status, body } = reply else { unreachable!() }; WireReply { status, body }
        }));
        drop(executor);
        assert!(matches!(reached, Ok(true)), "{name}: owner never reached a pending native tail");
        assert_eq!(held_slots, 0, "{name}");
        assert!(held_owned > before && held_owned >= Budget::context_bytes() + limits().decode_owned + JSON.len(), "{name}: pending receive/decode storage");
        assert_eq!(baseline.is_ok(), succeeds, "{name}: actual plugin");
        assert_eq!(actual.is_ok(), baseline.is_ok(), "{name}: private owner");
        if succeeds { assert_eq!(actual.as_ref().unwrap(), baseline.as_ref().unwrap(), "{name}"); }
        drop(actual); drop(baseline);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn pending_tail_cancellation_and_caller_drop_release_receive_decode_and_admission() {
    for abandon in [false, true] {
        let mut server = Loopback::new(vec![held_gzip(b"0\r\n\r\n".to_vec())]).await;
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let pending = Arc::new(Notify::new()); let observed = pending.clone();
        let url = format!("{}/v1/embeddings", server.url);
        let job = tokio::spawn(async move {
            let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
            executor.decoder.tail_pending = Some(observed);
            executor.execute(request(url)).await
        });
        let reached = tokio::time::timeout(Duration::from_secs(3), pending.notified()).await;
        let held_owned = budget.owned_bytes(); let held_slots = slots.available_permits();
        if abandon { job.abort(); }
        else { cancel.cancel(); }
        server.release.notify_one();
        let result = job.await;
        assert!(reached.is_ok(), "native tail never returned Pending");
        assert_eq!(held_slots, 0);
        assert!(held_owned >= Budget::context_bytes() + limits().decode_owned + JSON.len());
        if abandon {
            assert!(matches!(result, Err(error) if error.is_cancelled()));
            assert_eq!(cancel.origin(), decode::CancelOrigin::Owner);
        } else { assert!(matches!(result.unwrap(), Err(Failure::Cancelled))); }
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        server.captured().await;
    }
}

#[tokio::test]
async fn cancellation_raised_by_a_ready_tail_wins_over_eof_and_error() {
    for error in [false, true] {
        let (operation, budget, slots) = operation();
        let decoder = DecoderOwner::new(&operation, limits()).unwrap();
        let private = decoder.budget.as_ref().unwrap();
        let mut first = Some(Bytes::from(decode::tests::encode(decode::Coding::Gzip, JSON)));
        let on_ready = operation.cancel().clone();
        let stream = futures::stream::poll_fn(move |_cx| {
            if let Some(first) = first.take() { Poll::Ready(Some(Ok(first))) }
            else {
                on_ready.cancel();
                Poll::Ready(if error { Some(Err(io::Error::other("ready tail error"))) } else { None })
            }
        });
        let response = tauri::http::Response::new(reqwest::Body::wrap_stream(stream)).into();
        let source = synthetic_source(response, &operation, private, limits().decode.max_encoded_bytes);
        let mut pipeline = Pipeline::native_plugin(Box::new(source), true, limits().decode, private.clone()).unwrap();
        let part = pipeline.pull(operation.cancel()).await.unwrap().unwrap();
        assert_eq!(part.bytes(), JSON);
        assert!(!operation.cancel().is_cancelled(), "tail was polled before returning decoded output");
        assert!(matches!(pipeline.pull(operation.cancel()).await, Err(DecodeError::Cancelled)));
        drop(part); drop(pipeline);
        assert_eq!(private.owned_bytes(), Budget::context_bytes());
        drop(decoder); drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}
