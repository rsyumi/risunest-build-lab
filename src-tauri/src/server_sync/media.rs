//! Native control-plane only. Media bodies travel from the server to WebView.
use super::{
    client::{AdmissionRetry, ServerClient},
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

pub(crate) struct MediaProvider {
    root: PathBuf,
    origin: String,
    signer: MediaSigner,
    grants: Mutex<VecDeque<(String, GrantSlot)>>,
    issuing: Mutex<()>,
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
            issuing: Mutex::new(()),
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
        let mut cached = slot
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
        // A screen can request many distinct images together. Serialize only
        // capability issuance against the server's two authenticated slots;
        // cached URLs and direct browser media transfers remain concurrent.
        let _issuing = self
            .issuing
            .lock()
            .map_err(|_| SyncError::new("media-cache-unavailable", 503))?;
        // Another request of this device, such as a Sync transfer, can hold both
        // slots. A refusal before admission is retried briefly; exhaustion is a
        // retryable failure and leaves no grant cached.
        let client = ServerClient::new(proof.config.resolve(&self.root)?)?
            .with_admission_retry(Arc::new(AdmissionRetry::new()));
        let head = client.resolve_identity(false)?;
        let request = MediaRequest {
            object: object.clone(),
            refresh_url: self.signer.refresh_url(&self.origin, object)?,
        };
        let started = Instant::now();
        let (_, mut grants): (_, Vec<MediaAccess>) = client.json(
            reqwest::Method::POST,
            "media/access",
            &[],
            Some(&serde_json::json!({"epoch":head.epoch,"requests":[request]})),
            &[],
        )?;
        if grants.len() != 1 {
            return Err(SyncError::new("invalid-media-access", 502));
        }
        let access = grants.pop().unwrap();
        let lifetime = access
            .valid_for_seconds
            .as_str()
            .parse::<u64>()
            .ok()
            .filter(|v| *v > 5 && *v <= 3600)
            .ok_or_else(|| SyncError::new("invalid-media-access", 502))?;
        if access.object != *object
            || access.token.len() > 8192
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
        *cached = Some(Grant {
            url: url.clone(),
            expires: started + Duration::from_secs(lifetime - 5),
        });
        Ok(url)
    }
}
