use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
struct Trace {
    armed: AtomicBool,
    target: std::sync::Mutex<Option<(std::path::PathBuf, String)>>,
    requests: std::sync::Mutex<Vec<Vec<String>>>,
    renderer: crate::persistent_store::commands::PersistentStoreState,
}

fn fixture(trace: Arc<Trace>) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(root.path()).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2).enable_all().build().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let entered = runtime.enter();
    let router = http::router(server.clone()).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let trace = trace.clone();
            async move {
                if request.method() == axum::http::Method::POST
                    && request.uri().path() == "/objects/retention/release"
                {
                    let (parts, body) = request.into_parts();
                    let body = axum::body::to_bytes(body, risunest_sync_wire::MAX_METADATA_BYTES).await.unwrap();
                    let value: Value = serde_json::from_slice(&body).unwrap();
                    let hashes: Vec<String> = value["objects"].as_array().unwrap().iter()
                        .map(|object| object["hash"].as_str().unwrap().to_owned()).collect();
                    trace.requests.lock().unwrap().push(hashes.clone());
                    if trace.armed.swap(false, Ordering::SeqCst) {
                        let (root, target) = trace.target.lock().unwrap().clone().unwrap();
                        assert!(!hashes.contains(&target), "the new root belongs to a later page");
                        let _renderer = trace.renderer.admit_renderer_operation().unwrap();
                        let mut store = PersistentStore::open(&root).unwrap();
                        let revision = store.revision().unwrap();
                        store.commit(&WorkingSetCommit {
                            plugin_storage: Some(vec![PluginStorageMutation::Set {
                                owner: "synthetic-release-race".into(),
                                key: "later-page".into(),
                                value: json!(crate::asset_repository::object_physical_key(&target)),
                            }]),
                            ..empty_working_set_commit(revision)
                        }).unwrap();
                    }
                    return next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(body))).await;
                }
                next.run(request).await
            }
        },
    ));
    drop(entered);
    let task = runtime.spawn(async move { axum::serve(listener, router).await.unwrap() });
    Fixture { _server_root: root, server, runtime, task, endpoint }
}

#[test]
fn a_plugin_root_added_after_the_first_release_page_preserves_later_custody() {
    let trace = Arc::new(Trace::default());
    let fixture = fixture(trace.clone());
    let (_root, mut store) = prepared();
    fixture.bind(&mut store);
    assert_eq!(settle(&mut store).phase, "idle");
    let config = store.server_config().unwrap().unwrap();
    let stored = store.server_stored_config().unwrap().unwrap();
    let device = fixture.server.authenticate(&config.library_id, &config.token).unwrap();
    let client = crate::server_sync::client::ServerClient::new(config).unwrap();
    let head = fixture.server.head().unwrap();
    let mut objects = Vec::new();
    for index in 0..300 {
        let bytes = format!("synthetic-unused-release-object-{index}").into_bytes();
        let hash = risunest_sync_wire::hash(&bytes);
        fixture.server.put_object(&device, &hash, &bytes).unwrap();
        objects.push((hash, Some(bytes.len() as u64)));
    }
    objects.sort_by(|left, right| left.0.cmp(&right.0));
    let target = objects.last().unwrap().0.clone();
    let context = Residency::context_id(&stored, &head.epoch);
    Residency::open(store.repository_root()).unwrap().retain(&client, &stored, &head, &objects).unwrap();
    assert!(PayloadCas::new(store.repository_root()).unwrap().stat_object(&target).unwrap().is_none());
    *trace.target.lock().unwrap() = Some((store.repository_root().to_path_buf(), target.clone()));
    trace.armed.store(true, Ordering::SeqCst);
    let admission = Arc::new(crate::native_file_jobs::admission::Admission::default());
    let _server = admission.server().unwrap();
    store.asset_residency_release_unused(|| Ok(())).unwrap();
    assert!(!trace.armed.load(Ordering::SeqCst));
    let requests = trace.requests.lock().unwrap();
    // Opaque plugin storage conservatively blocks every later release page.
    assert_eq!(requests.iter().map(Vec::len).collect::<Vec<_>>(), [128]);
    assert_eq!(requests[0], objects[..128].iter().map(|(hash, _)| hash.clone()).collect::<Vec<_>>());
    assert!(requests.iter().all(|hashes| !hashes.contains(&target)));
    let residency = Residency::open(store.repository_root()).unwrap();
    let retained = fixture.server.retained_objects(&device, &head.epoch, None).unwrap();
    for (hash, _) in &objects[128..] {
        assert!(residency.object(hash, Some(&context)).unwrap().is_some());
        assert!(retained.objects.iter().any(|object| object.hash == *hash));
    }
}
