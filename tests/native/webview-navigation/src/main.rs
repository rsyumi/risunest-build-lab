use std::{cell::RefCell, rc::Rc, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use tao::{event::Event, event_loop::{ControlFlow, EventLoopBuilder}, platform::run_return::EventLoopExtRunReturn, window::WindowBuilder};
use wry::{http::Request, WebViewBuilder};

#[path = "../../../../src-tauri/src/webview_navigation_policy.rs"]
mod navigation;

#[derive(Debug)]
enum Probe { FrameLoaded, Blocked, Finished, Result(String) }

fn main() {
    let Some(case) = std::env::args().nth(1) else {
        for case in ["response", "connection-refused", "allowed-failure"] {
            if case == "allowed-failure" && !cfg!(target_os = "linux") { continue; }
            let status = std::process::Command::new(std::env::current_exe().unwrap()).arg(case).status().unwrap();
            assert!(status.success(), "Native navigation case failed: {case}");
        }
        return;
    };
    assert!(matches!(case.as_str(), "response" | "connection-refused" | "allowed-failure"));
    let app = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let external = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let app_url = format!("http://{}/", app.server_addr());
    let external_url = format!("http://{}", external.server_addr());
    let closed_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let refused_url = format!("http://{}/blocked", closed_port.local_addr().unwrap());
    drop(closed_port);
    let target_url = match case.as_str() {
        "response" => format!("{external_url}/blocked"),
        "connection-refused" => refused_url,
        _ => format!("{app_url}failed"),
    };
    let origin = url::Url::parse(&app_url).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let app_done = Arc::new(AtomicBool::new(false));
    let mut servers = Vec::new();
    for (server, is_app) in [(app, true), (external, false)] {
        let done = if is_app { app_done.clone() } else { done.clone() };
        let external_url = external_url.clone();
        servers.push(std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                let Some(request) = server.recv_timeout(Duration::from_millis(100)).unwrap() else { continue };
                let response = if is_app {
                    tiny_http::Response::from_string(format!(r#"<!doctype html><title>Synthetic navigation check</title>
<script>
addEventListener('message', event => {{
    if (event.data === 'synthetic-frame-ready' && event.source === document.querySelector('iframe').contentWindow) {{
        window.ipc.postMessage('frame-loaded');
    }}
}});
</script><p id="app-state">Synthetic app document</p><iframe src="{external_url}/redirect"></iframe>"#))
                } else if request.url() == "/redirect" {
                    tiny_http::Response::from_string("").with_status_code(302)
                        .with_header(tiny_http::Header::from_bytes("Location", "/frame").unwrap())
                } else if request.url() == "/frame" {
                    tiny_http::Response::from_string("<script>parent.postMessage('synthetic-frame-ready', '*')</script>")
                } else {
                    tiny_http::Response::from_string("<script>window.ipc.postMessage('replaced')</script>")
                };
                let response = response.with_header(tiny_http::Header::from_bytes("Content-Type", "text/html").unwrap());
                request.respond(response).unwrap();
            }
        }));
    }

    let mut events = EventLoopBuilder::<Probe>::with_user_event().build();
    let window = WindowBuilder::new().with_title("Synthetic WebView navigation check").build(&events).unwrap();
    let blocked_proxy = events.create_proxy();
    let ipc_proxy = events.create_proxy();
    let load_proxy = events.create_proxy();
    let rejected = Rc::new(RefCell::new(Vec::new()));
    let recorded = rejected.clone();
    let profile = tempfile::tempdir().unwrap();
    let mut context = wry::WebContext::new(Some(profile.path().to_owned()));
    let mut headers = wry::http::HeaderMap::new();
    headers.insert(wry::http::header::CONNECTION, "close".parse().unwrap());
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url_and_headers(&app_url, headers)
        .with_navigation_handler(move |url| {
            let allowed = url::Url::parse(&url).is_ok_and(|url| navigation::allows_navigation(&url, Some(&origin)));
            if !allowed {
                recorded.borrow_mut().push(url.clone());
                if url.ends_with("/blocked") { blocked_proxy.send_event(Probe::Blocked).unwrap(); }
            }
            allowed
        })
        .with_on_page_load_handler(move |event, _url| {
            if matches!(event, wry::PageLoadEvent::Finished) {
                load_proxy.send_event(Probe::Finished).unwrap();
            }
        })
        .with_ipc_handler(move |request: Request<String>| {
            if request.body() == "frame-loaded" {
                ipc_proxy.send_event(Probe::FrameLoaded).unwrap();
            }
        });
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().unwrap()).unwrap()
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).unwrap();
    let result_proxy = events.create_proxy();
    let mut frame_loaded = false;
    let mut result = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    events.run_return(|event, _, flow| {
        *flow = ControlFlow::WaitUntil(deadline);
        match event {
            Event::UserEvent(Probe::FrameLoaded) => {
                frame_loaded = true;
                if case == "allowed-failure" {
                    // Stop the app server so its own origin fails without an HTTP response.
                    app_done.store(true, Ordering::Release);
                    servers.remove(0).join().unwrap();
                }
                webview.evaluate_script(&format!("location.href = {}", serde_json::to_string(&target_url).unwrap())).unwrap();
            },
            Event::UserEvent(Probe::Blocked) | Event::UserEvent(Probe::Finished)
                if !rejected.borrow().is_empty() || webview.url().is_ok_and(|url| url == target_url) => {
                let proxy = result_proxy.clone();
                webview.evaluate_script_with_callback(
                    "({url: location.href, title: document.title, text: document.querySelector('#app-state')?.textContent ?? null, body: document.body?.innerText ?? ''})",
                    move |snapshot| { proxy.send_event(Probe::Result(snapshot)).unwrap(); },
                ).unwrap();
            }
            Event::UserEvent(Probe::Result(snapshot)) => {
                let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
                result = if case == "allowed-failure" {
                    snapshot["text"].is_null() && snapshot["body"].as_str().is_some_and(|text| !text.is_empty())
                        && webview.url().unwrap() == target_url && rejected.borrow().is_empty()
                } else {
                    snapshot["url"] == app_url && snapshot["title"] == "Synthetic navigation check"
                        && snapshot["text"] == "Synthetic app document" && webview.url().unwrap() == app_url
                };
                // A failed load finishes before WebKit commits its default error document.
                if case == "allowed-failure" && !result { return; }
                if !result { eprintln!("{case}: document snapshot {snapshot}; native URL {:?}", webview.url()); }
                *flow = ControlFlow::Exit;
            }
            _ if Instant::now() >= deadline => *flow = ControlFlow::Exit,
            _ => {}
        }
    });
    app_done.store(true, Ordering::Release);
    done.store(true, Ordering::Release);
    for server in servers { server.join().unwrap(); }
    assert!(frame_loaded, "Cross-origin iframe (including its redirect) did not load; rejected: {:?}", rejected.borrow());
    assert!(result, "{case}: unexpected main document after navigation; native URL: {:?}; rejected: {:?}", webview.url(), rejected.borrow());
    if case == "allowed-failure" {
        assert!(rejected.borrow().is_empty());
        println!("PASS: allowed-origin transport failure keeps the default error page");
    } else {
        assert_eq!(&*rejected.borrow(), &[target_url]);
        println!("PASS: {case}: iframe and redirect load; main document, URL and title are preserved");
    }
}
