use super::*;
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[cfg(windows)]
mod desktop_probe;
#[cfg(windows)]
mod redirect_probe;

fn fixture(size: u64) -> (TempDir, Files, String, PathBuf) {
    let root = TempDir::new().unwrap();
    let zeroes = vec![0; CHUNK_BYTES];
    let mut remaining = size;
    let mut digest = Sha256::new();
    while remaining > 0 {
        let count = remaining.min(zeroes.len() as u64) as usize;
        digest.update(&zeroes[..count]);
        remaining -= count as u64;
    }
    let hash = hex::encode(digest.finalize());
    let physical_key = format!("assets/objects/{}/{}", &hash[..2], &hash[2..]);
    let payload = root.path().join(&physical_key);
    fs::create_dir_all(payload.parent().unwrap()).unwrap();
    fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&payload)
        .unwrap()
        .set_len(size)
        .unwrap();
    let state = Files {
        root: root.path().to_path_buf(),
        authority: "127.0.0.1:12345".into(),
        prefix: "/0123456789abcdef0123456789abcdef/".into(),
        slots: Arc::new(Semaphore::new(MAX_TRANSFERS)),
        remote: Arc::new(
            MediaProvider::new(root.path().to_path_buf(), "http://127.0.0.1:12345".into()).unwrap(),
        ),
    };
    let path = format!(
        "{}{}?mime=application%2Foctet-stream&size={size}",
        state.prefix,
        hex::encode(physical_key),
    );
    (root, state, path, payload)
}

fn request(state: &Files, path: &str, method: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, &state.authority)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn full_file_stream_is_bounded_and_byte_exact() {
    let size = 64 * 1024 * 1024 + 17;
    let (_root, state, path, _) = fixture(size);
    let response = serve(State(state.clone()), request(&state, &path, "GET")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], size.to_string());
    assert_eq!(state.slots.available_permits(), MAX_TRANSFERS - 1);
    let mut stream = response.into_body().into_data_stream();
    let mut received = 0;
    let mut chunks = 0;
    while let Some(bytes) = stream.next().await {
        let bytes = bytes.unwrap();
        assert!(bytes.len() <= CHUNK_BYTES);
        assert!(bytes.iter().all(|byte| *byte == 0));
        received += bytes.len() as u64;
        chunks += 1;
    }
    assert_eq!(received, size);
    assert!(chunks >= size.div_ceil(CHUNK_BYTES as u64));
    assert_eq!(state.slots.available_permits(), MAX_TRANSFERS);
}

#[tokio::test]
async fn cancelled_transfer_releases_slot_and_open_handle_survives_cleanup() {
    let (_root, state, path, payload) = fixture(1024 * 1024);
    let response = serve(State(state.clone()), request(&state, &path, "GET")).await;
    fs::remove_file(payload).unwrap();
    let mut stream = response.into_body().into_data_stream();
    assert_eq!(stream.next().await.unwrap().unwrap().len(), CHUNK_BYTES);
    drop(stream);
    assert_eq!(state.slots.available_permits(), MAX_TRANSFERS);
}

#[tokio::test]
async fn rejects_wrong_host_capability_paths_and_methods() {
    let (_root, state, path, _) = fixture(10);
    for path in [
        "/wrong/asset".to_owned(),
        format!("{}{}", state.prefix, hex::encode("../secret")),
    ] {
        let response = serve(State(state.clone()), request(&state, &path, "GET")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let mut wrong_host = request(&state, &path, "GET");
    wrong_host
        .headers_mut()
        .insert(header::HOST, "attacker.invalid:12345".parse().unwrap());
    assert_eq!(
        serve(State(state.clone()), wrong_host).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        serve(State(state.clone()), request(&state, &path, "POST"))
            .await
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(state.slots.available_permits(), MAX_TRANSFERS);
}

#[tokio::test]
async fn head_and_ranges_keep_headers_and_exact_lengths() {
    let (_root, state, path, _) = fixture(10);
    let head = serve(State(state.clone()), request(&state, &path, "HEAD")).await;
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
    assert!(head.into_body().into_data_stream().next().await.is_none());
    for (range, expected, length, status) in [
        ("bytes=2-5", "bytes 2-5/10", 4, StatusCode::PARTIAL_CONTENT),
        ("bytes=-3", "bytes 7-9/10", 3, StatusCode::PARTIAL_CONTENT),
        ("bytes=7-", "bytes 7-9/10", 3, StatusCode::PARTIAL_CONTENT),
        (
            "bytes=10-",
            "bytes */10",
            0,
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
    ] {
        let mut req = request(&state, &path, "GET");
        req.headers_mut()
            .insert(header::RANGE, range.parse().unwrap());
        let response = serve(State(state.clone()), req).await;
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CONTENT_RANGE], expected);
        let bytes = axum::body::to_bytes(response.into_body(), 10)
            .await
            .unwrap();
        assert_eq!(bytes.len(), length);
    }
}

#[tokio::test]
async fn if_range_with_a_weak_or_changed_validator_returns_the_full_file() {
    let (_root, state, path, _) = fixture(10);
    let first = serve(State(state.clone()), request(&state, &path, "HEAD")).await;
    for tag in [
        format!("W/{}", first.headers()[header::ETAG].to_str().unwrap())
            .parse()
            .unwrap(),
        "\"changed\"".parse().unwrap(),
    ] {
        let mut req = request(&state, &path, "GET");
        req.headers_mut()
            .insert(header::RANGE, "bytes=3-5".parse().unwrap());
        req.headers_mut().insert(header::IF_RANGE, tag);
        let response = serve(State(state.clone()), req).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 10)
                .await
                .unwrap()
                .len(),
            10
        );
    }
}

#[tokio::test]
async fn simultaneous_transfers_wait_for_a_cancelled_slot() {
    let (_root, state, path, _) = fixture(1024 * 1024);
    let mut responses = Vec::new();
    for _ in 0..MAX_TRANSFERS {
        responses.push(serve(State(state.clone()), request(&state, &path, "GET")).await);
    }
    let mut pending = Box::pin(serve(State(state.clone()), request(&state, &path, "GET")));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut pending)
            .await
            .is_err()
    );
    responses.pop();
    let admitted = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .unwrap();
    assert_eq!(admitted.status(), StatusCode::OK);
    drop(admitted);
    drop(responses);
    assert_eq!(state.slots.available_permits(), MAX_TRANSFERS);
}

#[tokio::test]
async fn actual_loopback_http_serves_only_capability_url() {
    let (root, state, path, _) = fixture(100);
    let server = MediaServer::start(root.path().to_path_buf()).unwrap();
    let client = reqwest::Client::new();
    let capability = path.strip_prefix(&state.prefix).unwrap();
    let url = format!("{}{}", server.base_url, capability);
    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), &[0; 100]);
    let without_token = url::Url::parse(&url).unwrap().join("/missing").unwrap();
    assert_eq!(
        client
            .get(without_token)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
}

#[test]
fn cleanup_media_waits_for_owned_transfers_and_revokes_the_previous_capability() {
    let root = TempDir::new().unwrap();
    let state = MediaServerState::initialize(root.path().to_owned());
    let (previous, slots) = {
        let server = state.0.lock().unwrap();
        let server = server.as_ref().unwrap();
        (server.base_url.clone(), server.slots.clone())
    };
    let transfer = slots.clone().try_acquire_owned().unwrap();
    state.begin_cleanup().unwrap();
    assert!(slots.try_acquire_owned().is_err());
    assert!(!state.cleanup_drained().unwrap());
    drop(transfer);
    assert!(state.cleanup_drained().unwrap());
    assert!(state.0.lock().unwrap().is_err());
    state.reopen_after_cleanup(root.path().to_owned()).unwrap();
    assert_ne!(state.0.lock().unwrap().as_ref().unwrap().base_url, previous);
}
