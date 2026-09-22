use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

#[cfg(target_os = "ios")]
use serde::{Deserialize, Serialize};
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

#[cfg(target_os = "ios")]
#[derive(Debug, Eq, PartialEq)]
pub enum WebAuthenticationOutcome {
    Callback(String),
    Cancelled,
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

#[cfg(target_os = "ios")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebAuthenticationResponse {
    status: String,
    callback_url: Option<String>,
}

#[cfg(target_os = "ios")]
#[derive(Serialize)]
struct OpenedFilesRequest { urls: Vec<String> }

#[cfg(target_os = "ios")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DataRootRequest<'a> { data_root: &'a str }

#[cfg(target_os = "ios")]
impl<R: Runtime> IosNative<R> {
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
        match response {
            Ok(response) if response.status == "succeeded" => response
                .callback_url
                .filter(|url| !url.is_empty())
                .map(WebAuthenticationOutcome::Callback)
                .unwrap_or(WebAuthenticationOutcome::Failed),
            Ok(response) if response.status == "cancelled" => {
                WebAuthenticationOutcome::Cancelled
            }
            _ => WebAuthenticationOutcome::Failed,
        }
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
