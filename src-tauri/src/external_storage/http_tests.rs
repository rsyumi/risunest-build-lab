use super::{
    contract::*,
    http::*,
    quota::AccountKey,
    wire_fixture::{Reply, WireServer},
};
use std::{collections::BTreeMap, time::{Duration, Instant}};
use tokio::io::AsyncReadExt;
fn request(server: &WireServer, body: Option<Vec<u8>>) -> HttpRequest {
    let length = body.as_ref().map(|body| body.len() as u64);
    HttpRequest {
        method: if body.is_some() {
            reqwest::Method::PUT
        } else {
            reqwest::Method::GET
        },
        url: server.url.clone(),
        headers: BTreeMap::new(),
        body: body.map(|body| Box::pin(std::io::Cursor::new(body)) as _),
        content_length: length,
        operation: ProviderOperation::ReplaceHead,
        account: AccountKey::new("webdav", &server.url, "synthetic-account").unwrap(),
        api_request: true,
        mybox_charge: None,
        control: true,
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
#[test]
fn wire_streams_exact_bytes_and_exposes_status_headers_without_redirects_or_retries() {
    runtime().block_on(async {
        let data = vec![42; 1024 * 1024];
        let server = WireServer::start(vec![
            Reply::Http {
                status: 200,
                headers: vec![
                    ("ETag".into(), "\"version-1\"".into()),
                    ("Content-Encoding".into(), "gzip".into()),
                ],
                body: data.clone(),
            },
            Reply::Http {
                status: 307,
                headers: vec![("Location".into(), "http://127.0.0.1:1/do-not-follow".into())],
                body: vec![],
            },
            Reply::Http {
                status: 429,
                headers: vec![("Retry-After".into(), "60".into())],
                body: vec![],
            },
            Reply::Lost,
        ]);
        let transport = NativeHttpTransport::for_loopback_tests();
        let cancel = Cancellation::default();
        let mut response = transport
            .send(request(&server, Some(data.clone())), &cancel)
            .await
            .unwrap();
        assert_eq!(response.headers["etag"], "\"version-1\"");
        let mut output = Vec::new();
        response.body.read_to_end(&mut output).await.unwrap();
        assert_eq!(output, data);
        let response = transport
            .send(request(&server, None), &cancel)
            .await
            .unwrap();
        assert_eq!(response.status, 307);
        let response = transport
            .send(request(&server, None), &cancel)
            .await
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(response.headers["retry-after"], "60");
        assert!(transport
            .send(request(&server, Some(vec![9; 10])), &cancel)
            .await
            .is_err());
        let records = server.requests.lock().unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].body, data);
        assert!(records[0].headers.starts_with("PUT /synthetic HTTP/1.1"));
    });
}
#[test]
fn cancellation_wakes_header_and_body_waits_and_all_listeners() {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    struct Wake(AtomicBool);
    impl futures::task::ArcWake for Wake {
        fn wake_by_ref(wake: &Arc<Self>) {
            wake.0.store(true, Ordering::Release);
        }
    }
    fn pending<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> Arc<Wake> {
        let wake = Arc::new(Wake(AtomicBool::new(false)));
        let waker = futures::task::waker(wake.clone());
        assert!(future.poll(&mut std::task::Context::from_waker(&waker)).is_pending());
        wake
    }
    runtime().block_on(async {
        let shared = Cancellation::default();
        let (first, second) = (shared.cancelled(), shared.cancelled());
        tokio::pin!(first, second);
        let first_wake = pending(first.as_mut());
        let second_wake = pending(second.as_mut());
        shared.cancel();
        assert!(first_wake.0.load(Ordering::Acquire));
        assert!(second_wake.0.load(Ordering::Acquire));
        assert!(futures::poll!(first.as_mut()).is_ready());
        assert!(futures::poll!(second.as_mut()).is_ready());

        for send_headers in [false, true] {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel();
            let server = WireServer::start(vec![Reply::Gated {
                send_headers,
                ready: ready_tx,
                release: released,
            }]);
            let transport = NativeHttpTransport::for_loopback_tests();
            let cancel = Cancellation::default();
            let outcome = tokio::time::timeout(Duration::from_secs(10), async {
                let send = transport.send(request(&server, None), &cancel);
                tokio::pin!(send);
                if send_headers {
                    let mut response = send.await.unwrap();
                    ready_rx.await.unwrap();
                    let mut byte = [0];
                    {
                        let read = response.body.read(&mut byte);
                        tokio::pin!(read);
                        let wake = pending(read.as_mut());
                        cancel.cancel();
                        assert!(wake.0.load(Ordering::Acquire));
                        assert_eq!(read.await.unwrap_err().kind(), std::io::ErrorKind::Other);
                    }
                    assert_eq!(
                        response.body.read(&mut byte).await.unwrap_err().kind(),
                        std::io::ErrorKind::Other,
                    );
                } else {
                    tokio::select! {
                        ready = ready_rx => ready.unwrap(),
                        _ = &mut send => panic!("header wait ended before cancellation"),
                    }
                    let wake = pending(send.as_mut());
                    cancel.cancel();
                    assert!(wake.0.load(Ordering::Acquire));
                    assert_eq!(send.await.err().unwrap().kind, ErrorKind::Cancelled);
                }
                futures::future::join(cancel.cancelled(), cancel.cancelled()).await;
            }).await;
            drop(release);
            drop(server);
            outcome.expect("cancellation did not wake a ready network wait");
        }
    });
}

#[test]
fn mybox_quota_denies_wire_dispatch_after_reopen() {
    use super::{
        durable_quota::MyboxBudget,
        quota_profiles::{MyboxCharge, MyboxCounter, MyboxPlan},
    };
    struct Time;
    impl Clock for Time {
        fn now_ms(&self) -> u64 {
            10
        }
    }
    runtime().block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("quota.sqlite");
        let budget = MyboxBudget::new(path.clone());
        let server = WireServer::start(vec![Reply::Lost]);
        let account = AccountKey::new("mybox", &server.url, "synthetic-account").unwrap();
        let charge = MyboxCharge {
            plan: MyboxPlan::Plan30gb,
            counters: vec![MyboxCounter::ListMinute],
        };
        for _ in 0..9 {
            budget.reserve(&account, &charge, 10).unwrap();
        }
        let transport = NativeHttpTransport::for_loopback_tests();
        let cancel = Cancellation::default();
        let make = || {
            let mut request = request(&server, Some(vec![1]));
            request.account = account.clone();
            request.mybox_charge = Some(charge.clone());
            request
        };
        let first_state = RequestState::default();
        assert_eq!(
            send(&transport, &budget, &Time, &first_state, make(), &cancel)
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::Transient
        );
        drop(budget);
        let reopened = MyboxBudget::new(path);
        let reopened_state = RequestState::default();
        assert_eq!(
            send(&transport, &reopened, &Time, &reopened_state, make(), &cancel)
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::RateLimited
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    });
}

#[test]
fn account_wait_stops_a_control_request_before_dispatch() {
    use super::fake::RecordingBudget;
    struct Time;
    impl Clock for Time {
        fn now_ms(&self) -> u64 {
            10
        }
    }
    runtime().block_on(async {
        let server = WireServer::start(Vec::new());
        let transport = NativeHttpTransport::for_loopback_tests();
        let budget = RecordingBudget::default();
        let state = RequestState::default();
        let account = AccountKey::new("s3", &server.url, "synthetic-account").unwrap();
        state
            .backoff
            .failure(
                &account,
                ErrorKind::RateLimited,
                Some(Duration::from_secs(120)),
                Instant::now(),
            )
            .unwrap();
        let mut blocked = request(&server, None);
        blocked.account = account;
        assert_eq!(
            send(&transport, &budget, &Time, &state, blocked, &Cancellation::default())
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::RateLimited
        );
        assert!(server.requests.lock().unwrap().is_empty());
        assert!(budget.reservations.lock().unwrap().is_empty());
    });
}
