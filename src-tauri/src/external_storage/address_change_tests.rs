//! A WebDAV or S3 repository stays the same repository when the address used
//! to reach it changes. Each server fake keeps one remote namespace that two
//! addresses (another host and another base path) both reach, and answers only
//! the requests the providers make; anything else fails the test.
use super::{
    capture::CaptureCatalog,
    connection::{strategy_for_new_connection, ConnectionPurpose},
    contract::*,
    control::{self, BackupPointDocument, BackupPointKind},
    descriptor,
    fake::{self, MemoryVault},
    http::{HttpRequest, HttpResponse, HttpTransport},
    journal::{JobIdentity, TransferJournal},
    lww_engine::{Admission, ExternalLwwEngine},
    packaging::{package_and_upload, PackageLimits, SnapshotMetadata, SnapshotPurpose},
    phase_progress::PhaseProgress,
    providers::Dependencies,
    recovery, snapshot_restore,
};
use crate::{
    asset_repository::PayloadCas,
    logical_records::{encode_logical_record, encode_logical_record_key, LogicalRecordEnvelope, LogicalRecordLocator},
    persistent_store::{
        content_capture::ContentCaptureSink, external_capture::CapturedSnapshot, lww::UnitMutation,
        sync_selection::CaptureIdentity,
        PersistentStore, WorkingSetCommit,
    },
};
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use risunest_external_storage_format::{
    control as wire_control,
    crypto::RecoveryKey,
    format::{library_fingerprint_domain, Descriptor},
};
use risunest_sync_wire::{stamp::DecimalU64, unit::UnitKey};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex},
};
use tokio::io::AsyncReadExt;

const CREDENTIAL: &str = "synthetic-credential";

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// Host and base path segments of one address, and the request path below it.
fn below_address(addresses: &[(&str, &[&str])], url: &url::Url) -> (String, Vec<String>) {
    let host = url.host_str().expect("request host").to_owned();
    let mut segments: Vec<String> = url
        .path_segments()
        .expect("request path")
        .map(|segment| percent_decode_str(segment).decode_utf8().expect("utf-8 path").into_owned())
        .collect();
    if segments.last().is_some_and(String::is_empty) {
        segments.pop();
    }
    let (_, base) = addresses
        .iter()
        .find(|(known, _)| *known == host)
        .unwrap_or_else(|| panic!("request to an unknown host {host}"));
    assert!(
        segments.len() >= base.len() && segments.iter().zip(base.iter()).all(|(a, b)| a == b),
        "request outside the base path of {host}"
    );
    (host, segments.split_off(base.len()))
}

fn response(status: u16, mut headers: BTreeMap<String, String>, body: Vec<u8>) -> HttpResponse {
    headers.insert("content-length".into(), body.len().to_string());
    headers.insert("date".into(), httpdate::fmt_http_date(std::time::SystemTime::now()));
    HttpResponse { status, headers, body: Box::pin(std::io::Cursor::new(body)) }
}

async fn request_body(request: &mut HttpRequest) -> Vec<u8> {
    let mut body = Vec::new();
    if let Some(mut reader) = request.body.take() {
        reader.read_to_end(&mut body).await.unwrap();
    }
    body
}

fn condition_headers(request: &HttpRequest, allowed: &[&str]) {
    for name in request.headers.keys() {
        if name.starts_with("if-") && !allowed.contains(&name.as_str()) {
            panic!("unexpected conditional header {name} on {}", request.method);
        }
    }
}

#[derive(Clone)]
enum DavNode {
    Collection,
    File { bytes: Vec<u8>, etag: String },
}

struct DavServer {
    addresses: Vec<(&'static str, &'static [&'static str])>,
    nodes: Mutex<BTreeMap<Vec<String>, DavNode>>,
    next_tag: AtomicU64,
    hosts: Mutex<Vec<String>>,
}
impl DavServer {
    fn new(addresses: Vec<(&'static str, &'static [&'static str])>) -> Arc<Self> {
        Arc::new(Self {
            addresses,
            nodes: Mutex::new(BTreeMap::from([(Vec::new(), DavNode::Collection)])),
            next_tag: AtomicU64::new(1),
            hosts: Mutex::default(),
        })
    }
    fn href(&self, host: &str, path: &[String], collection: bool) -> String {
        let (_, base) = self.addresses.iter().find(|(known, _)| *known == host).unwrap();
        let mut href = String::new();
        for segment in base.iter().copied().chain(path.iter().map(String::as_str)) {
            href.push('/');
            href.push_str(&utf8_percent_encode(segment, NON_ALPHANUMERIC).to_string());
        }
        if collection || href.is_empty() {
            href.push('/');
        }
        href
    }
    fn entry(&self, host: &str, path: &[String], node: &DavNode) -> String {
        let properties = match node {
            DavNode::Collection => "<D:resourcetype><D:collection/></D:resourcetype>".to_owned(),
            DavNode::File { bytes, etag } => format!(
                "<D:resourcetype/><D:getcontentlength>{}</D:getcontentlength><D:getetag>{etag}</D:getetag>",
                bytes.len()
            ),
        };
        format!(
            "<D:response><D:href>{}</D:href><D:propstat><D:prop>{properties}</D:prop>\
             <D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
            self.href(host, path, matches!(node, DavNode::Collection))
        )
    }
    fn tag(&self) -> String {
        format!("\"v{}\"", self.next_tag.fetch_add(1, Ordering::SeqCst))
    }
    /// Copies every member below `from` to `to`, as moving a folder on the
    /// server would.
    fn copy_tree(&self, from: &str, to: &str) {
        let mut nodes = self.nodes.lock().unwrap();
        let copied: Vec<_> = nodes
            .iter()
            .filter(|(path, _)| path.first().map(String::as_str) == Some(from))
            .map(|(path, node)| {
                let mut moved = path.clone();
                moved[0] = to.to_owned();
                (moved, node.clone())
            })
            .collect();
        nodes.extend(copied);
    }
    fn handle(&self, request: &HttpRequest, body: Vec<u8>) -> HttpResponse {
        let (host, path) = below_address(&self.addresses, &request.url);
        self.hosts.lock().unwrap().push(host.clone());
        let mut nodes = self.nodes.lock().unwrap();
        let none = BTreeMap::new;
        let parent_is_collection = |nodes: &BTreeMap<Vec<String>, DavNode>| {
            path.split_last()
                .is_some_and(|(_, parent)| matches!(nodes.get(parent), Some(DavNode::Collection)))
        };
        match request.method.as_str() {
            "PROPFIND" => {
                condition_headers(request, &[]);
                let Some(node) = nodes.get(&path) else {
                    return response(404, none(), Vec::new());
                };
                let mut entries = self.entry(&host, &path, node);
                match request.headers.get("depth").map(String::as_str) {
                    Some("0") => {}
                    Some("1") => {
                        for (child, node) in nodes.range(path.clone()..) {
                            if child.len() == path.len() + 1 && child.starts_with(&path) {
                                entries.push_str(&self.entry(&host, child, node));
                            }
                        }
                    }
                    depth => panic!("unexpected PROPFIND depth {depth:?}"),
                }
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">{entries}</D:multistatus>"
                );
                response(207, BTreeMap::from([("content-type".into(), "application/xml".into())]), body.into_bytes())
            }
            "MKCOL" => {
                condition_headers(request, &[]);
                if nodes.contains_key(&path) {
                    return response(405, none(), Vec::new());
                }
                if !parent_is_collection(&nodes) {
                    return response(409, none(), Vec::new());
                }
                nodes.insert(path, DavNode::Collection);
                response(201, none(), Vec::new())
            }
            "PUT" => {
                condition_headers(request, &["if-none-match", "if-match"]);
                if !parent_is_collection(&nodes) {
                    return response(409, none(), Vec::new());
                }
                let current = match nodes.get(&path) {
                    Some(DavNode::File { etag, .. }) => Some(etag.clone()),
                    Some(DavNode::Collection) => return response(405, none(), Vec::new()),
                    None => None,
                };
                if request.headers.get("if-none-match").is_some_and(|value| value == "*") && current.is_some() {
                    return response(412, none(), Vec::new());
                }
                if let Some(expected) = request.headers.get("if-match") {
                    if current.as_ref() != Some(expected) {
                        return response(412, none(), Vec::new());
                    }
                }
                let etag = self.tag();
                let status = if current.is_some() { 204 } else { 201 };
                nodes.insert(path, DavNode::File { bytes: body, etag: etag.clone() });
                response(status, BTreeMap::from([("etag".into(), etag)]), Vec::new())
            }
            "GET" => {
                condition_headers(request, &["if-none-match"]);
                match nodes.get(&path) {
                    Some(DavNode::File { bytes, etag }) => {
                        if request.headers.get("if-none-match") == Some(etag) {
                            return response(304, BTreeMap::from([("etag".into(), etag.clone())]), Vec::new());
                        }
                        response(200, BTreeMap::from([("etag".into(), etag.clone())]), bytes.clone())
                    }
                    Some(DavNode::Collection) => response(405, none(), Vec::new()),
                    None => response(404, none(), Vec::new()),
                }
            }
            "DELETE" => {
                condition_headers(request, &[]);
                if !nodes.contains_key(&path) {
                    return response(404, none(), Vec::new());
                }
                nodes.retain(|member, _| !member.starts_with(&path));
                response(204, none(), Vec::new())
            }
            method => panic!("unexpected WebDAV method {method}"),
        }
    }
}
impl HttpTransport for DavServer {
    fn send<'a>(&'a self, mut request: HttpRequest, _cancel: &'a Cancellation) -> ProviderFuture<'a, HttpResponse> {
        Box::pin(async move {
            let body = request_body(&mut request).await;
            Ok(self.handle(&request, body))
        })
    }
}

struct S3Server {
    addresses: Vec<(&'static str, &'static [&'static str])>,
    bucket: &'static str,
    objects: Mutex<BTreeMap<String, (Vec<u8>, String)>>,
    next_tag: AtomicU64,
    hosts: Mutex<Vec<String>>,
}
impl S3Server {
    fn new(addresses: Vec<(&'static str, &'static [&'static str])>, bucket: &'static str) -> Arc<Self> {
        Arc::new(Self {
            addresses,
            bucket,
            objects: Mutex::default(),
            next_tag: AtomicU64::new(1),
            hosts: Mutex::default(),
        })
    }
    fn checksum(bytes: &[u8]) -> String {
        use base64::Engine;
        use sha2::Digest;
        base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(bytes))
    }
    fn object_headers(bytes: &[u8], etag: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("etag".into(), etag.to_owned()),
            ("x-amz-checksum-sha256".into(), Self::checksum(bytes)),
            ("x-amz-checksum-type".into(), "FULL_OBJECT".into()),
        ])
    }
    fn list(&self, request: &HttpRequest) -> HttpResponse {
        let query: BTreeMap<String, String> = request.url.query_pairs().into_owned().collect();
        for name in query.keys() {
            assert!(
                matches!(name.as_str(), "list-type" | "prefix" | "max-keys" | "continuation-token"),
                "unexpected list parameter {name}"
            );
        }
        assert_eq!(query.get("list-type").map(String::as_str), Some("2"));
        let prefix = query.get("prefix").cloned().unwrap_or_default();
        let limit: usize = query.get("max-keys").map_or(1000, |value| value.parse().unwrap());
        let objects = self.objects.lock().unwrap();
        let mut matching = objects
            .iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .filter(|(key, _)| query.get("continuation-token").is_none_or(|after| key.as_str() > after.as_str()));
        let page: Vec<_> = matching.by_ref().take(limit).collect();
        let truncated = matching.next().is_some();
        let mut body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>{}</Name><Prefix>{prefix}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{limit}</MaxKeys><IsTruncated>{truncated}</IsTruncated>",
            self.bucket,
            page.len()
        );
        for (key, (bytes, etag)) in &page {
            body.push_str(&format!(
                "<Contents><Key>{key}</Key><ETag>{}</ETag><Size>{}</Size></Contents>",
                etag.replace('"', "&quot;"),
                bytes.len()
            ));
        }
        if truncated {
            body.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", page.last().unwrap().0));
        }
        body.push_str("</ListBucketResult>");
        response(200, BTreeMap::from([("content-type".into(), "application/xml".into())]), body.into_bytes())
    }
    fn handle(&self, request: &HttpRequest, body: Vec<u8>) -> HttpResponse {
        let (host, mut path) = below_address(&self.addresses, &request.url);
        self.hosts.lock().unwrap().push(host);
        assert!(!path.is_empty() && path.remove(0) == self.bucket, "request outside the bucket");
        let none = BTreeMap::new;
        if path.is_empty() {
            assert_eq!(request.method.as_str(), "GET", "unexpected bucket request");
            condition_headers(request, &[]);
            return self.list(request);
        }
        assert!(request.url.query().is_none(), "unexpected object query on {}", request.method);
        let key = path.join("/");
        let mut objects = self.objects.lock().unwrap();
        match request.method.as_str() {
            "HEAD" | "GET" => {
                condition_headers(request, &["if-none-match"]);
                let Some((bytes, etag)) = objects.get(&key) else {
                    return response(404, none(), Vec::new());
                };
                if request.headers.get("if-none-match") == Some(etag) {
                    return response(304, BTreeMap::from([("etag".into(), etag.clone())]), Vec::new());
                }
                let headers = Self::object_headers(bytes, etag);
                if request.method.as_str() == "HEAD" {
                    let mut headed = response(200, headers, Vec::new());
                    headed.headers.insert("content-length".into(), bytes.len().to_string());
                    return headed;
                }
                response(200, headers, bytes.clone())
            }
            "PUT" => {
                condition_headers(request, &["if-none-match", "if-match"]);
                let current = objects.get(&key).map(|(_, etag)| etag.clone());
                if request.headers.get("if-none-match").is_some_and(|value| value == "*") && current.is_some() {
                    return response(412, none(), Vec::new());
                }
                if let Some(expected) = request.headers.get("if-match") {
                    match &current {
                        None => return response(404, none(), Vec::new()),
                        Some(current) if current != expected => return response(412, none(), Vec::new()),
                        Some(_) => {}
                    }
                }
                if let Some(declared) = request.headers.get("x-amz-checksum-sha256") {
                    assert_eq!(*declared, Self::checksum(&body), "declared checksum differs from the body");
                }
                let etag = format!("\"v{}\"", self.next_tag.fetch_add(1, Ordering::SeqCst));
                let headers = Self::object_headers(&body, &etag);
                objects.insert(key, (body, etag));
                response(200, headers, Vec::new())
            }
            "DELETE" => {
                condition_headers(request, &[]);
                objects.remove(&key);
                response(204, none(), Vec::new())
            }
            method => panic!("unexpected S3 method {method}"),
        }
    }
}
impl HttpTransport for S3Server {
    fn send<'a>(&'a self, mut request: HttpRequest, _cancel: &'a Cancellation) -> ProviderFuture<'a, HttpResponse> {
        Box::pin(async move {
            let body = request_body(&mut request).await;
            Ok(self.handle(&request, body))
        })
    }
}

fn webdav_config(endpoint: &str, root: &str) -> ConnectionConfig {
    ConnectionConfig {
        provider: "webdav".into(),
        profile: None,
        endpoint: endpoint.into(),
        account_id: "synthetic-user".into(),
        location: BTreeMap::from([("root".into(), root.into())]),
        oauth_profile: None,
    }
}

fn s3_config(endpoint: &str) -> ConnectionConfig {
    ConnectionConfig {
        provider: "s3".into(),
        profile: Some("generic".into()),
        endpoint: endpoint.into(),
        account_id: "synthetic-access".into(),
        location: BTreeMap::from([
            ("bucket".into(), "synthetic-bucket".into()),
            ("prefix".into(), "risunest".into()),
            ("region".into(), "us-east-1".into()),
        ]),
        oauth_profile: None,
    }
}

const S3_SECRET: &[u8] = br#"{"accessKeyId":"synthetic-access","secretAccessKey":"synthetic-secret"}"#;

/// One device: its own dependencies, provider and local directory.
struct Device {
    provider: Arc<dyn Provider>,
    directory: tempfile::TempDir,
    _dependencies: fake::TestDependencies,
}
impl Device {
    fn new(
        transport: Arc<dyn HttpTransport>,
        secret: &[u8],
        create: fn(Dependencies) -> Result<Arc<dyn Provider>>,
    ) -> Self {
        let dependencies =
            fake::with_transport(transport, MemoryVault::with(CREDENTIAL, secret), super::runtime::now_ms());
        Self {
            provider: create(dependencies.dependencies.clone()).unwrap(),
            directory: tempfile::tempdir().unwrap(),
            _dependencies: dependencies,
        }
    }
    async fn open(&self, config: &ConnectionConfig, mode: OpenMode) -> Result<(RepositoryHandle, super::capabilities::Capabilities)> {
        self.provider
            .open_repository(config, &SecretRef(CREDENTIAL.into()), mode, &Cancellation::default())
            .await
    }
}

struct Created {
    descriptor: Descriptor,
    descriptor_locator: RemoteLocator,
    root_key: [u8; 32],
    recovery_key: zeroize::Zeroizing<String>,
}

/// A new repository with its descriptor and recovery bootstrap, as adding a
/// connection makes one.
async fn create_repository(device: &Device, config: &ConnectionConfig, root_key: [u8; 32]) -> Created {
    let cancel = Cancellation::default();
    let (handle, capabilities) = device.open(config, OpenMode::Create).await.unwrap();
    let strategy = strategy_for_new_connection(ConnectionPurpose::Sync, &capabilities).unwrap();
    let descriptor = Descriptor::new(uuid::Uuid::new_v4().to_string(), strategy).unwrap();
    let path = device.directory.path();
    let descriptor_locator =
        descriptor::upload(path, device.provider.as_ref(), &handle, &descriptor, &root_key, &cancel)
            .await
            .unwrap();
    let recovery_key = recovery::generate_key().unwrap();
    recovery::publish_bootstrap(
        path,
        device.provider.as_ref(),
        &handle,
        &recovery::BootstrapMetadata { descriptor: descriptor.clone(), descriptor_locator: descriptor_locator.clone() },
        &root_key,
        &recovery_key,
        &cancel,
    )
    .await
    .unwrap();
    Created { descriptor, descriptor_locator, root_key, recovery_key }
}

/// Captures one root record and one asset and publishes them as a manual
/// backup point.
async fn back_up(device: &Device, handle: &RepositoryHandle, capabilities: &super::capabilities::Capabilities, created: &Created) -> (String, String) {
    let source = device.directory.path();
    let cancel = Cancellation::default();
    let asset = PayloadCas::new(source).unwrap().prepare_bytes(b"synthetic backed up asset").unwrap();
    let record = encode_logical_record(&LogicalRecordEnvelope::Root {
        value: json!({ "marker": "backed-up-through-the-first-address" }),
        owner_heads: vec![],
    })
    .unwrap();
    let record_key = encode_logical_record_key(&LogicalRecordLocator::Root).unwrap();
    let identity = CaptureIdentity {
        store_id: "synthetic-first-device".into(),
        library_epoch: "synthetic-library".into(),
        generation: "synthetic-generation".into(),
        selection_epoch: "synthetic-selection".into(),
        revision: 1,
    };
    let external = source.join("external-storage");
    let mut catalog = CaptureCatalog::create(&external.join("captures").join("address-capture"), &external, None).unwrap();
    catalog.begin(&identity, None).unwrap();
    catalog.record(&record_key, &record.bytes).unwrap();
    catalog.reference(&record_key, &asset.content_hash, asset.byte_size).unwrap();
    catalog.finish().unwrap();
    let fingerprint = catalog.content_fingerprint(&library_fingerprint_domain()).unwrap();
    let capture = CapturedSnapshot {
        id: "address-capture".into(),
        identity: identity.clone(),
        catalog,
        projected_records: 1,
        shared: false,
    };
    let mut journal = TransferJournal::open(
        &source.join("transfer-journal"),
        JobIdentity {
            job_id: "address-backup".into(),
            connection_id: "synthetic-first-connection".into(),
            repository_id: handle.repository_id.clone(),
            capture_id: capture.id.clone(),
            capture: identity.clone(),
        },
    )
    .unwrap();
    let completed = package_and_upload(
        capture,
        Vec::new(),
        source,
        &source.join("package-cache"),
        SnapshotMetadata {
            snapshot_id: "address-snapshot".into(),
            repository_id: created.descriptor.repository_id.clone(),
            library_id: identity.library_epoch.clone(),
            author_device_id: identity.store_id.clone(),
            created_at_ms: 1,
            logical_revision: 1,
            purpose: SnapshotPurpose::BackupBundle {
                source: wire_control::BundleSource::Device { writer_id: "synthetic-writer".into() },
                remote_generation: None,
                original_units: BTreeMap::new().into(),
            },
            parent_snapshot_id: None,
            content_fingerprint: fingerprint,
        },
        &created.root_key,
        PackageLimits::from_capabilities(capabilities).unwrap(),
        None,
        &mut journal,
        device.provider.as_ref(),
        handle,
        &PhaseProgress::silent(),
        &cancel,
    )
    .await
    .unwrap();
    let point = BackupPointDocument::single(
        &created.descriptor,
        "address-point".into(),
        BackupPointKind::Manual,
        1,
        completed.reference.clone(),
    )
    .unwrap();
    control::upload_backup_point(
        &created.descriptor,
        &created.root_key,
        point,
        &mut journal,
        device.provider.as_ref(),
        handle,
        &cancel,
    )
    .await
    .unwrap();
    (record_key, record.hash)
}

fn engine(device: &Device, handle: RepositoryHandle, capabilities: super::capabilities::Capabilities, created: &Created, id: &str) -> ExternalLwwEngine {
    ExternalLwwEngine {
        provider: device.provider.clone(),
        repository: handle,
        library: created.descriptor.repository_id.clone(),
        root_key: zeroize::Zeroizing::new(created.root_key),
        admission: Some(Admission::synthetic(super::runtime::now_ms())),
        connection_id: id.into(),
        connection_root: device.directory.path().join(id),
        capabilities,
        descriptor: created.descriptor.clone(),
    }
}

fn set(store: &mut PersistentStore, key: &[&str], value: serde_json::Value) {
    store
        .commit(&WorkingSetCommit {
            expected_revision: store.revision().unwrap(),
            unit_mutations: Some(vec![UnitMutation::Set { key: UnitKey::new(key).unwrap(), value }]),
            ..Default::default()
        })
        .unwrap();
}

fn other_key(key: &str) -> zeroize::Zeroizing<String> {
    loop {
        let candidate = RecoveryKey::generate().unwrap().expose();
        if candidate.as_str() != key {
            return candidate;
        }
    }
}

/// A repository made and backed up through `first` is joined through
/// `second`: the recovery key opens it, its history lists the backup, the
/// backup restores, and sync published through one address is received
/// through the other.
async fn reach_through_another_address(
    transport: Arc<dyn HttpTransport>,
    secret: &[u8],
    create: fn(Dependencies) -> Result<Arc<dyn Provider>>,
    first: ConnectionConfig,
    second: ConnectionConfig,
) {
    let cancel = Cancellation::default();
    let first_device = Device::new(transport.clone(), secret, create);
    let second_device = Device::new(transport, secret, create);
    let created = create_repository(&first_device, &first, [7; 32]).await;
    let (first_handle, first_capabilities) = first_device.open(&first, OpenMode::Existing).await.unwrap();
    let (record_key, record_hash) = back_up(&first_device, &first_handle, &first_capabilities, &created).await;

    let (second_handle, second_capabilities) = second_device.open(&second, OpenMode::Existing).await.unwrap();
    assert_eq!(second_handle.connection_identity, first_handle.connection_identity);
    assert_eq!(second_handle.repository_id, first_handle.repository_id);
    let target = second_device.directory.path();

    let wrong = recovery::open_bootstrap(target, second_device.provider.as_ref(), &second_handle, &other_key(&created.recovery_key), &cancel)
        .await
        .err()
        .expect("another recovery key is refused");
    assert_eq!(wrong.kind, ErrorKind::RecoveryKeyMismatch);
    let malformed = recovery::open_bootstrap(target, second_device.provider.as_ref(), &second_handle, "not-a-recovery-key", &cancel)
        .await
        .err()
        .expect("a malformed recovery key is refused");
    assert_eq!(malformed.kind, ErrorKind::RecoveryKeyMismatch);

    let imported = recovery::open_bootstrap(target, second_device.provider.as_ref(), &second_handle, &created.recovery_key, &cancel)
        .await
        .unwrap();
    assert_eq!(imported.metadata.descriptor, created.descriptor);
    assert_eq!(imported.metadata.descriptor_locator, created.descriptor_locator);
    assert_eq!(*imported.key, created.root_key);
    descriptor::read(
        target,
        second_device.provider.as_ref(),
        &second_handle,
        &imported.metadata.descriptor_locator,
        &created.descriptor,
        &imported.key,
        &cancel,
    )
    .await
    .unwrap();

    let history = control::list_backup_points_page(
        &created.descriptor,
        &imported.key,
        second_device.provider.as_ref(),
        &second_handle,
        None,
        10,
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(history.points.len(), 1);
    let point = &history.points[0].document;
    assert_eq!(point.point_id, "address-point");
    let restored = snapshot_restore::download_snapshot(
        &point.bundle,
        &target.join("download"),
        &imported.key,
        None,
        snapshot_restore::SourceTrust::Downloaded,
        second_device.provider.as_ref(),
        &second_handle,
        &PhaseProgress::silent(),
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(restored.snapshot_id, "address-snapshot");
    assert_eq!(restored.records.len(), 1);
    assert_eq!(restored.records[0].key, record_key);
    assert_eq!(restored.records[0].content_hash, record_hash);

    let now = super::runtime::now_ms();
    super::leases::observe_time_sample(super::leases::TimeSample {
        date_ms: Some(now),
        local_before_ms: now,
        local_after_ms: now,
        round_trip_ms: 0,
        cache_bypassed: true,
        cache_hit: false,
        age_ms: None,
        status: 200,
    });
    let mut sender = engine(&first_device, first_handle, first_capabilities, &created, "synthetic-first-connection");
    let receiver = engine(&second_device, second_handle, second_capabilities, &created, "synthetic-second-connection");
    assert_eq!(sender.target_scope(), receiver.target_scope());
    let first_store_root = tempfile::tempdir().unwrap();
    let second_store_root = tempfile::tempdir().unwrap();
    let mut first_store = PersistentStore::open(first_store_root.path()).unwrap();
    let mut second_store = PersistentStore::open(second_store_root.path()).unwrap();
    set(&mut first_store, &["root", "language"], json!("ko"));
    let sent = sender.publish(&mut first_store, DecimalU64(0), &[], &cancel).await.unwrap();
    assert_eq!(sent.segments.0, 1);
    assert_eq!(receiver.receive_and_apply(&mut second_store, DecimalU64(0), &[], &cancel).await.unwrap(), 1);
    assert_eq!(second_store.read_root(None).unwrap().value["language"], "ko");
}

const DAV_ADDRESSES: [(&str, &[&str]); 2] = [("dav-old.invalid", &["dav", "files"]), ("dav-new.invalid", &["remote"])];
const S3_ADDRESSES: [(&str, &[&str]); 2] = [("s3-old.invalid", &["gateway"]), ("s3-new.invalid", &[])];

#[test]
fn a_webdav_repository_keeps_its_history_restore_and_sync_at_a_new_address() {
    run(async {
        let server = DavServer::new(DAV_ADDRESSES.to_vec());
        reach_through_another_address(
            server.clone(),
            b"app-password",
            super::providers::webdav::create,
            webdav_config("http://dav-old.invalid/dav/files", "RisuNest"),
            webdav_config("http://dav-new.invalid/remote/", "RisuNest"),
        )
        .await;
        let hosts = server.hosts.lock().unwrap();
        assert!(hosts.iter().any(|host| host == "dav-old.invalid"));
        assert!(hosts.iter().any(|host| host == "dav-new.invalid"));
    });
}

#[test]
fn an_s3_repository_keeps_its_history_restore_and_sync_at_a_new_endpoint() {
    run(async {
        let server = S3Server::new(S3_ADDRESSES.to_vec(), "synthetic-bucket");
        reach_through_another_address(
            server.clone(),
            S3_SECRET,
            super::providers::s3::create,
            s3_config("http://s3-old.invalid/gateway"),
            s3_config("http://s3-new.invalid"),
        )
        .await;
        let hosts = server.hosts.lock().unwrap();
        assert!(hosts.iter().any(|host| host == "s3-old.invalid"));
        assert!(hosts.iter().any(|host| host == "s3-new.invalid"));
    });
}

/// Another repository at the same address and root is never taken for the
/// one a connection holds, and the refusal names what differs instead of
/// calling the data damaged.
async fn refuse_another_repository_at_one_address(
    first_server: Arc<dyn HttpTransport>,
    second_server: Arc<dyn HttpTransport>,
    secret: &[u8],
    create: fn(Dependencies) -> Result<Arc<dyn Provider>>,
    config: ConnectionConfig,
) {
    let cancel = Cancellation::default();
    let empty = Device::new(second_server.clone(), secret, create);
    let error = empty.open(&config, OpenMode::Existing).await.err().expect("an empty root holds no repository");
    assert_eq!(error.kind, ErrorKind::NotFound);

    let first_device = Device::new(first_server, secret, create);
    let held = create_repository(&first_device, &config, [7; 32]).await;
    let second_device = Device::new(second_server, secret, create);
    let other = create_repository(&second_device, &config, [9; 32]).await;
    assert_ne!(other.descriptor.repository_id, held.descriptor.repository_id);

    let (handle, _) = second_device.open(&config, OpenMode::Existing).await.unwrap();
    let path = second_device.directory.path();
    let error = recovery::open_bootstrap(path, second_device.provider.as_ref(), &handle, &held.recovery_key, &cancel)
        .await
        .err()
        .expect("the held repository's key does not open another repository");
    assert_eq!(error.kind, ErrorKind::RecoveryKeyMismatch);
    let error = descriptor::read(
        path,
        second_device.provider.as_ref(),
        &handle,
        &held.descriptor_locator,
        &held.descriptor,
        &held.root_key,
        &cancel,
    )
    .await
    .err()
    .expect("the held descriptor is not at this address");
    assert_eq!(error.kind, ErrorKind::NotFound);
}

#[test]
fn another_webdav_repository_at_the_same_address_is_refused_by_kind() {
    run(async {
        refuse_another_repository_at_one_address(
            DavServer::new(DAV_ADDRESSES.to_vec()),
            DavServer::new(DAV_ADDRESSES.to_vec()),
            b"app-password",
            super::providers::webdav::create,
            webdav_config("http://dav-old.invalid/dav/files", "RisuNest"),
        )
        .await;
    });
}

#[test]
fn another_s3_repository_at_the_same_endpoint_is_refused_by_kind() {
    run(async {
        refuse_another_repository_at_one_address(
            S3Server::new(S3_ADDRESSES.to_vec(), "synthetic-bucket"),
            S3Server::new(S3_ADDRESSES.to_vec(), "synthetic-bucket"),
            S3_SECRET,
            super::providers::s3::create,
            s3_config("http://s3-old.invalid/gateway"),
        )
        .await;
    });
}

/// A repository moved to another folder carries a recovery bootstrap made for
/// its old location, which is reported as a different repository.
#[test]
fn a_webdav_repository_moved_to_another_folder_is_reported_as_another_repository() {
    run(async {
        let server = DavServer::new(DAV_ADDRESSES.to_vec());
        let device = Device::new(server.clone(), b"app-password", super::providers::webdav::create);
        let created = create_repository(&device, &webdav_config("http://dav-old.invalid/dav/files", "RisuNest"), [7; 32]).await;
        server.copy_tree("RisuNest", "Moved");
        let (handle, _) = device
            .open(&webdav_config("http://dav-old.invalid/dav/files", "Moved"), OpenMode::Existing)
            .await
            .unwrap();
        let error = recovery::open_bootstrap(
            device.directory.path(),
            device.provider.as_ref(),
            &handle,
            &created.recovery_key,
            &Cancellation::default(),
        )
        .await
        .err()
        .expect("a bootstrap made for another location is refused");
        assert_eq!(error.kind, ErrorKind::RepositoryMismatch);
    });
}
