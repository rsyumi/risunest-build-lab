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

fn http_window(platform: Target) -> WebviewWindow<MockRuntime> {
    let manifests = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/acl-manifests.json"
    )))
    .unwrap();
    let capabilities = [
        include_str!("../capabilities/migrated.json"),
        include_str!("../capabilities/mobile.json"),
        include_str!("../capabilities/desktop.json"),
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
        .plugin(tauri_plugin_http::init())
        .build(context)
        .unwrap();
    WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap()
}

fn invoke(
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

#[test]
fn all_http_urls_pass_the_native_scope_on_every_configured_platform() {
    for platform in [
        Target::Windows,
        Target::Linux,
        Target::MacOS,
        Target::Android,
    ] {
        let window = http_window(platform);
        for url in [
            "https://api.example.invalid",
            "https://api.example.invalid:443/v1/chat?model=test#result",
            "https://api.example.invalid:8443/v1/chat?model=test#result",
            "http://api.example.invalid:8080/v1/chat",
            "http://localhost:11434/api/chat",
            "http://127.0.0.1:5000/",
            "http://192.168.1.2:8000/v1/chat",
            "http://[::1]:11434/api/chat",
            "https://[2001:db8::1]:9443/a/b?x=1",
            "https://api.example.invalid:65535/a/b/c",
        ] {
            // `fetch` creates a request resource. Only `fetch_send` accesses the network.
            let response = invoke(
                &window,
                "fetch",
                json!({"clientConfig": {
                    "url": url, "method": "GET", "headers": []
                }}),
            );
            assert!(
                response.is_ok(),
                "{platform:?} rejected {url}: {response:?}"
            );
            let rid = response.unwrap().deserialize::<u32>().unwrap();
            invoke(&window, "fetch_cancel", json!({"rid": rid})).unwrap();
        }
    }
}

#[test]
fn caller_headers_reach_the_native_http_server() {
    use std::time::Duration;

    let listener = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/headers", listener.server_addr().to_ip().unwrap());
    let server = std::thread::spawn(move || {
        let request = listener
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .expect("local HTTP fixture did not receive a request");
        let headers = request
            .headers()
            .iter()
            .map(|header| format!("{header}\r\n").to_lowercase())
            .collect::<String>();
        request.respond(tiny_http::Response::empty(204)).unwrap();
        headers
    });
    let window = http_window(Target::Windows);
    let rid = invoke(
        &window,
        "fetch",
        json!({"clientConfig": {
            "url": url, "method": "GET", "headers": [
                ["Origin", "https://plugin.example.invalid"],
                ["Referer", "https://plugin.example.invalid/page"],
                ["Cookie", "fixture=synthetic"],
                ["Authorization", "Bearer synthetic"]
            ]
        }}),
    )
    .unwrap()
    .deserialize::<u32>()
    .unwrap();
    let response = invoke(&window, "fetch_send", json!({"rid": rid}));
    let request = server.join().unwrap();
    assert!(
        response.is_ok(),
        "native fetch failed: {response:?}; captured request: {request:?}"
    );
    for header in [
        "origin: https://plugin.example.invalid\r\n",
        "referer: https://plugin.example.invalid/page\r\n",
        "cookie: fixture=synthetic\r\n",
        "authorization: bearer synthetic\r\n",
    ] {
        assert!(
            request.contains(header),
            "missing {header:?} in {request:?}"
        );
    }
}

/// Serves one request with `response` and returns the `Accept-Encoding` values it carried.
fn serve_once(
    response: tiny_http::ResponseBox,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/data", listener.server_addr().to_ip().unwrap());
    let server = std::thread::spawn(move || {
        let request = listener
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .expect("local HTTP fixture did not receive a request");
        let accept_encoding = request
            .headers()
            .iter()
            .filter(|header| header.field.equiv("Accept-Encoding"))
            .map(|header| header.value.to_string())
            .collect();
        request.respond(response).unwrap();
        accept_encoding
    });
    (url, server)
}

fn gzip_response(status: u16, body: &[u8]) -> tiny_http::ResponseBox {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).unwrap();
    tiny_http::Response::from_data(encoder.finish().unwrap())
        .with_status_code(status)
        .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap())
        .with_header("Content-Encoding: gzip".parse::<tiny_http::Header>().unwrap())
        .boxed()
}

fn send(window: &WebviewWindow<MockRuntime>, url: &str, method: &str) -> Value {
    let rid = invoke(
        window,
        "fetch",
        json!({"clientConfig": {
            "url": url, "method": method, "headers": [["Accept-Encoding", "br, zstd"]]
        }}),
    )
    .unwrap()
    .deserialize::<u32>()
    .unwrap();
    invoke(window, "fetch_send", json!({"rid": rid}))
        .unwrap()
        .deserialize::<Value>()
        .unwrap()
}

fn read_body(window: &WebviewWindow<MockRuntime>, response: &Value) -> Vec<u8> {
    let rid = response["rid"].as_u64().unwrap();
    let mut body = Vec::new();
    loop {
        let chunk = match invoke(window, "fetch_read_body", json!({"rid": rid})) {
            Ok(InvokeResponseBody::Raw(chunk)) => chunk,
            other => panic!("unexpected body chunk: {other:?}"),
        };
        // A chunk ends in 0 when it carries data and is a lone 1 once the body is done.
        match chunk.split_last() {
            Some((0, data)) => body.extend_from_slice(data),
            Some((1, [])) => return body,
            _ => panic!("malformed body chunk: {chunk:?}"),
        }
    }
}

fn content_encoding(response: &Value) -> Option<&str> {
    response["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|header| header[0].as_str().unwrap().eq_ignore_ascii_case("content-encoding"))
        .map(|header| header[1].as_str().unwrap())
}

#[test]
fn native_http_offers_gzip_itself_and_decodes_it() {
    let json = br#"[[["synthetic translation","synthetic source"]],null,"en"]"#;
    let (url, server) = serve_once(gzip_response(200, json));
    let window = http_window(Target::Windows);

    let response = send(&window, &url, "GET");

    assert_eq!(server.join().unwrap(), ["gzip"]);
    assert_eq!(response["status"], 200);
    assert_eq!(content_encoding(&response), None);
    assert_eq!(read_body(&window, &response), json);
}

#[test]
fn native_http_accepts_empty_gzip_labelled_responses() {
    for (method, status) in [("HEAD", 200), ("GET", 204), ("GET", 304)] {
        let (url, server) = serve_once(gzip_response(status, b"{}"));
        let window = http_window(Target::Windows);

        let response = send(&window, &url, method);

        assert_eq!(server.join().unwrap(), ["gzip"], "{method} {status}");
        assert_eq!(response["status"], status, "{method} {status}");
        assert_eq!(read_body(&window, &response), b"", "{method} {status}");
    }
}

#[test]
fn native_http_keeps_no_cookies_between_requests() {
    let listener = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/session", listener.server_addr().to_ip().unwrap());
    let server = std::thread::spawn(move || {
        (0..2)
            .map(|_| {
                let request = listener
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap()
                    .expect("local HTTP fixture did not receive a request");
                let cookies = request
                    .headers()
                    .iter()
                    .filter(|header| header.field.equiv("Cookie"))
                    .map(|header| header.value.to_string())
                    .collect::<Vec<_>>();
                let response = tiny_http::Response::empty(204).with_header(
                    "Set-Cookie: session=synthetic; Path=/"
                        .parse::<tiny_http::Header>()
                        .unwrap(),
                );
                request.respond(response).unwrap();
                cookies
            })
            .collect::<Vec<_>>()
    });
    let window = http_window(Target::Windows);

    let responses = [send(&window, &url, "GET"), send(&window, &url, "GET")];

    // The caller still sees Set-Cookie, but nothing stores it for the next request.
    for response in &responses {
        assert!(response["headers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|header| header[0].as_str().unwrap().eq_ignore_ascii_case("set-cookie")));
    }
    assert_eq!(server.join().unwrap(), [Vec::<String>::new(), Vec::new()]);
}
