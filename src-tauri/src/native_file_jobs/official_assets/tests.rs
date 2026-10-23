use super::download::Spooled;
use super::*;
use crate::asset_repository::PayloadCas;
use crate::native_file_jobs::{JobKind, JobRegistry};
use crate::persistent_store::AssetAlias;
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::thread;

enum Reply {
    Send(Vec<u8>),
    /// A body written in separate flushed pieces.
    Chunks(Vec<Vec<u8>>),
    /// Closes the connection without an answer.
    Close,
    /// Sends a prefix and then holds the connection open.
    Stall(Vec<u8>),
}

fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

type Responder = dyn Fn(&str, usize) -> Reply + Send + Sync;

/// A loopback account server that answers each read by key and attempt number.
struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
    accept: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start(reply: impl Fn(&str, usize) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let reply: Arc<Responder> = Arc::new(reply);
        let accept = {
            let (stop, seen) = (Arc::clone(&stop), Arc::clone(&seen));
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let (stop, seen, reply) = (Arc::clone(&stop), Arc::clone(&seen), Arc::clone(&reply));
                            thread::spawn(move || answer(stream, &stop, &seen, reply.as_ref()));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(error) => panic!("fixture accept failed: {error}"),
                    }
                }
            })
        };
        Self { url, stop, seen, accept: Some(accept) }
    }

    fn requests(&self, key: &str) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().iter().filter(|(seen, _)| seen == key).map(|(_, header)| header.clone()).collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn answer(mut stream: TcpStream, stop: &AtomicBool, seen: &Mutex<Vec<(String, Vec<u8>)>>, reply: &Responder) {
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    let end = loop {
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end;
        }
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&buffer[..read]),
        }
    };
    let mut lines = request[..end].split(|byte| *byte == b'\n').map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    let target = lines.next().unwrap().split(|byte| *byte == b' ').nth(1).unwrap();
    let key = String::from_utf8(hex::decode(target.strip_prefix(b"/api/account/read/").unwrap()).unwrap()).unwrap();
    let header = lines.find_map(|line| line.strip_prefix(b"x-risu-key: ").map(<[u8]>::to_vec)).unwrap_or_default();
    let attempt = {
        let mut seen = seen.lock().unwrap();
        seen.push((key.clone(), header));
        seen.iter().filter(|(seen, _)| *seen == key).count()
    };
    match reply(&key, attempt) {
        Reply::Send(bytes) => {
            let _ = stream.write_all(&bytes);
        }
        Reply::Chunks(parts) => {
            for part in parts {
                if stream.write_all(&part).and_then(|_| stream.flush()).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        Reply::Close => {}
        Reply::Stall(prefix) => {
            let _ = stream.write_all(&prefix).and_then(|_| stream.flush());
            while !stop.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    store: PersistentStore,
    registry: JobRegistry,
    job: Arc<JobControl>,
    owned: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = PersistentStore::open(directory.path()).unwrap();
        let registry = JobRegistry::default();
        let job = registry.create(JobKind::RestoreOfficialAccountAssets).unwrap();
        let owned = directory.path().join("native-file-jobs").join("jobs").join(job.id());
        std::fs::create_dir_all(&owned).unwrap();
        Self { directory, store, registry, job, owned }
    }

    fn request(&self, url: &str, keys: usize, concurrency: usize, reauthenticated: bool, library: Option<&str>) -> Request {
        Request {
            base_url: url.to_owned(),
            credential: OfficialSnapshotCredential::RisuAuth { token: "synthetic-token".to_owned() },
            account_id: uuid::Uuid::new_v4().to_string(),
            expected_library: library.map_or_else(|| self.store.library_identity().unwrap(), str::to_owned),
            key_count: keys,
            concurrency,
            reauthenticated,
            store: self.store.open_native_job_store().unwrap(),
            revision: self.store.revision().unwrap(),
            shared: Handle::new(keys),
        }
    }

    /// Runs the job with every key enqueued in pages of `page` and returns its settled keys.
    fn restore(&self, request: Request, keys: &[&str], page: usize) -> (Result<JobResultSummary, NativeJobError>, BTreeMap<String, Settled>) {
        let shared = Arc::clone(&request.shared.0);
        let (owned, job) = (&self.owned, &self.job);
        let result = thread::scope(|scope| {
            let running = scope.spawn(move || run(request, owned, job));
            for chunk in keys.chunks(page) {
                assert!(shared.enqueue(chunk.iter().map(|key| (*key).to_owned()).collect()).unwrap());
            }
            running.join().unwrap()
        });
        let mut settled = BTreeMap::new();
        let mut offset = 0;
        loop {
            let page = shared.page(offset, 2).unwrap();
            if page.entries.is_empty() {
                assert_eq!(offset, page.total);
                break;
            }
            offset += page.entries.len();
            for entry in page.entries {
                assert!(settled.insert(keys[entry.index].to_owned(), entry.result).is_none());
            }
        }
        (result, settled)
    }

    fn alias(&self, key: &str) -> Option<AssetAlias> {
        self.store.read_asset_alias("asset", key, None).unwrap().map(|alias| alias.value)
    }

    fn object(&self, alias: &AssetAlias) -> Vec<u8> {
        let mut bytes = Vec::new();
        PayloadCas::new(self.directory.path())
            .unwrap()
            .open_object(alias.object_hash.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    fn progress(&self) -> RestoreProgress {
        self.job.status().official_asset_restore.unwrap()
    }

    fn spooled_files(&self) -> usize {
        std::fs::read_dir(&self.owned).unwrap().count()
    }
}

fn ok(body: &[u8]) -> Reply {
    Reply::Send(response("200 OK", &[], body))
}

fn gzip(body: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn bodies_install_with_the_verified_write_metadata_and_other_answers_settle_by_class() {
    let server = Server::start(|key, _| match key {
        "assets/a.png" => Reply::Chunks(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\nfirst".to_vec(),
            b" second".to_vec(),
            b" end".to_vec(),
        ]),
        "assets/sub\\noext" => ok(b"no extension"),
        "assets/café.png" => ok(b"latin one"),
        "assets/missing.png" => Reply::Send(response("204 No Content", &[], b"")),
        "assets/cached.png" => Reply::Send(response("303 See Other", &[], br#"{"match":false}"#)),
        "assets/matched.png" => Reply::Send(response("303 See Other", &[], br#"{"match":true}"#)),
        "assets/gone.png" => Reply::Send(response("404 Not Found", &[], b"")),
        "assets/encoded.png" => Reply::Send(response("200 OK", &[("Content-Encoding", "gzip")], &gzip(b"compressed"))),
        _ => panic!("unexpected read of {key}"),
    });
    let fixture = Fixture::new();
    let keys = [
        "assets/a.png", "assets/sub\\noext", "assets/café.png", "assets/missing.png", "assets/cached.png",
        "assets/matched.png", "assets/gone.png", "assets/encoded.png", "assets/한.png",
    ];
    let start = fixture.store.revision().unwrap();
    let (result, settled) = fixture.restore(fixture.request(&server.url, keys.len(), 2, false, None), &keys, 4);
    let result = result.unwrap();
    use Settled::{Failed, Installed, Missing};
    assert_eq!(
        settled,
        BTreeMap::from([
            ("assets/a.png", Installed), ("assets/sub\\noext", Installed), ("assets/café.png", Installed),
            ("assets/missing.png", Missing), ("assets/cached.png", Missing), ("assets/matched.png", Failed),
            ("assets/gone.png", Failed), ("assets/encoded.png", Failed), ("assets/한.png", Failed),
        ].map(|(key, result)| (key.to_owned(), result))),
    );
    assert!(result.revision > start);
    assert_eq!(fixture.store.revision().unwrap(), result.revision);
    let first = fixture.alias("assets/a.png").unwrap();
    assert_eq!(
        first,
        AssetAlias {
            key: "assets/a.png".to_owned(),
            object_hash: Some(hex::encode(Sha256::digest(b"first second end"))),
            kind: "asset".to_owned(),
            size: 16,
            mime: String::new(),
            name: "a.png".to_owned(),
            ext: "png".to_owned(),
            inlay_type: None,
            width: None,
            height: None,
            metadata: serde_json::json!({}),
        },
    );
    assert_eq!(fixture.object(&first), b"first second end");
    let unnamed = fixture.alias("assets/sub\\noext").unwrap();
    assert_eq!((unnamed.name.as_str(), unnamed.ext.as_str()), ("noext", "assets/sub\\noext"));
    assert_eq!(fixture.object(&fixture.alias("assets/café.png").unwrap()), b"latin one");
    assert_eq!(server.requests("assets/café.png"), vec![b"assets/caf\xe9.png".to_vec()]);
    assert!(server.requests("assets/한.png").is_empty());
    assert_eq!(server.requests("assets/encoded.png").len(), 1);
    for key in &keys[3..] {
        assert_eq!(fixture.alias(key), None);
    }
    let progress = fixture.progress();
    assert_eq!((progress.installed, progress.missing, progress.failed, progress.reason), (3, 2, 4, None));
    assert_eq!(fixture.spooled_files(), 0);
}

#[test]
fn transient_failures_are_retried_until_the_body_arrives() {
    let server = Server::start(|key, attempt| match (key, attempt) {
        ("assets/busy.png", 1) => Reply::Send(response("503 Service Unavailable", &[("Retry-After", "0")], b"")),
        ("assets/dropped.png", 1) => Reply::Close,
        (_, _) => ok(key.as_bytes()),
    });
    let fixture = Fixture::new();
    let keys = ["assets/busy.png", "assets/dropped.png"];
    let (result, settled) = fixture.restore(fixture.request(&server.url, keys.len(), 2, false, None), &keys, 512);
    result.unwrap();
    assert!(settled.values().all(|result| *result == Settled::Installed));
    for key in keys {
        assert_eq!(server.requests(key).len(), 2);
        assert_eq!(fixture.object(&fixture.alias(key).unwrap()), key.as_bytes());
    }
}

#[test]
fn a_rejection_ends_the_run_and_bodies_already_downloaded_still_install() {
    for reauthenticated in [false, true] {
        let server = Server::start(|key, _| match key {
            "assets/rejected.png" => Reply::Send(response("401 Unauthorized", &[], b"")),
            _ => ok(key.as_bytes()),
        });
        let fixture = Fixture::new();
        let keys = ["assets/first.png", "assets/rejected.png", "assets/later.png"];
        let (result, settled) = fixture.restore(fixture.request(&server.url, keys.len(), 1, reauthenticated, None), &keys, 512);
        result.unwrap();
        let mut expected = BTreeMap::from([("assets/first.png".to_owned(), Settled::Installed)]);
        if reauthenticated {
            expected.insert("assets/rejected.png".to_owned(), Settled::Failed);
        }
        assert_eq!(settled, expected);
        assert!(fixture.alias("assets/first.png").is_some());
        assert!(server.requests("assets/later.png").is_empty());
        let reason = if reauthenticated { StopReason::Authentication } else { StopReason::ReauthenticationNeeded };
        assert_eq!(fixture.progress().reason, Some(reason));
    }
}

#[test]
fn a_library_replaced_before_commit_fails_the_batch_without_aliases() {
    let server = Server::start(|key, _| ok(key.as_bytes()));
    let fixture = Fixture::new();
    let keys = ["assets/first.png", "assets/second.png", "assets/third.png"];
    let (result, settled) =
        fixture.restore(fixture.request(&server.url, keys.len(), 1, false, Some("replaced-library")), &keys, 512);
    result.unwrap();
    assert_eq!(settled, BTreeMap::from([("assets/first.png".to_owned(), Settled::Failed)]));
    assert_eq!(fixture.progress().reason, Some(StopReason::LibraryChanged));
    for key in keys {
        assert_eq!(fixture.alias(key), None);
    }
    assert_eq!(fixture.spooled_files(), 0);
}

#[test]
fn cancellation_during_a_body_leaves_no_alias_or_spooled_file_and_stops_taking_keys() {
    let server = Server::start(|_, _| {
        Reply::Stall(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\npartial".to_vec())
    });
    let fixture = Fixture::new();
    let request = fixture.request(&server.url, 2, 2, false, None);
    let shared = Arc::clone(&request.shared.0);
    let (owned, job) = (&fixture.owned, &fixture.job);
    let result = thread::scope(|scope| {
        let running = scope.spawn(move || run(request, owned, job));
        assert!(shared.enqueue(vec!["assets/stalled.png".to_owned()]).unwrap());
        while fixture.job.status().official_asset_restore.is_none_or(|progress| progress.bytes == 0) {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fixture.spooled_files(), 1);
        fixture.registry.cancel(&fixture.job.id()).unwrap();
        running.join().unwrap()
    });
    assert_eq!(result.unwrap_err().code, "cancelled");
    assert_eq!(fixture.alias("assets/stalled.png"), None);
    assert_eq!(fixture.spooled_files(), 0);
    assert_eq!(shared.enqueue(vec!["assets/after.png".to_owned()]).unwrap(), false);
    assert_eq!(shared.page(0, 512).unwrap().total, 0);
}

#[test]
fn the_spool_holds_one_batch_including_the_batch_being_installed() {
    let directory = tempfile::tempdir().unwrap();
    let body = |index: usize, size: u64| Body {
        index,
        key: format!("assets/{index}.png"),
        spooled: Spooled { path: directory.path().join(format!("asset-{index}.payload")), sha256: String::new(), size },
    };
    let mut backlog = Backlog::default();
    for index in 0..BACKLOG_BODIES {
        assert!(backlog.open());
        backlog.push(body(index, 1));
    }
    assert!(!backlog.open());
    assert_eq!(backlog.take().unwrap().len(), BACKLOG_BODIES);
    assert!(!backlog.open());
    // A read already in flight still lands, and waits for the installing batch.
    backlog.push(body(BACKLOG_BODIES, 1));
    assert!(backlog.take().is_none());
    backlog.installed();
    assert!(backlog.open());
    assert_eq!(backlog.take().unwrap().len(), 1);
    backlog.installed();

    backlog.push(body(0, BACKLOG_BYTES / 2));
    assert_eq!(backlog.take().unwrap().len(), 1);
    assert!(backlog.open());
    backlog.push(body(1, BACKLOG_BYTES / 2));
    assert!(!backlog.open());
    backlog.installed();
    assert!(backlog.open());
}

#[test]
fn key_pages_are_bounded_to_asset_keys_within_the_declared_count() {
    let shared = Handle::new(513).0;
    let page = |count: usize| (0..count).map(|index| format!("assets/{index}.png")).collect::<Vec<_>>();
    assert_eq!(shared.enqueue(page(513)).unwrap_err().code, "invalid-input");
    assert_eq!(shared.enqueue(vec!["database/database.bin".to_owned()]).unwrap_err().code, "invalid-input");
    assert!(shared.enqueue(page(512)).unwrap());
    assert!(shared.enqueue(vec!["assets\\last.png".to_owned()]).unwrap());
    assert_eq!(shared.enqueue(vec!["assets/extra.png".to_owned()]).unwrap_err().code, "invalid-input");
    assert_eq!(shared.page(0, 513).unwrap_err().code, "invalid-input");
}
