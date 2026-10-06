//! Native control-plane only. Media bodies travel from the server to WebView.
use super::{
    client::ServerClient,
    residency::{RemoteObject, Residency},
    Result, SyncError,
};
use risunest_sync_connect::media::{
    generate_key, MediaAccess, MediaObject, MediaRequest, MediaSigner,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

struct Grant {
    url: String,
    expires: Instant,
}
type GrantSlot = Arc<Mutex<Option<Grant>>>;
struct Issuer {
    key: String,
    client: ServerClient,
    head: risunest_sync_wire::RemoteHead,
    expires: Instant,
}
struct PendingGrant {
    key: String,
    proof: RemoteObject,
    object: MediaObject,
    slot: GrantSlot,
    result: std::sync::mpsc::Sender<Result<String>>,
}

pub(crate) struct MediaProvider {
    root: PathBuf,
    origin: String,
    signer: MediaSigner,
    grants: Mutex<VecDeque<(String, GrantSlot)>>,
    issuing: Mutex<Option<Issuer>>,
    pending: Mutex<VecDeque<PendingGrant>>,
    // Held open so a burst of lookups does not repeatedly close the last
    // connection, whose WAL teardown makes concurrent opens fail.
    residency: Mutex<Option<Residency>>,
}
impl MediaProvider {
    pub fn new(root: PathBuf, origin: String) -> Result<Self> {
        Ok(Self {
            root,
            origin,
            signer: MediaSigner::new(&generate_key()?)?,
            grants: Mutex::new(VecDeque::new()),
            issuing: Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
            residency: Mutex::new(None),
        })
    }
    fn proof(&self, hash: &str) -> Result<Option<RemoteObject>> {
        let mut residency = self
            .residency
            .lock()
            .map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
        let result = match residency.as_ref() {
            Some(residency) => residency.object(hash, None),
            None => Residency::open(&self.root).and_then(|opened| {
                let result = opened.object(hash, None);
                *residency = Some(opened);
                result
            }),
        };
        if result.is_err() {
            *residency = None;
        }
        result
    }
    pub fn verify_refresh(&self, token: &str) -> Result<MediaObject> {
        Ok(self.signer.verify_refresh(token)?)
    }
    pub fn url(&self, object: &MediaObject, refresh: bool) -> Result<String> {
        object.validate()?;
        let proof = self
            .proof(&object.hash)?
            .filter(|proof| object.size == proof.size.into())
            .ok_or_else(|| SyncError::new("remote-object-unavailable", 404))?;
        let key = format!(
            "{}:{}:{}:{}:{}",
            proof.context, proof.config.device_id, proof.config.endpoint, object.hash, object.mime
        );
        let slot = {
            let mut grants = self
                .grants
                .lock()
                .map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
            if let Some(index) = grants.iter().position(|(candidate, _)| candidate == &key) {
                let entry = grants.remove(index).unwrap();
                let slot = entry.1.clone();
                grants.push_back(entry);
                slot
            } else {
                let slot = Arc::new(Mutex::new(None));
                if grants.len() >= 256 {
                    if let Some(index) = grants
                        .iter()
                        .position(|(_, slot)| Arc::strong_count(slot) == 1)
                    {
                        grants.remove(index);
                    }
                }
                if grants.len() < 256 {
                    grants.push_back((key, slot.clone()));
                }
                slot
            }
        };
        let cached = slot
            .lock()
            .map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
        if !refresh {
            if let Some(grant) = cached
                .as_ref()
                .filter(|grant| grant.expires > Instant::now())
            {
                return Ok(grant.url.clone());
            }
        }
        drop(cached);
        let issuer_key = format!("{}:{}", proof.context, serde_json::to_string(&proof.config)
            .map_err(|_| SyncError::new("invalid-retention-config", 409))?);
        let (send, receive) = std::sync::mpsc::channel();
        {
            let mut pending = self.pending.lock().map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
            if pending.len() >= 256 { return Err(SyncError::new("media-cache-unavailable", 503)); }
            pending.push_back(PendingGrant { key: issuer_key.clone(), proof, object: object.clone(), slot, result: send });
        }
        // A screen can request many distinct images together. Serialize only
        // capability issuance against the server's two authenticated slots;
        // cached URLs and direct browser media transfers remain concurrent.
        let mut issuing = self
            .issuing
            .lock()
            .map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
        if let Ok(result) = receive.try_recv() { return result; }
        loop {
            let batch = {
                let mut pending = self.pending.lock().map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
                let mut batch = Vec::new();
                let mut index = 0;
                while index < pending.len() && batch.len() < 128 {
                    if pending[index].key == issuer_key {
                        batch.push(pending.remove(index).unwrap());
                    } else { index += 1; }
                }
                batch
            };
            if batch.is_empty() { return Err(SyncError::new("media-cache-unavailable", 503)); }
            let outcome = self.issue(&mut issuing, &issuer_key, &batch);
            match outcome {
                Ok(urls) => for (request, url) in batch.into_iter().zip(urls) { let _ = request.result.send(Ok(url)); },
                Err(error) => {
                    *issuing = None;
                    for request in batch { let _ = request.result.send(Err(SyncError::new(&error.code, error.status))); }
                }
            }
            if let Ok(result) = receive.try_recv() { return result; }
        }
    }
    fn issue(&self, issuing: &mut Option<Issuer>, issuer_key: &str, batch: &[PendingGrant]) -> Result<Vec<String>> {
        if issuing.as_ref().is_some_and(|session| session.key != issuer_key || session.expires <= Instant::now()) {
            *issuing = None;
        }
        if issuing.is_none() {
            let client = ServerClient::new(batch[0].proof.config.resolve(&self.root)?)?;
            let head = client.resolve_identity()?;
            *issuing = Some(Issuer { key: issuer_key.to_owned(), client, head, expires: Instant::now() + Duration::from_secs(60) });
        }
        let issuer = issuing.as_ref().unwrap();
        let client = &issuer.client;
        let head = &issuer.head;
        let mut requests: Vec<MediaRequest> = Vec::new();
        for request in batch {
            if !requests.iter().any(|existing| existing.object == request.object) {
                requests.push(MediaRequest {
                    object: request.object.clone(),
                    refresh_url: self.signer.refresh_url(&self.origin, &request.object)?,
                });
            }
        }
        let started = Instant::now();
        let (_, grants): (_, Vec<MediaAccess>) = client.json(
            reqwest::Method::POST,
            "media/access",
            &[],
            Some(&serde_json::json!({"epoch":head.epoch,"requests":requests})),
            &[],
        )?;
        if grants.len() != requests.len() {
            return Err(SyncError::new("invalid-media-access", 502));
        }
        let mut validated = Vec::new();
        for request in batch {
        let access = grants.iter().find(|access| access.object == request.object)
            .ok_or_else(|| SyncError::new("invalid-media-access", 502))?;
        let lifetime = access
            .valid_for_seconds
            .as_str()
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 5 && *v <= 3600)
            .ok_or_else(|| SyncError::new("invalid-media-access", 502))?;
        if access.token.len() > 8192
            || access.token.is_empty()
            || !access
                .token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(SyncError::new("invalid-media-access", 502));
        }
        let endpoint = client.config().validate()?;
        let url = endpoint
            .join(&format!("media/{}", access.token))
            .map_err(|_| SyncError::new("invalid-media-access", 502))?
            .to_string();
        validated.push(Grant {
            url: url.clone(),
            expires: started + Duration::from_secs(lifetime - 5),
        });
        }
        let mut urls = Vec::with_capacity(batch.len());
        for (request, grant) in batch.iter().zip(validated) {
            urls.push(grant.url.clone());
            *request.slot.lock().map_err(|_| SyncError::new("media-cache-unavailable", 503))? = Some(grant);
        }
        Ok(urls)
    }
}
