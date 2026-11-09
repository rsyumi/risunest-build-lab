use std::collections::BTreeMap;

use serde_json::{json, Value};
use tauri::{
    ipc::{CallbackFn, InvokeBody, InvokeResponseBody, RuntimeAuthority},
    test::{get_ipc_response, mock_builder, mock_context, noop_assets, MockRuntime, INVOKE_KEY},
    utils::{
        acl::{capability::Capability, resolved::Resolved},
        platform::Target,
    },
    webview::InvokeRequest,
    WebviewWindow, WebviewWindowBuilder,
};

pub fn http_window(platform: Target) -> WebviewWindow<MockRuntime> {
    http_window_with_client_builder(platform, |builder| builder)
}

pub fn http_window_with_client_builder(
    platform: Target,
    client_builder: tauri_plugin_http::ClientBuilderHook,
) -> WebviewWindow<MockRuntime> {
    let manifests = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/acl-manifests.json"
    )))
    .unwrap();
    let capabilities = [
        include_str!("../../capabilities/migrated.json"),
        include_str!("../../capabilities/mobile.json"),
        include_str!("../../capabilities/desktop.json"),
    ]
    .into_iter()
    .map(|source| {
        let mut value: Value = serde_json::from_str(source).unwrap();
        // Exercise each platform's real HTTP permissions without unrelated plugins.
        value["permissions"]
            .as_array_mut()
            .unwrap()
            .retain(|permission| {
                permission
                    .as_str()
                    .or_else(|| permission["identifier"].as_str())
                    .is_some_and(|identifier| identifier.starts_with("http:"))
            });
        let capability: Capability = serde_json::from_value(value).unwrap();
        (capability.identifier.clone(), capability)
    })
    .collect::<BTreeMap<_, _>>();
    let resolved = Resolved::resolve(&manifests, capabilities, platform).unwrap();
    let mut context = mock_context(noop_assets());
    *context.runtime_authority_mut() = RuntimeAuthority::new(manifests, resolved);
    let app = mock_builder()
        .plugin(tauri_plugin_http::init_with_client_builder(client_builder))
        .build(context)
        .unwrap();
    WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap()
}

pub fn invoke(
    window: &WebviewWindow<MockRuntime>,
    command: &str,
    body: Value,
) -> Result<InvokeResponseBody, Value> {
    get_ipc_response(
        window,
        InvokeRequest {
            cmd: format!("plugin:http|{command}"),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: window.url().unwrap(),
            body: InvokeBody::Json(body),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.into(),
        },
    )
}

pub fn read_body_result(
    window: &WebviewWindow<MockRuntime>,
    response: &Value,
) -> Result<Vec<u8>, Value> {
    let rid = response["rid"]
        .as_u64()
        .ok_or_else(|| json!("Native HTTP response has no body resource"))?;
    let result = (|| {
        let mut body = Vec::new();
        loop {
            let chunk = match invoke(window, "fetch_read_body", json!({"rid": rid}))? {
                InvokeResponseBody::Raw(chunk) => chunk,
                _ => return Err(json!("Unexpected native HTTP body IPC response")),
            };
            // A chunk ends in 0 when it carries data and is a lone 1 once the body is done.
            match chunk.split_last() {
                Some((0, data)) => body.extend_from_slice(data),
                Some((1, [])) => return Ok(body),
                _ => return Err(json!("Malformed native HTTP body IPC response")),
            }
        }
    })();
    if result.is_err() {
        let _ = invoke(window, "fetch_cancel_body", json!({"rid": rid}));
    }
    result
}

pub fn read_body(window: &WebviewWindow<MockRuntime>, response: &Value) -> Vec<u8> {
    read_body_result(window, response).unwrap()
}
