use crate::{store::Store, Error, Result};
use serde::Serialize;
use std::{sync::{Arc, atomic::{AtomicU64, Ordering}}, time::Duration};
use tokio::sync::{watch, Notify};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationStatus {
    pub phase: &'static str,
    pub error: Option<&'static str>,
}

pub struct Publisher {
    http: reqwest::Client,
    status: watch::Sender<PublicationStatus>,
    retry_after: AtomicU64,
}
impl Publisher {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| Error::new("directory-client-unavailable", 503))?;
        let (status, _) = watch::channel(PublicationStatus {
            phase: "waiting",
            error: None,
        });
        Ok(Self { http, status, retry_after: AtomicU64::new(0) })
    }
    pub fn subscribe(&self) -> watch::Receiver<PublicationStatus> {
        self.status.subscribe()
    }
    pub async fn publish_once(&self, store: &Arc<Store>) -> Result<bool> {
        self.retry_after.store(0, Ordering::Relaxed);
        let planning = store.clone();
        let (publication, status) = tokio::task::spawn_blocking(move || {
            Ok::<_, Error>((planning.plan_publication()?, planning.connection_status()?))
        }).await.map_err(|_| Error::new("connection-state-unavailable", 503))??;
        let Some(publication) = publication else {
            self.status.send_replace(PublicationStatus {
                phase: status.publication,
                error: None,
            });
            return Ok(false);
        };
        self.status.send_replace(PublicationStatus {
            phase: "publishing",
            error: None,
        });
        let mut response = self
            .http
            .post(publication.directory.record_url()?)
            .bearer_auth(&publication.writer)
            .header("content-type", "text/plain; charset=utf-8")
            .header("accept-encoding", "identity")
            .body(publication.envelope.clone())
            .send()
            .await
            .map_err(|_| Error::new("directory-unreachable", 503))?;
        if response.status() != reqwest::StatusCode::NO_CONTENT {
            let status = response.status().as_u16();
            let floor = response.headers().get("retry-after").and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok()).unwrap_or(0).min(3600);
            self.retry_after.store(floor, Ordering::Relaxed);
            let mut body = Vec::new();
            while let Ok(Some(chunk)) = response.chunk().await {
                if body.len() + chunk.len() > 256 { body.clear(); break; }
                body.extend_from_slice(&chunk);
            }
            let error = serde_json::from_slice::<serde_json::Value>(&body).ok();
            let code = if status == 503 && error.as_ref().and_then(|value| value.get("error")).and_then(|value| value.as_str()) == Some("registry-full") {
                "directory-full"
            } else {
                match status {
                    403 => "directory-record-owned",
                    429 => "directory-admission-limited",
                    400..=499 => "directory-rejected",
                    _ => "directory-unavailable",
                }
            };
            return Err(Error::new(code, status));
        }
        let confirming = store.clone();
        tokio::task::spawn_blocking(move || confirming.confirm_publication(&publication))
            .await.map_err(|_| Error::new("connection-state-unavailable", 503))??;
        self.status.send_replace(PublicationStatus {
            phase: "published",
            error: None,
        });
        Ok(true)
    }
    pub async fn run(
        self,
        store: Arc<Store>,
        changed: Arc<Notify>,
        mut stop: watch::Receiver<bool>,
        wait_for_address: bool,
    ) {
        if wait_for_address {
            tokio::select! { _ = changed.notified() => (), _ = stop.changed() => return }
        }
        let mut failures = 0u32;
        loop {
            if *stop.borrow() {
                return;
            }
            let outcome = tokio::select! {
                _ = stop.changed() => return,
                value = self.publish_once(&store) => value,
            };
            match outcome {
                Ok(_) => {
                    failures = 0;
                    // Check persisted renewal time without making an HTTP request
                    // until the seven-day interval has elapsed.
                    tokio::select! {
                        _ = changed.notified() => (),
                        _ = tokio::time::sleep(Duration::from_secs(60)) => (),
                        _ = stop.changed() => return,
                    }
                }
                Err(error) if error.code == "publication-superseded" => continue,
                Err(error) => {
                    self.status.send_replace(PublicationStatus {
                        phase: "failed",
                        error: Some(error.code),
                    });
                    failures = failures.saturating_add(1);
                    let delay = Duration::from_secs((1u64 << failures.min(6)).min(60).max(self.retry_after.load(Ordering::Relaxed)));
                    tokio::select! { _ = changed.notified() => (), _ = tokio::time::sleep(delay) => (), _ = stop.changed() => return }
                }
            }
        }
    }
}
