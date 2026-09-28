//! A loopback Sync server that records the requests a client actually sends.
//! Request counts are the structural unit the transfer work is measured in, so
//! they are observed from outside the product rather than from counters
//! compiled into it.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

#[derive(Default)]
pub(super) struct Requests {
    total: AtomicU64,
    by_path: Mutex<BTreeMap<String, u64>>,
}

impl Requests {
    fn observe(&self, method: &str, path: &str) {
        self.total.fetch_add(1, Ordering::Relaxed);
        *self
            .by_path
            .lock()
            .unwrap()
            .entry(format!("{method} {}", category(path)))
            .or_default() += 1;
    }
}

/// Collapse the per-object path segment so a report names the request kind
/// rather than one entry per hash.
fn category(path: &str) -> String {
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    match (parts.next(), parts.next()) {
        (Some("objects"), Some("transfer")) => "objects/transfer".to_owned(),
        (Some("objects"), Some("delta")) => "objects/delta".to_owned(),
        (Some("objects"), Some(_)) => "objects/{hash}".to_owned(),
        (Some("object-deltas"), Some(_)) => "object-deltas/{id}".to_owned(),
        (Some("uploads"), Some(_)) => match parts.next() {
            Some("chunks") => "uploads/{id}/chunks/{index}".to_owned(),
            Some(tail) => format!("uploads/{{id}}/{tail}"),
            None => "uploads/{id}".to_owned(),
        },
        (Some(head), Some(_)) => format!("{head}/{{id}}"),
        (Some(head), None) => head.to_owned(),
        _ => path.to_owned(),
    }
}

pub(super) struct Observed {
    pub store: Arc<Store>,
    _directory: tempfile::TempDir,
    endpoint: String,
    requests: Arc<Requests>,
    runtime: Option<tokio::runtime::Runtime>,
    task: tokio::task::JoinHandle<()>,
    credential: risunest_sync_server::store::DeviceCredential,
}

impl Observed {
    pub fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::init(directory.path()).unwrap());
        let credential = store.add_device().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Requests::default());
        let observed = requests.clone();
        let served = store.clone();
        let task = runtime.spawn(async move {
            let router = http::router(served).layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let observed = observed.clone();
                    async move {
                        observed.observe(request.method().as_str(), request.uri().path());
                        next.run(request).await
                    }
                },
            ));
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            store,
            _directory: directory,
            endpoint,
            requests,
            runtime: Some(runtime),
            task,
            credential,
        }
    }

    pub fn device(&self) -> risunest_sync_server::store::Device {
        self.store
            .authenticate(&self.credential.library_id, &self.credential.token)
            .unwrap()
    }

    pub fn client(&self) -> ServerClient {
        ServerClient::new(ServerConfig {
            directory: None,
            endpoint: self.endpoint.clone(),
            library_id: self.credential.library_id.clone(),
            device_id: self.credential.device_id.clone(),
            token: self.credential.token.clone(),
        })
        .unwrap()
    }

    pub fn reset(&self) {
        self.requests.total.store(0, Ordering::Relaxed);
        self.requests.by_path.lock().unwrap().clear();
    }

    pub fn total(&self) -> u64 {
        self.requests.total.load(Ordering::Relaxed)
    }

    pub fn by_path(&self) -> BTreeMap<String, u64> {
        self.requests.by_path.lock().unwrap().clone()
    }
}

impl Drop for Observed {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(std::time::Duration::from_secs(2));
        }
    }
}
