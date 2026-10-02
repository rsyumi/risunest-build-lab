use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

#[cfg(target_os = "ios")]
use serde::Serialize;
#[cfg(any(target_os = "ios", test))]
use serde::Deserialize;
#[cfg(target_os = "ios")]
use tauri::{plugin::PluginHandle, Manager};

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_ios_native);

#[cfg(target_os = "ios")]
struct Inner<R: Runtime> {
    handle: PluginHandle<R>,
    data_root: String,
    /// `Some` once the native side has accepted the root. A rejected handover
    /// is left unset so the next call tries again.
    delivered: tauri::async_runtime::Mutex<bool>,
}

#[cfg(target_os = "ios")]
pub struct IosNative<R: Runtime>(std::sync::Arc<Inner<R>>);

// A derived Clone would only apply to a cloneable runtime, so `ios_native().clone()`
// would copy the reference instead of the handle.
#[cfg(target_os = "ios")]
impl<R: Runtime> Clone for IosNative<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

#[cfg(target_os = "ios")]
pub trait IosNativeExt<R: Runtime> {
    fn ios_native(&self) -> &IosNative<R>;
}

#[cfg(target_os = "ios")]
impl<R: Runtime, T: Manager<R>> IosNativeExt<R> for T {
    fn ios_native(&self) -> &IosNative<R> {
        self.state::<IosNative<R>>().inner()
    }
}

#[cfg(any(target_os = "ios", test))]
#[derive(Debug, Eq, PartialEq)]
pub enum WebAuthenticationOutcome {
    Callback(String),
    Cancelled,
    Unavailable,
    Failed,
}

#[cfg(target_os = "ios")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebAuthenticationRequest<'a> {
    authorization_url: &'a str,
    callback_scheme: &'a str,
    prefers_ephemeral: bool,
}

#[cfg(any(target_os = "ios", test))]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebAuthenticationResponse {
    status: String,
    callback_url: Option<String>,
}

#[cfg(any(target_os = "ios", test))]
impl WebAuthenticationResponse {
    fn outcome(self) -> WebAuthenticationOutcome {
        match self.status.as_str() {
            "succeeded" => self.callback_url.filter(|url| !url.is_empty())
                .map(WebAuthenticationOutcome::Callback).unwrap_or(WebAuthenticationOutcome::Failed),
            "cancelled" => WebAuthenticationOutcome::Cancelled,
            "busy" | "presentation-unavailable" => WebAuthenticationOutcome::Unavailable,
            _ => WebAuthenticationOutcome::Failed,
        }
    }
}

#[cfg(test)]
mod authentication_tests {
    use super::*;

    #[test]
    fn presentation_outcomes_remain_distinct_from_session_failure() {
        for status in ["busy", "presentation-unavailable"] {
            assert_eq!(WebAuthenticationResponse { status: status.into(), callback_url: None }.outcome(), WebAuthenticationOutcome::Unavailable);
        }
        assert_eq!(WebAuthenticationResponse { status: "cancelled".into(), callback_url: None }.outcome(), WebAuthenticationOutcome::Cancelled);
        assert_eq!(WebAuthenticationResponse { status: "failed".into(), callback_url: None }.outcome(), WebAuthenticationOutcome::Failed);
        assert_eq!(WebAuthenticationResponse { status: "succeeded".into(), callback_url: Some("synthetic:/callback".into()) }.outcome(), WebAuthenticationOutcome::Callback("synthetic:/callback".into()));
    }
}

#[cfg(target_os = "ios")]
#[derive(Serialize)]
struct OpenedFilesRequest { urls: Vec<String> }

#[cfg(target_os = "ios")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DataRootRequest<'a> { data_root: &'a str }

#[cfg(target_os = "ios")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PortableSourceRequest<'a> { token:&'a str, job_id:Option<&'a str> }
#[cfg(target_os = "ios")]
#[derive(Serialize)]
struct PortableSourceFormatRequest<'a> {token:&'a str, format:&'a str}
#[cfg(target_os = "ios")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PortableSourceProbeRequest<'a> {token:&'a str,probe_id:&'a str}
#[cfg(target_os = "ios")]
#[derive(Deserialize)]
pub struct PortableSourceDescriptor { pub fd:i32, pub bytes:u64 }

#[cfg(target_os = "ios")]
impl<R: Runtime> IosNative<R> {
    pub fn portable_source_descriptor(&self, token:&str, job_id:Option<&str>) -> Result<PortableSourceDescriptor,String> {
        self.0.handle.run_mobile_plugin("portableSourceDescriptor",PortableSourceRequest {token,job_id}).map_err(|error|error.to_string())
    }
    pub fn release_portable_source(&self, token:&str, job_id:Option<&str>) -> Result<bool,String> {
        self.0.handle.run_mobile_plugin("releasePortableSource",PortableSourceRequest {token,job_id}).map_err(|error|error.to_string())
    }
    pub fn portable_source_orphans(&self)->Result<Vec<String>,String> {
        self.0.handle.run_mobile_plugin("portableSourceOrphans",()).map_err(|error|error.to_string())
    }
    pub fn acknowledge_portable_source_orphan(&self,token:&str)->Result<bool,String> {
        self.0.handle.run_mobile_plugin("acknowledgePortableSourceOrphan",PortableSourceRequest{token,job_id:None}).map_err(|error|error.to_string())
    }
    pub fn begin_portable_source_probe(&self,token:&str,probe_id:&str)->Result<bool,String> {
        self.0.handle.run_mobile_plugin("beginPortableSourceProbe",PortableSourceProbeRequest{token,probe_id}).map_err(|error|error.to_string())
    }
    pub fn end_portable_source_probe(&self,token:&str,probe_id:&str)->Result<bool,String> {
        self.0.handle.run_mobile_plugin("endPortableSourceProbe",PortableSourceProbeRequest{token,probe_id}).map_err(|error|error.to_string())
    }
    pub fn confirm_portable_source_format(&self,token:&str,format:&str)->Result<(),String> {
        self.0.handle.run_mobile_plugin("confirmPortableSourceFormat",PortableSourceFormatRequest{token,format}).map_err(|error|error.to_string())
    }
    /// Hands over the store root, once. The native side enforces file ownership
    /// against it and rejects every staged path until it arrives, so each call
    /// that can reach a staged file waits here instead of racing the handover.
    pub async fn ensure_data_root(&self) -> Result<(), String> {
        let mut delivered = self.0.delivered.lock().await;
        if *delivered {
            return Ok(());
        }
        self.0
            .handle
            .run_mobile_plugin_async::<()>(
                "setDataRoot",
                DataRootRequest {
                    data_root: &self.0.data_root,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        *delivered = true;
        Ok(())
    }

    pub async fn receive_opened_files(&self, urls: Vec<String>) {
        if self.ensure_data_root().await.is_err() {
            return;
        }
        let _ = self
            .0
            .handle
            .run_mobile_plugin_async::<()>("receiveOpenedFiles", OpenedFilesRequest { urls })
            .await;
    }

    pub async fn authenticate(
        &self,
        authorization_url: &str,
        callback_scheme: &str,
        prefers_ephemeral: bool,
    ) -> WebAuthenticationOutcome {
        let response = self
            .0
            .handle
            .run_mobile_plugin_async::<WebAuthenticationResponse>(
                "authenticate",
                WebAuthenticationRequest {
                    authorization_url,
                    callback_scheme,
                    prefers_ephemeral,
                },
            )
            .await;
        response.map(WebAuthenticationResponse::outcome).unwrap_or(WebAuthenticationOutcome::Failed)
    }

    pub async fn cancel_authentication(&self) {
        let _ = self
            .0
            .handle
            .run_mobile_plugin_async::<()>("cancelAuthentication", ())
            .await;
    }
}

/// The store root is resolved by the host, which is the only side holding a
/// path manifest. It is handed over on first use rather than at registration,
/// because the native handler resolves on the main queue.
pub fn init<R: Runtime>(data_root: String) -> TauriPlugin<R> {
    Builder::new("ios-native")
        .setup(move |app, api| {
            #[cfg(target_os = "ios")]
            {
                let handle = api.register_ios_plugin(init_plugin_ios_native)?;
                app.manage(IosNative(std::sync::Arc::new(Inner {
                    handle,
                    data_root,
                    delivered: tauri::async_runtime::Mutex::new(false),
                })));
            }
            #[cfg(not(target_os = "ios"))]
            let _ = (app, api, &data_root);
            Ok(())
        })
        .build()
}
