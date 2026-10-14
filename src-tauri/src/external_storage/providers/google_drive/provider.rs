//! File-ID based Drive v3 adapter. Every remote object is one file in the
//! repository root folder, tagged with private application properties, so a
//! duplicate file name never decides which object is read or replaced.
use super::{
    auth::{self, CachedToken},
    config::{self, Settings, Space},
    wire::{self, About, DriveFile, FileList, GeneratedIds},
};
use crate::external_storage::{
    auth::SecretBytes,
    capabilities::Capabilities,
    contract::*,
    http::{self, HttpRequest, HttpResponse},
    providers::{common, Dependencies},
    quota::AccountKey,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Reserved control name of the mutable head. It resolves to a stable file ID
/// held by the handle, which every device rediscovers from the role property.
pub(super) const HEAD_OBJECT: &str = "control-head";
const ROLE_KEY: &str = "risunestRole";
const OBJECT_KEY: &str = "risunestObjectId";
const JOB_KEY: &str = "risunestJobId";
const HEAD_ROLE: &str = "head";
const DESCRIPTOR_ROLE: &str = "descriptor";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
const OCTET_STREAM: &str = "application/octet-stream";
const FILE_FIELDS: &str = "id,name,size,version,sha256Checksum,appProperties";
const LIST_FIELDS: &str =
    "nextPageToken,incompleteSearch,files(id,name,size,version,sha256Checksum,appProperties)";
const UPLOAD_ALIGNMENT: u64 = 256 * 1024;
const UPLOAD_CHUNK_BYTES: u64 = 32 * UPLOAD_ALIGNMENT;
/// Documented ceiling of a single multipart or simple upload request.
const MULTIPART_MAX_BYTES: u64 = 5_000_000;
const MAX_STORED_BYTES: u64 = 5_000_000_000_000;
const CONTROL_PAGE_SIZE: &str = "100";

fn corrupt() -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}

fn segment_file_name(file: &DriveFile) -> Result<&str> {
    if file.property(ROLE_KEY) != Some("segment") { return Err(corrupt()); }
    let name = file.name.as_deref().and_then(|name| name.strip_prefix("segments/")).ok_or_else(corrupt)?;
    crate::external_storage::contract::parse_segment_object_id(name)?;
    Ok(name)
}

fn segment_locator_object(file_id: &str, name: &str) -> Result<String> {
    if !config::is_drive_id(file_id) { return Err(corrupt()); }
    crate::external_storage::contract::parse_segment_object_id(name)?;
    Ok(format!("{file_id}/{name}"))
}

fn segment_locator_parts(locator: &RemoteLocator) -> Result<Option<(&str, &str)>> {
    if locator.collection.as_deref() != Some("segments") { return Ok(None); }
    let (file_id, name) = locator.object.split_once('/').ok_or_else(corrupt)?;
    if !config::is_drive_id(file_id) { return Err(corrupt()); }
    crate::external_storage::contract::parse_segment_object_id(name)?;
    Ok(Some((file_id, name)))
}

fn validate_segment_file(file: &DriveFile, file_id: &str, name: &str) -> Result<()> {
    if file.file_id()? != file_id || segment_file_name(file)? != name { return Err(corrupt()); }
    Ok(())
}

fn role_token(role: ObjectRole) -> &'static str {
    match role {
        ObjectRole::Descriptor => DESCRIPTOR_ROLE,
        ObjectRole::Pack => "pack",
        ObjectRole::Catalog => "catalog",
        ObjectRole::SyncState => "state",
        ObjectRole::Segment => "segment",
        ObjectRole::Snapshot => "snapshot",
        ObjectRole::BackupBundle => "bundle",
        ObjectRole::BackupPoint => "backupPoint",
        ObjectRole::InventoryPage => "inventoryPage",
        ObjectRole::Lease => "lease",
    }
}
/// Only names the container a collection lives in. States and bundles share
/// one, so the role returned here never classifies a listed object; the
/// authenticated envelope header does that.
fn collection_role(collection: Collection) -> ObjectRole {
    match collection {
        Collection::Segments => ObjectRole::Segment,
        Collection::Snapshots => ObjectRole::Snapshot,
        Collection::BackupPoints => ObjectRole::BackupPoint,
        Collection::InventoryPages => ObjectRole::InventoryPage,
        Collection::Descriptors => ObjectRole::Descriptor,
        Collection::Leases => ObjectRole::Lease,
    }
}
fn collection_token(role: ObjectRole) -> Option<&'static str> {
    match role {
        ObjectRole::SyncState | ObjectRole::Snapshot | ObjectRole::BackupBundle => Some("snapshots"),
        ObjectRole::Segment => Some("segments"),
        ObjectRole::BackupPoint => Some("backupPoints"),
        ObjectRole::InventoryPage => Some("inventory"),
        ObjectRole::Descriptor => Some("descriptors"),
        ObjectRole::Lease => Some("leases"),
        ObjectRole::Pack | ObjectRole::Catalog => None,
    }
}

struct HeadState {
    file_id: String,
    present: bool,
}

pub(super) struct Context {
    settings: Settings,
    secret: SecretRef,
    account: AccountKey,
    token: Mutex<Option<CachedToken>>,
    head: Mutex<HeadState>,
}
impl Context {
    fn session(&self) -> Session<'_> {
        Session {
            settings: &self.settings,
            secret: &self.secret,
            account: &self.account,
            token: &self.token,
        }
    }
}

#[derive(Clone, Copy)]
struct Session<'a> {
    settings: &'a Settings,
    secret: &'a SecretRef,
    account: &'a AccountKey,
    token: &'a Mutex<Option<CachedToken>>,
}

pub(crate) struct GoogleDrive {
    dependencies: Dependencies,
}

pub(crate) struct SetupFolder {
    pub id: String,
    pub name: String,
}

/// Opaque upload state sealed in the vault. The session URI is a bearer
/// credential, so it never reaches a journal, a locator or a log.
struct SealedUpload {
    file_id: String,
    session_uri: Option<url::Url>,
    intent: ObjectIntent,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredUpload {
    file_id: String,
    session_uri: Option<String>,
    intent: ObjectIntent,
}

enum SessionStatus {
    Complete(DriveFile),
    Incomplete(u64),
    Gone,
}

fn with_query(mut url: url::Url, pairs: &[(&str, &str)]) -> url::Url {
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in pairs {
            query.append_pair(name, value);
        }
    }
    url
}

fn authorized_headers(token: &str) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("authorization".to_owned(), format!("Bearer {token}"));
    headers
}

fn parse_confirmed_offset(headers: &BTreeMap<String, String>) -> Result<u64> {
    let Some(range) = headers.get("range") else {
        return Ok(0);
    };
    let end = range
        .trim()
        .strip_prefix("bytes=0-")
        .and_then(|end| end.parse::<u64>().ok())
        .ok_or_else(corrupt)?;
    end.checked_add(1).ok_or_else(corrupt)
}

impl GoogleDrive {
    pub(crate) fn new(dependencies: Dependencies) -> Self {
        Self { dependencies }
    }

    fn setup_context(
        &self,
        config: &ConnectionConfig,
        secret: &SecretRef,
        account_id: &str,
        folder_id: &str,
    ) -> Result<(Settings, AccountKey, Mutex<Option<auth::CachedToken>>)> {
        let mut bound = config.clone();
        bound.account_id = account_id.to_owned();
        bound.location.remove("folderName");
        bound.location.insert("folderId".into(), folder_id.into());
        let settings = Settings::parse(&bound)?;
        let account = AccountKey::new(config::PROVIDER_ID, &settings.api("/")?, account_id)?;
        let _ = secret;
        Ok((settings, account, Mutex::new(None)))
    }

    pub(crate) async fn inspect_setup_folder(
        &self,
        config: &ConnectionConfig,
        secret: &SecretRef,
        account_id: &str,
        folder_id: &str,
        cancel: &Cancellation,
    ) -> Result<SetupFolder> {
        let (settings, account, token) =
            self.setup_context(config, secret, account_id, folder_id)?;
        let session = Session {
            settings: &settings,
            secret,
            account: &account,
            token: &token,
        };
        let file: DriveFile = self
            .control(
                session,
                &with_query(
                    settings.api(&format!("/files/{folder_id}"))?,
                    &[("fields", "id,name,mimeType,trashed,driveId")],
                ),
                ProviderOperation::Metadata,
                cancel,
            )
            .await
            .map_err(|error| match error.kind {
                ErrorKind::NotFound | ErrorKind::Unauthorized => {
                    ProviderError::new(ErrorKind::FolderInaccessible)
                }
                _ => error,
            })?;
        if file.file_id()? != folder_id
            || file.mime_type.as_deref() != Some(FOLDER_MIME)
            || file.trashed == Some(true)
        {
            return Err(ProviderError::new(ErrorKind::FolderInaccessible));
        }
        if file.drive_id.as_ref().is_some_and(|id| !id.is_empty()) {
            return Err(ProviderError::new(ErrorKind::FolderUnsupportedLocation));
        }
        let name = file
            .name
            .filter(|name| !name.is_empty())
            .ok_or_else(corrupt)?;
        Ok(SetupFolder {
            id: folder_id.to_owned(),
            name,
        })
    }

    pub(crate) async fn create_setup_folder(
        &self,
        config: &ConnectionConfig,
        secret: &SecretRef,
        account_id: &str,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<SetupFolder> {
        let (settings, account, token) =
            self.setup_context(config, secret, account_id, "pending")?;
        let session = Session {
            settings: &settings,
            secret,
            account: &account,
            token: &token,
        };
        let url = with_query(
            settings.api("/files")?,
            &[("fields", "id,name,mimeType,trashed,driveId")],
        );
        let body =
            serde_json::to_vec(&serde_json::json!({ "name": name, "mimeType": FOLDER_MIME }))
                .map_err(|_| corrupt())?;
        let access = self.token(session, None, cancel).await?;
        let mut headers = authorized_headers(&access);
        headers.insert(
            "content-type".into(),
            "application/json; charset=UTF-8".into(),
        );
        let length = body.len() as u64;
        let request = HttpRequest {
            method: reqwest::Method::POST,
            url,
            headers,
            body: Some(Box::pin(std::io::Cursor::new(body))),
            content_length: Some(length),
            operation: ProviderOperation::Create,
            account: account.clone(),
            api_request: true,
            mybox_charge: None,
            control: true,
        };
        let mut response = self.dispatch(request, cancel).await?;
        if let Err(error) = self
            .require_status(&mut response, &[200, 201], &account, cancel)
            .await
        {
            return Err(match error.kind {
                ErrorKind::Cancelled | ErrorKind::ReauthRequired | ErrorKind::Unauthorized => error,
                _ => ProviderError::new(ErrorKind::FolderCreateFailed),
            });
        }
        let file: DriveFile = wire::json(&mut response, cancel).await?;
        if file.mime_type.as_deref() != Some(FOLDER_MIME)
            || file.trashed == Some(true)
            || file.drive_id.is_some()
        {
            return Err(ProviderError::new(ErrorKind::FolderCreateFailed));
        }
        let id = file
            .id
            .filter(|id| config::is_drive_id(id))
            .ok_or_else(|| ProviderError::new(ErrorKind::FolderCreateFailed))?;
        let returned_name = file
            .name
            .filter(|value| value == name)
            .ok_or_else(|| ProviderError::new(ErrorKind::FolderCreateFailed))?;
        Ok(SetupFolder {
            id,
            name: returned_name,
        })
    }
    fn now_ms(&self) -> u64 {
        self.dependencies.clock.now_ms()
    }
    async fn dispatch(&self, request: HttpRequest, cancel: &Cancellation) -> Result<HttpResponse> {
        let mut request = request;
        if http::control_operation(request.operation) {
            http::bypass_cache(&mut request.headers);
        }
        self.dependencies.send(request, cancel).await
    }
    async fn classify_response(
        &self,
        response: &mut HttpResponse,
        account: &AccountKey,
        cancel: &Cancellation,
    ) -> Result<ProviderError> {
        let now = self.now_ms();
        let error = wire::classify(response, now, cancel).await;
        self.dependencies.requests.observe_classified_error(
            account,
            &error,
            &response.headers,
            now,
        )?;
        Ok(error)
    }
    async fn require_status(
        &self,
        response: &mut HttpResponse,
        allowed: &[u16],
        account: &AccountKey,
        cancel: &Cancellation,
    ) -> Result<()> {
        if allowed.contains(&response.status) {
            Ok(())
        } else {
            Err(self.classify_response(response, account, cancel).await?)
        }
    }
    async fn token(
        &self,
        session: Session<'_>,
        rejected: Option<&str>,
        cancel: &Cancellation,
    ) -> Result<zeroize::Zeroizing<String>> {
        auth::access_token(
            &self.dependencies,
            session.settings,
            session.secret,
            session.account,
            session.token,
            rejected,
            cancel,
        )
        .await
    }

    /// Bodyless control request. A rejected access token is refreshed once and
    /// the same request is repeated; no other status is retried here.
    async fn control_request(
        &self,
        session: Session<'_>,
        method: reqwest::Method,
        url: &url::Url,
        operation: ProviderOperation,
        allowed: &[u16],
        cancel: &Cancellation,
    ) -> Result<HttpResponse> {
        let mut refreshed = false;
        loop {
            let token = self.token(session, None, cancel).await?;
            let request = HttpRequest {
                method: method.clone(),
                url: url.clone(),
                headers: authorized_headers(&token),
                body: None,
                content_length: None,
                operation,
                account: session.account.clone(),
                api_request: true,
                mybox_charge: None,
                control: http::control_operation(operation),
            };
            let mut response = self.dispatch(request, cancel).await?;
            if response.status == 401 && !refreshed {
                refreshed = true;
                self.token(session, Some(token.as_str()), cancel).await?;
                continue;
            }
            self.require_status(&mut response, allowed, session.account, cancel)
                .await?;
            return Ok(response);
        }
    }

    async fn control<T: serde::de::DeserializeOwned>(
        &self,
        session: Session<'_>,
        url: &url::Url,
        operation: ProviderOperation,
        cancel: &Cancellation,
    ) -> Result<T> {
        let mut response = self
            .control_request(
                session,
                reqwest::Method::GET,
                url,
                operation,
                &[200],
                cancel,
            )
            .await?;
        wire::json(&mut response, cancel).await
    }

    fn role_query(&self, settings: &Settings, role: &str) -> String {
        format!(
            "'{}' in parents and trashed = false and appProperties has {{ key='{ROLE_KEY}' and value='{role}' }}",
            settings.folder_id
        )
    }
    fn list_url(
        &self,
        settings: &Settings,
        query: &str,
        pairs: &[(&str, &str)],
    ) -> Result<url::Url> {
        let mut parameters: Vec<(&str, &str)> = vec![("q", query), ("fields", LIST_FIELDS)];
        parameters.extend_from_slice(pairs);
        if settings.space == Space::AppData {
            parameters.push(("spaces", config::APP_DATA_FOLDER));
        }
        Ok(with_query(settings.api("/files")?, &parameters))
    }

    async fn list_control_files(
        &self,
        session: Session<'_>,
        query: &str,
        maximum: usize,
        overflow: ErrorKind,
        cancel: &Cancellation,
    ) -> Result<Vec<DriveFile>> {
        let mut files = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen = BTreeSet::new();
        loop {
            cancel.check()?;
            let mut parameters = vec![("pageSize", CONTROL_PAGE_SIZE), ("orderBy", "name")];
            if let Some(token) = cursor.as_deref() {
                parameters.push(("pageToken", token));
            }
            let url = self.list_url(session.settings, query, &parameters)?;
            let page: FileList = self
                .control(session, &url, ProviderOperation::List, cancel)
                .await?;
            page.validate_page(cursor.as_deref())?;
            if files.len().saturating_add(page.files.len()) > maximum {
                return Err(ProviderError::new(overflow));
            }
            files.extend(page.files);
            let Some(next) = page.next_page_token else {
                return Ok(files);
            };
            if !seen.insert(next.clone()) {
                return Err(corrupt());
            }
            cursor = Some(next);
        }
    }

    async fn generate_id(&self, session: Session<'_>, cancel: &Cancellation) -> Result<String> {
        let url = with_query(
            session.settings.api("/files/generateIds")?,
            &[("count", "1"), ("space", session.settings.space.as_str())],
        );
        let generated: GeneratedIds = self
            .control(session, &url, ProviderOperation::Metadata, cancel)
            .await?;
        generated
            .ids
            .into_iter()
            .find(|id| config::is_drive_id(id))
            .ok_or_else(corrupt)
    }

    fn object_metadata(&self, settings: &Settings, intent: &ObjectIntent, file_id: &str) -> String {
        let role = role_token(intent.role);
        let mut properties = serde_json::json!({ ROLE_KEY: role, JOB_KEY: intent.job_id });
        let name = if intent.role == ObjectRole::Segment {
            let (writer, seq, _) = crate::external_storage::contract::parse_segment_object_id(&intent.object_id).unwrap();
            properties["writerId"] = writer.into();
            properties["seq"] = seq.to_string().into();
            format!("segments/{}", intent.object_id)
        } else {
            properties[OBJECT_KEY] = intent.object_id.clone().into();
            if intent.role == ObjectRole::Snapshot { format!("snapshots/{}", intent.object_id) }
            else { format!("{role}-{}", intent.object_id) }
        };
        serde_json::json!({ "id": file_id, "name": name, "mimeType": OCTET_STREAM,
            "parents": [settings.folder_id], "appProperties": properties }).to_string()
    }

    fn check_intent(&self, intent: &ObjectIntent) -> Result<()> {
        if intent.byte_length == 0 {
            return Err(corrupt());
        }
        if intent.byte_length > MAX_STORED_BYTES {
            return Err(ProviderError::new(ErrorKind::FileTooLarge));
        }
        let fits = |key: &str, value: &str| key.len() + value.len() <= config::MAX_PROPERTY_BYTES;
        if (intent.role != ObjectRole::Segment && !fits(OBJECT_KEY, &intent.object_id))
            || !fits(JOB_KEY, &intent.job_id)
            || !fits(ROLE_KEY, role_token(intent.role))
        {
            return Err(config::unsupported());
        }
        Ok(())
    }

    async fn open_session(
        &self,
        session: Session<'_>,
        intent: &ObjectIntent,
        file_id: &str,
        cancel: &Cancellation,
    ) -> Result<url::Url> {
        let url = with_query(
            session.settings.upload("/files")?,
            &[("uploadType", "resumable"), ("fields", FILE_FIELDS)],
        );
        let token = self.token(session, None, cancel).await?;
        let metadata = self.object_metadata(session.settings, intent, file_id);
        let length = metadata.len() as u64;
        let mut headers = authorized_headers(&token);
        headers.insert(
            "content-type".to_owned(),
            "application/json; charset=UTF-8".to_owned(),
        );
        headers.insert("x-upload-content-type".to_owned(), OCTET_STREAM.to_owned());
        headers.insert(
            "x-upload-content-length".to_owned(),
            intent.byte_length.to_string(),
        );
        let request = HttpRequest {
            method: reqwest::Method::POST,
            url: url.clone(),
            headers,
            body: Some(Box::pin(std::io::Cursor::new(metadata.into_bytes()))),
            content_length: Some(length),
            operation: ProviderOperation::UploadSession,
            account: session.account.clone(),
            api_request: true,
            mybox_charge: None,
            control: true,
        };
        let mut response = self.dispatch(request, cancel).await?;
        self.require_status(&mut response, &[200, 201], session.account, cancel)
            .await?;
        let location = response.headers.get("location").ok_or_else(corrupt)?;
        let session_uri = url::Url::options()
            .base_url(Some(&url))
            .parse(location)
            .map_err(|_| corrupt())?;
        // The session URI carries its own upload credential; keeping it on the
        // configured origin also keeps the Authorization header on that origin.
        if !session.settings.same_origin(&session_uri)
            || !session_uri.username().is_empty()
            || session_uri.password().is_some()
            || session_uri.fragment().is_some()
        {
            return Err(corrupt());
        }
        Ok(session_uri)
    }

    async fn seal_upload(&self, upload: &SealedUpload) -> Result<SecretRef> {
        let payload = serde_json::json!({
            "fileId": upload.file_id.clone(),
            "sessionUri": upload.session_uri.as_ref().map(|uri| uri.as_str()),
            "intent": upload.intent,
        })
        .to_string();
        let bytes = SecretBytes(zeroize::Zeroizing::new(payload.into_bytes()));
        self.dependencies.vault.store(&bytes).await
    }
    async fn open_sealed(
        &self,
        reference: &SecretRef,
        settings: &Settings,
    ) -> Result<SealedUpload> {
        let bytes = self.dependencies.vault.read(reference).await?;
        let stored: StoredUpload = serde_json::from_slice(&bytes.0).map_err(|_| corrupt())?;
        let session_uri = stored.session_uri.as_deref().map(url::Url::parse).transpose().map_err(|_| corrupt())?;
        if !config::is_drive_id(&stored.file_id) || session_uri.as_ref().is_some_and(|uri| !settings.same_origin(uri)) {
            return Err(corrupt());
        }
        Ok(SealedUpload {
            file_id: stored.file_id,
            session_uri,
            intent: stored.intent,
        })
    }

    async fn session_status(
        &self,
        session: Session<'_>,
        session_uri: &url::Url,
        total: u64,
        cancel: &Cancellation,
    ) -> Result<SessionStatus> {
        let token = self.token(session, None, cancel).await?;
        let mut headers = authorized_headers(&token);
        headers.insert("content-range".to_owned(), format!("bytes */{total}"));
        let request = HttpRequest {
            method: reqwest::Method::PUT,
            url: session_uri.clone(),
            headers,
            body: None,
            content_length: Some(0),
            operation: ProviderOperation::ReconcileUpload,
            account: session.account.clone(),
            api_request: true,
            mybox_charge: None,
            control: true,
        };
        let mut response = self.dispatch(request, cancel).await?;
        match response.status {
            200 | 201 => Ok(SessionStatus::Complete(
                wire::json(&mut response, cancel).await?,
            )),
            308 => Ok(SessionStatus::Incomplete(parse_confirmed_offset(
                &response.headers,
            )?)),
            404 | 410 => Ok(SessionStatus::Gone),
            _ => Err(self
                .classify_response(&mut response, session.account, cancel)
                .await?),
        }
    }

    async fn upload_chunks(
        &self,
        session: Session<'_>,
        intent: &ObjectIntent,
        source: &dyn TransferSource,
        upload: &SealedUpload,
        start_offset: u64,
        cancel: &Cancellation,
    ) -> Result<ObjectReceipt> {
        let session_uri = upload.session_uri.as_ref().ok_or_else(corrupt)?;
        let total = intent.byte_length;
        let mut offset = start_offset;
        let mut refreshed = false;
        if offset > total {
            return Err(corrupt());
        }
        while offset < total {
            cancel.check()?;
            let length = UPLOAD_CHUNK_BYTES.min(total - offset);
            let reader = source.open(offset, length, cancel).await?;
            let token = self.token(session, None, cancel).await?;
            let mut headers = authorized_headers(&token);
            headers.insert(
                "content-range".to_owned(),
                format!("bytes {}-{}/{total}", offset, offset + length - 1),
            );
            headers.insert("content-type".to_owned(), OCTET_STREAM.to_owned());
            let request = HttpRequest {
                method: reqwest::Method::PUT,
                url: session_uri.clone(),
                headers,
                body: Some(reader),
                content_length: Some(length),
                operation: ProviderOperation::UploadChunk,
                account: session.account.clone(),
                api_request: true,
                mybox_charge: None,
                control: false,
            };
            let mut response = self.dispatch(request, cancel).await?;
            if response.status == 401 && !refreshed {
                refreshed = true;
                self.token(session, Some(token.as_str()), cancel).await?;
                match self.session_status(session, session_uri, total, cancel).await? {
                    SessionStatus::Complete(file) => return self.completed_receipt(
                        session.settings, intent, &file, Some(&upload.file_id)),
                    SessionStatus::Incomplete(confirmed) if confirmed <= total => offset = confirmed,
                    SessionStatus::Incomplete(_) => return Err(corrupt()),
                    SessionStatus::Gone => return Err(ProviderError::new(ErrorKind::NotFound)),
                }
                continue;
            }
            match response.status {
                200 | 201 => {
                    let file: DriveFile = wire::json(&mut response, cancel).await?;
                    return self.completed_receipt(
                        session.settings,
                        intent,
                        &file,
                        Some(&upload.file_id),
                    );
                }
                308 => {
                    let confirmed = parse_confirmed_offset(&response.headers)?;
                    if confirmed <= offset {
                        return Err(corrupt());
                    }
                    offset = confirmed;
                }
                404 | 410 => {
                    return Err(common::error(ErrorKind::NotFound, response.status));
                }
                _ => {
                    return Err(self
                        .classify_response(&mut response, session.account, cancel)
                        .await?)
                }
            }
        }
        match self
            .session_status(session, session_uri, total, cancel)
            .await?
        {
            SessionStatus::Complete(file) => {
                self.completed_receipt(session.settings, intent, &file, Some(&upload.file_id))
            }
            SessionStatus::Incomplete(_) => Err(ProviderError::new(ErrorKind::Transient)),
            SessionStatus::Gone => Err(ProviderError::new(ErrorKind::NotFound)),
        }
    }

    /// A finished upload is only complete when the service reports the file
    /// this job addressed, its exact length, and the digest it computed itself
    /// when it exposes one.
    fn completed_receipt(
        &self,
        settings: &Settings,
        intent: &ObjectIntent,
        file: &DriveFile,
        expected_id: Option<&str>,
    ) -> Result<ObjectReceipt> {
        let file_id = file.file_id()?;
        if expected_id.is_some_and(|expected| expected != file_id)
            || file.byte_length()? != intent.byte_length
        {
            return Err(corrupt());
        }
        let checksum = file.checksum(Some(&intent.sha256));
        if checksum
            .as_ref()
            .is_some_and(|checksum| !checksum.provider_verified)
        {
            return Err(corrupt());
        }
        Ok(ObjectReceipt {
            locator: object_locator(settings, intent, file)?,
            byte_length: intent.byte_length,
            version: file.version_token(),
            checksum: checksum.or_else(|| Some(Checksum { algorithm: "sha256".into(), value: intent.sha256.clone(), provider_verified: false })),
            complete: true,
        })
    }

    /// An earlier attempt may have stored the object before its response was
    /// lost. Identical bytes converge; different bytes never overwrite.
    async fn verify_file(&self, session: Session<'_>, intent: &ObjectIntent, file: &DriveFile,
        cancel: &Cancellation) -> Result<()> {
        matches_intent(intent, file)?;
        let token = self.token(session, None, cancel).await?;
        let mut response = self.dispatch(HttpRequest {
            method: reqwest::Method::GET,
            url: with_query(session.settings.api(&format!("/files/{}", file.file_id()?))?, &[("alt", "media")]),
            headers: authorized_headers(&token), body: None, content_length: None,
            operation: ProviderOperation::Get, account: session.account.clone(), api_request: true,
            mybox_charge: None, control: false,
        }, cancel).await?;
        self.require_status(&mut response, &[200], session.account, cancel).await?;
        let mut sink = crate::external_storage::contract::IdentitySink { intent };
        common::stream_to_sink(&mut response.body, &mut sink, Some(intent.byte_length), intent.byte_length, cancel).await?;
        Ok(())
    }

    async fn existing_object(&self, session: Session<'_>, intent: &ObjectIntent,
        cancel: &Cancellation) -> Result<Option<DriveFile>> {
        let files = self.existing_files(session, intent, cancel).await?;
        for file in &files { self.verify_file(session, intent, file, cancel).await?; }
        Ok(files.into_iter().next())
    }

    async fn existing_files(&self, session: Session<'_>, intent: &ObjectIntent,
        cancel: &Cancellation) -> Result<Vec<DriveFile>> {
        let selector = if intent.role == ObjectRole::Segment {
            let (writer, seq, _) = crate::external_storage::contract::parse_segment_object_id(&intent.object_id)?;
            format!("name contains 'segments/{writer}-{seq}-'")
        } else {
            format!("appProperties has {{ key='{OBJECT_KEY}' and value='{}' }}", config::escape_query_literal(&intent.object_id)?)
        };
        let query = format!("'{}' in parents and trashed = false and {selector}", session.settings.folder_id);
        let mut files = self.list_control_files(session, &query, usize::MAX, ErrorKind::Corrupt, cancel).await?;
        files.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(files)
    }

    async fn file_by_id(&self, session: Session<'_>, file_id: &str, cancel: &Cancellation) -> Result<Option<DriveFile>> {
        let result = self.control(session, &with_query(session.settings.api(&format!("/files/{file_id}"))?,
            &[("fields", FILE_FIELDS)]), ProviderOperation::Metadata, cancel).await;
        match result { Ok(file) => Ok(Some(file)), Err(error) if error.kind == ErrorKind::NotFound => Ok(None), Err(error) => Err(error) }
    }

    async fn multipart_create(
        &self,
        session: Session<'_>,
        intent: &ObjectIntent,
        source: &dyn TransferSource,
        file_id: &str,
        cancel: &Cancellation,
    ) -> Result<ObjectReceipt> {
        let url = with_query(
            session.settings.upload("/files")?,
            &[("uploadType", "multipart"), ("fields", FILE_FIELDS)],
        );
        let metadata = self.object_metadata(session.settings, intent, file_id);
        let mut refreshed = false;
        loop {
        let content = source.open(0, intent.byte_length, cancel).await?;
        let token = self.token(session, None, cancel).await?;
        let (body, length, content_type) = multipart_body(&metadata, content, intent.byte_length);
        let mut headers = authorized_headers(&token);
        headers.insert("content-type".to_owned(), content_type);
        let request = HttpRequest {
            method: reqwest::Method::POST,
            url: url.clone(),
            headers,
            body: Some(body),
            content_length: Some(length),
            operation: ProviderOperation::Create,
            account: session.account.clone(),
            api_request: true,
            mybox_charge: None,
            control: false,
        };
        let mut response = self.dispatch(request, cancel).await?;
        if response.status == 401 && !refreshed {
            refreshed = true;
            self.token(session, Some(token.as_str()), cancel).await?;
            continue;
        }
        self.require_status(&mut response, &[200, 201], session.account, cancel)
            .await?;
        let file: DriveFile = wire::json(&mut response, cancel).await?;
        return self.completed_receipt(session.settings, intent, &file, Some(file_id));
        }
    }
}

fn multipart_body(
    metadata: &str,
    content: Pin<Box<dyn AsyncRead + Send>>,
    content_length: u64,
) -> (Pin<Box<dyn AsyncRead + Send>>, u64, String) {
    let boundary = format!("risunest-{}", risunest_sync_wire::hash(metadata.as_bytes()));
    let prefix = format!(
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: {OCTET_STREAM}\r\n\r\n"
    );
    let suffix = format!("\r\n--{boundary}--");
    let length = prefix.len() as u64 + content_length + suffix.len() as u64;
    let body = std::io::Cursor::new(prefix.into_bytes())
        .chain(content)
        .chain(std::io::Cursor::new(suffix.into_bytes()));
    (
        Box::pin(body),
        length,
        format!("multipart/related; boundary={boundary}"),
    )
}

fn context(repository: &RepositoryHandle) -> Result<&Context> {
    let context = repository
        .context
        .downcast_ref::<Context>()
        .ok_or_else(corrupt)?;
    if context.settings.connection_identity != repository.connection_identity {
        return Err(corrupt());
    }
    Ok(context)
}

/// The checks an existing file passes before its bytes are compared.
fn matches_intent(intent: &ObjectIntent, file: &DriveFile) -> Result<()> {
    if file.byte_length()? != intent.byte_length || file.property(ROLE_KEY) != Some(role_token(intent.role)) {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    if intent.role == ObjectRole::Segment {
        segment_locator_object(file.file_id()?, &intent.object_id)?;
        if file.name.as_deref() != Some(format!("segments/{}", intent.object_id).as_str()) {
            return Err(ProviderError::new(ErrorKind::PreconditionFailed));
        }
    }
    Ok(())
}

fn object_locator(settings: &Settings, intent: &ObjectIntent, file: &DriveFile) -> Result<RemoteLocator> {
    let file_id = file.file_id()?;
    let object = if intent.role == ObjectRole::Segment {
        if segment_file_name(file)? != intent.object_id { return Err(corrupt()); }
        segment_locator_object(file_id, &intent.object_id)?
    } else { file_id.to_owned() };
    Ok(RemoteLocator {
        connection_identity: settings.connection_identity.clone(),
        collection: collection_token(intent.role).map(str::to_owned),
        object,
    })
}

fn resolve_file_id(context: &Context, locator: &RemoteLocator) -> Result<String> {
    if let Some((file_id, _)) = segment_locator_parts(locator)? { return Ok(file_id.to_owned()); }
    if locator.object == HEAD_OBJECT {
        let head = context.head.lock().unwrap();
        return if head.present {
            Ok(head.file_id.clone())
        } else {
            Err(ProviderError::new(ErrorKind::NotFound))
        };
    }
    if !config::is_drive_id(&locator.object) {
        return Err(corrupt());
    }
    Ok(locator.object.clone())
}

/// A member of this repository folder whose role is a cleanup target. The head
/// and the descriptor role are never removed here.
fn removable(settings: &Settings, file: &DriveFile) -> bool {
    let parented = file
        .parents
        .as_ref()
        .is_some_and(|parents| parents.iter().any(|id| *id == settings.folder_id));
    parented
        && file.property(ROLE_KEY).is_some_and(|role| {
            [
                ObjectRole::Segment,
                ObjectRole::Snapshot,
                ObjectRole::Pack,
                ObjectRole::Catalog,
                ObjectRole::SyncState,
                ObjectRole::BackupBundle,
                ObjectRole::BackupPoint,
                ObjectRole::InventoryPage,
                ObjectRole::Lease,
            ]
            .iter()
            .any(|known| role_token(*known) == role)
        })
}

fn validate_resumable_descriptor(file: &DriveFile) -> Result<()> {
    if !config::is_drive_id(file.file_id()?)
        || file.byte_length()? == 0
        || file.version_token().is_none()
    {
        return Err(corrupt());
    }
    let object_id = file.property(OBJECT_KEY).ok_or_else(corrupt)?;
    if !config::is_drive_id(object_id) || file.property(JOB_KEY) != Some(object_id) {
        return Err(corrupt());
    }
    if file
        .sha256_checksum
        .as_deref()
        .is_some_and(|checksum| !crate::trust_boundary::is_lower_hex_256(checksum))
    {
        return Err(corrupt());
    }
    Ok(())
}

fn capabilities() -> Capabilities {
    Capabilities {
        immutable_create: true,
        direct_complete_read: true,
        // The read-only change counter is not an exchange token.
        atomic_create_head: false,
        conditional_head_update: false,
        stable_head_replace: true,
        head_read_after_write: true,
        head_retry_control: true,
        snapshot_discovery: true,
        lease_operations: true,
        delete_objects: true,
        conditional_get: false,
        range: true,
        resumable_upload: true,
        max_stored_bytes: Some(MAX_STORED_BYTES),
        sdk_overhead_bytes: 0,
        upload_alignment: UPLOAD_ALIGNMENT,
    }
}

impl Provider for GoogleDrive {
    fn open_repository<'a>(
        &'a self,
        config: &'a ConnectionConfig,
        secret: &'a SecretRef,
        mode: OpenMode,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, (RepositoryHandle, Capabilities)> {
        Box::pin(async move {
            cancel.check()?;
            let settings = Settings::parse(config)?;
            let account = AccountKey::new(
                config::PROVIDER_ID,
                &settings.api("/")?,
                &settings.account_id,
            )?;
            let token = Mutex::new(None);
            let session = Session {
                settings: &settings,
                secret,
                account: &account,
                token: &token,
            };
            let about: About = self
                .control(
                    session,
                    &with_query(settings.api("/about")?, &[("fields", "user(permissionId)")]),
                    ProviderOperation::Metadata,
                    cancel,
                )
                .await?;
            let identity = about.user.and_then(|user| user.permission_id);
            if identity.as_deref() != Some(settings.account_id.as_str()) {
                return Err(ProviderError::new(ErrorKind::Unauthorized));
            }
            if settings.space == Space::Drive {
                let folder: DriveFile = self
                    .control(
                        session,
                        &with_query(
                            settings.api(&format!("/files/{}", settings.folder_id))?,
                            &[("fields", "id,mimeType,trashed")],
                        ),
                        ProviderOperation::Metadata,
                        cancel,
                    )
                    .await?;
                if folder.mime_type.as_deref() != Some(FOLDER_MIME) || folder.trashed == Some(true)
                {
                    return Err(ProviderError::new(ErrorKind::NotFound));
                }
            }
            let query = if matches!(mode, OpenMode::Create | OpenMode::ResumeCreate) {
                format!("'{}' in parents and trashed = false", settings.folder_id)
            } else {
                format!(
                    "'{}' in parents and trashed = false and (appProperties has {{ key='{ROLE_KEY}' and value='{HEAD_ROLE}' }} or appProperties has {{ key='{ROLE_KEY}' and value='{DESCRIPTOR_ROLE}' }})",
                    settings.folder_id
                )
            };
            let control = if matches!(mode, OpenMode::Create) {
                let url = self.list_url(&settings, &query, &[("pageSize", "1")])?;
                let page: FileList = self.control(session, &url, ProviderOperation::List, cancel).await?;
                page.validate_page(None)?;
                if !page.files.is_empty() || page.next_page_token.is_some() {
                    return Err(ProviderError::new(ErrorKind::PreconditionFailed));
                }
                Vec::new()
            } else {
                let overflow = if matches!(mode, OpenMode::ResumeCreate) {
                    ErrorKind::PreconditionFailed
                } else { ErrorKind::Corrupt };
                self.list_control_files(session, &query, 3, overflow, cancel).await?
            };
            let heads: Vec<&DriveFile> = control
                .iter()
                .filter(|file| file.property(ROLE_KEY) == Some(HEAD_ROLE))
                .collect();
            let descriptors: Vec<&DriveFile> = control
                .iter()
                .filter(|file| file.property(ROLE_KEY) == Some(DESCRIPTOR_ROLE))
                .collect();
            if heads.len() > 1 {
                // Drive names are not unique; two control files are ambiguous
                // and must never be resolved by picking one of them.
                return Err(corrupt());
            }
            match mode {
                OpenMode::Create if !control.is_empty() => {
                    return Err(ProviderError::new(ErrorKind::PreconditionFailed));
                }
                OpenMode::ResumeCreate
                    if !heads.is_empty()
                        || descriptors.len() > 2
                        || heads.len() + descriptors.len() != control.len() =>
                {
                    return Err(ProviderError::new(ErrorKind::PreconditionFailed));
                }
                OpenMode::ResumeCreate => {
                    for descriptor in descriptors {
                        validate_resumable_descriptor(descriptor)?;
                    }
                }
                OpenMode::Existing if descriptors.is_empty() => {
                    return Err(ProviderError::new(ErrorKind::NotFound));
                }
                _ => {}
            }
            let head = match heads.first() {
                Some(file) => HeadState {
                    file_id: file.file_id()?.to_owned(),
                    present: true,
                },
                None => HeadState {
                    file_id: self.generate_id(session, cancel).await?,
                    present: false,
                },
            };
            let handle = RepositoryHandle {
                repository_id: format!(
                    "{}:{}:{}",
                    config::PROVIDER_ID,
                    settings.account_id,
                    settings.folder_id
                ),
                connection_identity: settings.connection_identity.clone(),
                account: account.clone(),
                context: Box::new(Context {
                    settings,
                    secret: secret.clone(),
                    account,
                    token,
                    head: Mutex::new(head),
                }),
            };
            Ok((handle, capabilities()))
        })
    }

    fn read_object<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        unchanged: Option<&'a VersionToken>,
        sink: &'a mut dyn TransferSink,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ReadReceipt> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            locator.validate_for(repository)?;
            let file_id = resolve_file_id(context, locator)?;
            let segment_name = segment_locator_parts(locator)?.map(|(_, name)| name);
            let session = context.session();
            let metadata: DriveFile = self
                .control(
                    session,
                    &with_query(
                        context.settings.api(&format!("/files/{file_id}"))?,
                        &[("fields", if segment_name.is_some() { FILE_FIELDS } else { "id,size,version,sha256Checksum" })],
                    ),
                    ProviderOperation::Metadata,
                    cancel,
                )
                .await?;
            if let Some(name) = segment_name { validate_segment_file(&metadata, &file_id, name)?; }
            let version = metadata.version_token();
            if let (Some(unchanged), Some(current)) = (unchanged, version.as_ref()) {
                if unchanged == current {
                    return Ok(ReadReceipt::NotModified(current.clone()));
                }
            }
            let length = metadata.byte_length()?;
            let mut refreshed = false;
            let mut response = loop {
            let token = self.token(session, None, cancel).await?;
            let request = HttpRequest {
                method: reqwest::Method::GET,
                url: with_query(
                    context.settings.api(&format!("/files/{file_id}"))?,
                    &[("alt", "media")],
                ),
                headers: authorized_headers(&token),
                body: None,
                content_length: None,
                operation: ProviderOperation::Get,
                account: session.account.clone(),
                api_request: true,
                mybox_charge: None,
                control: false,
            };
            let response = self.dispatch(request, cancel).await?;
            if response.status == 401 && !refreshed {
                refreshed = true;
                self.token(session, Some(token.as_str()), cancel).await?;
                continue;
            }
            break response;
            };
            self.require_status(&mut response, &[200], session.account, cancel)
                .await?;
            let declared = common::content_length(&response.headers)?;
            if declared.is_some_and(|declared| declared != length) {
                return Err(corrupt());
            }
            let (received, hash) =
                common::stream_to_sink(&mut response.body, sink, Some(length), length, cancel)
                    .await?;
            if let Some(name) = segment_name {
                if crate::external_storage::contract::parse_segment_object_id(name)?.2 != hash { return Err(corrupt()); }
            }
            let checksum = metadata.checksum(Some(&hash));
            if checksum
                .as_ref()
                .is_some_and(|checksum| !checksum.provider_verified)
            {
                // The service computed a different digest over the stored bytes.
                return Err(corrupt());
            }
            Ok(ReadReceipt::Body(ObjectReceipt {
                locator: locator.clone(),
                byte_length: received,
                version,
                checksum,
                complete: true,
            }))
        })
    }

    fn begin_upload<'a>(&'a self, repository: &'a RepositoryHandle, intent: &'a ObjectIntent,
        cancel: &'a Cancellation) -> ProviderFuture<'a, Option<ResumeState>> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            intent.validate(repository)?;
            self.check_intent(intent)?;
            if intent.role == ObjectRole::Descriptor && self.existing_object(context.session(), intent, cancel).await?.is_some() {
                return Ok(None);
            }
            let file_id = self.generate_id(context.session(), cancel).await?;
            let sealed_state = self.seal_upload(&SealedUpload { file_id, session_uri: None, intent: intent.clone() }).await?;
            Ok(Some(ResumeState { data: sealed_state.into(), confirmed_offset: 0, expires_at_ms: None }))
        })
    }

    fn create_object<'a>(&'a self, repository: &'a RepositoryHandle, intent: &'a ObjectIntent,
        source: &'a dyn TransferSource, resume: Option<&'a ResumeState>, cancel: &'a Cancellation)
        -> ProviderFuture<'a, ObjectReceipt> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            intent.validate(repository)?;
            self.check_intent(intent)?;
            crate::external_storage::contract::verify_source(source, intent, cancel).await?;
            let session = context.session();
            let Some(resume) = resume else {
                return match self.existing_object(session, intent, cancel).await? {
                    Some(file) => self.completed_receipt(&context.settings, intent, &file, None),
                    None => Err(ProviderError::new(ErrorKind::Unsupported)),
                };
            };
            let mut upload = self.open_sealed(&resume.data.secret()?, &context.settings).await?;
            if upload.intent != *intent { return Err(corrupt()); }
            if upload.session_uri.is_none() && intent.byte_length <= MULTIPART_MAX_BYTES {
                return match self.multipart_create(session, intent, source, &upload.file_id, cancel).await {
                    Err(error) if error.kind == ErrorKind::PreconditionFailed => {
                        let file = self.file_by_id(session, &upload.file_id, cancel).await?.ok_or(error)?;
                        self.verify_file(session, intent, &file, cancel).await?;
                        self.completed_receipt(&context.settings, intent, &file, Some(&upload.file_id))
                    }
                    result => result,
                };
            }
            if upload.session_uri.is_none() {
                upload.session_uri = Some(self.open_session(session, intent, &upload.file_id, cancel).await?);
                let bytes = SecretBytes(zeroize::Zeroizing::new(serde_json::json!({
                    "fileId": upload.file_id, "sessionUri": upload.session_uri.as_ref().map(|uri| uri.as_str()),
                    "intent": upload.intent }).to_string().into_bytes()));
                self.dependencies.vault.replace(&resume.data.secret()?, &bytes).await?;
            }
            self.upload_chunks(session, intent, source, &upload, resume.confirmed_offset, cancel).await
        })
    }

    fn compare_exchange_head<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        _expected: &'a ExpectedHead,
        _head: &'a HeadBytes,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, HeadReceipt> {
        Box::pin(async move {
            cancel.check()?;
            context(repository)?;
            locator.validate_for(repository)?;
            Err(config::unsupported())
        })
    }

    fn replace_head<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        head: &'a HeadBytes,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, HeadReceipt> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            locator.validate_for(repository)?;
            if locator.object != HEAD_OBJECT {
                return Err(corrupt());
            }
            let session = context.session();
            let (file_id, present) = {
                let head = context.head.lock().unwrap();
                (head.file_id.clone(), head.present)
            };
            let bytes = head.as_bytes().to_vec();
            let length = bytes.len() as u64;
            let token = self.token(session, None, cancel).await?;
            let mut headers = authorized_headers(&token);
            let request = if present {
                headers.insert("content-type".to_owned(), OCTET_STREAM.to_owned());
                HttpRequest {
                    method: reqwest::Method::PATCH,
                    url: with_query(
                        context.settings.upload(&format!("/files/{file_id}"))?,
                        &[("uploadType", "media"), ("fields", FILE_FIELDS)],
                    ),
                    headers,
                    body: Some(Box::pin(std::io::Cursor::new(bytes))),
                    content_length: Some(length),
                    operation: ProviderOperation::ReplaceHead,
                    account: session.account.clone(),
                    api_request: true,
                    mybox_charge: None,
                    control: true,
                }
            } else {
                let metadata = serde_json::json!({
                    "id": file_id,
                    "name": HEAD_OBJECT,
                    "mimeType": OCTET_STREAM,
                    "parents": [context.settings.folder_id.clone()],
                    "appProperties": { ROLE_KEY: HEAD_ROLE },
                })
                .to_string();
                let (body, total, content_type) =
                    multipart_body(&metadata, Box::pin(std::io::Cursor::new(bytes)), length);
                headers.insert("content-type".to_owned(), content_type);
                HttpRequest {
                    method: reqwest::Method::POST,
                    url: with_query(
                        context.settings.upload("/files")?,
                        &[("uploadType", "multipart"), ("fields", FILE_FIELDS)],
                    ),
                    headers,
                    body: Some(body),
                    content_length: Some(total),
                    operation: ProviderOperation::ReplaceHead,
                    account: session.account.clone(),
                    api_request: true,
                    mybox_charge: None,
                    control: true,
                }
            };
            // Exactly one write attempt: an ambiguous outcome is reported so the
            // owner re-observes the head instead of writing again.
            let mut response = self.dispatch(request, cancel).await?;
            self.require_status(&mut response, &[200, 201], session.account, cancel)
                .await?;
            let file: DriveFile = wire::json(&mut response, cancel).await?;
            if file.file_id()? != file_id {
                return Err(corrupt());
            }
            context.head.lock().unwrap().present = true;
            Ok(HeadReceipt {
                version: file.version_token(),
                complete: true,
            })
        })
    }

    /// `files.delete` removes the file outright rather than trashing it, so the
    /// target is confirmed to be a removable member of this repository folder
    /// before the request goes out.
    fn delete_object<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        locator: &'a RemoteLocator,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ()> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            locator.validate_for(repository)?;
            if locator.object == HEAD_OBJECT
                || (locator.collection.as_deref() != Some("segments") && !config::is_drive_id(&locator.object)) {
                return Err(ProviderError::new(ErrorKind::Unsupported));
            }
            let file_id = resolve_file_id(context, locator)?;
            let segment_name = segment_locator_parts(locator)?.map(|(_, name)| name);
            let session = context.session();
            let metadata: DriveFile = match self
                .control(
                    session,
                    &with_query(
                        context.settings.api(&format!("/files/{file_id}"))?,
                        &[("fields", if segment_name.is_some() { "id,name,parents,appProperties" } else { "id,parents,appProperties" })],
                    ),
                    ProviderOperation::Metadata,
                    cancel,
                )
                .await
            {
                Ok(metadata) => metadata,
                Err(error) if error.kind == ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            if let Some(name) = segment_name { validate_segment_file(&metadata, &file_id, name)?; }
            if !removable(&context.settings, &metadata) {
                return Err(ProviderError::new(ErrorKind::Unsupported));
            }
            self.control_request(
                session,
                reqwest::Method::DELETE,
                &context.settings.api(&format!("/files/{file_id}"))?,
                ProviderOperation::Delete,
                &[200, 204, 404],
                cancel,
            )
            .await
            .map(|_| ())
        })
    }

    fn list_objects<'a>(
        &'a self,
        repository: &'a RepositoryHandle,
        collection: Collection,
        cursor: Option<&'a str>,
        limit: u16,
        cancel: &'a Cancellation,
    ) -> ProviderFuture<'a, ObjectPage> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            if limit == 0 || limit > 1000 {
                return Err(config::unsupported());
            }
            let role = collection_role(collection);
            let page_size = limit.to_string();
            let mut parameters: Vec<(&str, &str)> =
                vec![("pageSize", &page_size), ("orderBy", "name")];
            if let Some(cursor) = cursor {
                if cursor.is_empty() || cursor.len() > 4096 {
                    return Err(corrupt());
                }
                parameters.push(("pageToken", cursor));
            }
            let query = if collection == Collection::Snapshots {
                format!("'{}' in parents and trashed = false and (appProperties has {{ key='{ROLE_KEY}' and value='state' }} or appProperties has {{ key='{ROLE_KEY}' and value='snapshot' }} or appProperties has {{ key='{ROLE_KEY}' and value='bundle' }})", context.settings.folder_id)
            } else { self.role_query(&context.settings, role_token(role)) };
            let url = self.list_url(&context.settings, &query, &parameters)?;
            let page: FileList = self
                .control(context.session(), &url, ProviderOperation::List, cancel)
                .await?;
            page.validate_page(cursor)?;
            let mut objects = Vec::with_capacity(page.files.len());
            for file in &page.files {
                let object = if collection == Collection::Segments {
                    segment_locator_object(file.file_id()?, segment_file_name(file)?)?
                } else { file.file_id()?.to_owned() };
                objects.push(ObjectReceipt {
                    locator: RemoteLocator {
                        connection_identity: context.settings.connection_identity.clone(),
                        collection: collection_token(role).map(str::to_owned),
                        object,
                    },
                    byte_length: file.byte_length()?,
                    version: file.version_token(),
                    // Nothing was compared here, so a reported digest stays
                    // unverified until the object is read or created.
                    checksum: file.checksum(None),
                    complete: true,
                });
            }
            Ok(ObjectPage {
                objects,
                next_cursor: page.next_page_token,
            })
        })
    }

    fn reconcile_upload<'a>(&'a self, repository: &'a RepositoryHandle, intent: &'a ObjectIntent,
        resume: Option<&'a ResumeState>, cancel: &'a Cancellation) -> ProviderFuture<'a, UploadResolution> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            intent.validate(repository)?;
            let session = context.session();
            let Some(resume) = resume else {
                return match self.existing_object(session, intent, cancel).await? {
                    Some(file) => Ok(UploadResolution::Complete(self.completed_receipt(&context.settings, intent, &file, None)?)),
                    None => Ok(UploadResolution::RestartRequired),
                };
            };
            let mut upload = self.open_sealed(&resume.data.secret()?, &context.settings).await?;
            if upload.intent != *intent { return Err(corrupt()); }
            let found = if let Some(uri) = upload.session_uri.as_ref() {
                match self.session_status(session, uri, intent.byte_length, cancel).await? {
                    SessionStatus::Incomplete(confirmed_offset) if confirmed_offset <= intent.byte_length => {
                        return Ok(UploadResolution::Resumable(ResumeState { data: resume.data.clone(),
                            confirmed_offset, expires_at_ms: resume.expires_at_ms }));
                    }
                    SessionStatus::Incomplete(_) => return Err(corrupt()),
                    SessionStatus::Complete(file) => Some(file),
                    SessionStatus::Gone => self.file_by_id(session, &upload.file_id, cancel).await?,
                }
            } else { self.file_by_id(session, &upload.file_id, cancel).await? };
            let Some(file) = found else {
                upload.session_uri = None;
                let bytes = SecretBytes(zeroize::Zeroizing::new(serde_json::json!({ "fileId": upload.file_id,
                    "sessionUri": null, "intent": upload.intent }).to_string().into_bytes()));
                self.dependencies.vault.replace(&resume.data.secret()?, &bytes).await?;
                return Ok(UploadResolution::Resumable(ResumeState { data: resume.data.clone(), confirmed_offset: 0, expires_at_ms: None }));
            };
            match self.verify_file(session, intent, &file, cancel).await {
                Ok(()) => (),
                Err(error) if error.kind == ErrorKind::PreconditionFailed => return Ok(UploadResolution::Conflict),
                Err(error) => return Err(error),
            }
            Ok(UploadResolution::Complete(self.completed_receipt(&context.settings, intent, &file, Some(&upload.file_id))?))
        })
    }

    fn lookup_metadata<'a>(&'a self, repository: &'a RepositoryHandle, intent: &'a ObjectIntent,
        known: Option<&'a RemoteLocator>, cancel: &'a Cancellation) -> ProviderFuture<'a, Option<ObjectReceipt>> {
        Box::pin(async move {
            cancel.check()?;
            let context = context(repository)?;
            intent.validate(repository)?;
            let session = context.session();
            let (file, locator) = match known {
                Some(locator) => {
                    locator.validate_for(repository)?;
                    let file_id = resolve_file_id(context, locator)?;
                    let Some(file) = self.file_by_id(session, &file_id, cancel).await? else { return Ok(None) };
                    if let Some((_, name)) = segment_locator_parts(locator)? { validate_segment_file(&file, &file_id, name)?; }
                    (file, locator.clone())
                }
                None => {
                    let files = self.existing_files(session, intent, cancel).await?;
                    for file in &files { matches_intent(intent, file)?; }
                    let Some(file) = files.into_iter().next() else { return Ok(None) };
                    let locator = object_locator(&context.settings, intent, &file)?;
                    (file, locator)
                }
            };
            Ok(Some(ObjectReceipt {
                locator,
                byte_length: file.byte_length()?,
                version: file.version_token(),
                checksum: file.checksum(Some(&intent.sha256)),
                complete: true,
            }))
        })
    }

    fn head_locator(&self, repository: &RepositoryHandle) -> Result<RemoteLocator> {
        context(repository)?;
        Ok(RemoteLocator {
            connection_identity: repository.connection_identity.clone(),
            collection: None,
            object: HEAD_OBJECT.to_owned(),
        })
    }
}

pub(super) fn provider(dependencies: Dependencies) -> Arc<dyn Provider> {
    Arc::new(GoogleDrive::new(dependencies))
}
