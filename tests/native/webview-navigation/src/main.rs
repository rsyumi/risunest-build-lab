use std::{cell::RefCell, rc::Rc, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use tao::{event::Event, event_loop::{ControlFlow, EventLoopBuilder}, platform::run_return::EventLoopExtRunReturn, window::WindowBuilder};
use wry::{http::Request, WebViewBuilder};

#[path = "../../../../src-tauri/src/webview_navigation_policy.rs"]
mod navigation;

#[derive(Debug)]
enum Probe { FrameLoaded, Blocked, Result(bool) }

fn main() {
    let app = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let external = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let app_url = format!("http://{}/", app.server_addr());
    let external_url = format!("http://{}", external.server_addr());
    let origin = url::Url::parse(&app_url).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let mut servers = Vec::new();
    for (server, is_app) in [(app, true), (external, false)] {
        let done = done.clone();
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
        location.href = '{external_url}/blocked';
    }}
}});
</script><iframe src="{external_url}/redirect"></iframe>"#))
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
    let rejected = Rc::new(RefCell::new(Vec::new()));
    let recorded = rejected.clone();
    let expected_url = app_url.clone();
    let profile = tempfile::tempdir().unwrap();
    let mut context = wry::WebContext::new(Some(profile.path().to_owned()));
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(&app_url)
        .with_navigation_handler(move |url| {
            let allowed = url::Url::parse(&url).is_ok_and(|url| navigation::allows_navigation(&url, Some(&origin)));
            if !allowed {
                recorded.borrow_mut().push(url.clone());
                if url.ends_with("/blocked") { blocked_proxy.send_event(Probe::Blocked).unwrap(); }
            }
            allowed
        })
        .with_ipc_handler(move |request: Request<String>| {
            let event = match request.body().as_str() {
                "frame-loaded" => Probe::FrameLoaded,
                "preserved" => Probe::Result(true),
                _ => Probe::Result(false),
            };
            ipc_proxy.send_event(event).unwrap();
        });
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().unwrap()).unwrap()
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).unwrap();
    let mut frame_loaded = false;
    let mut result = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    events.run_return(|event, _, flow| {
        *flow = ControlFlow::WaitUntil(deadline);
        match event {
            Event::UserEvent(Probe::FrameLoaded) => frame_loaded = true,
            Event::UserEvent(Probe::Blocked) => {
                webview.evaluate_script(&format!("window.ipc.postMessage(location.href === {} ? 'preserved' : 'replaced')", serde_json::to_string(&expected_url).unwrap())).unwrap();
            }
            Event::UserEvent(Probe::Result(preserved)) => { result = preserved; *flow = ControlFlow::Exit; }
            _ if Instant::now() >= deadline => *flow = ControlFlow::Exit,
            _ => {}
        }
    });
    done.store(true, Ordering::Release);
    for server in servers { server.join().unwrap(); }
    assert!(frame_loaded, "Cross-origin iframe (including its redirect) did not load; rejected: {:?}", rejected.borrow());
    assert!(result, "External main-frame navigation was not cancelled before document replacement");
    assert_eq!(&*rejected.borrow(), &[format!("{external_url}/blocked")]);
    println!("PASS: cross-origin iframe and redirect load; external main-frame document is blocked");
}
