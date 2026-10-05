use super::*;

#[test]
fn a_control_request_completes_beside_two_incomplete_response_bodies() {
    use axum::{body::{Body, Bytes}, routing::get, Router};
    use futures::StreamExt;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (release, gate) = tokio::sync::watch::channel(false);
    let (entered, requests) = std::sync::mpsc::channel();
    let router = Router::new().route("/held", get(move || {
        let mut gate = gate.clone();
        entered.send(()).unwrap();
        async move {
            let prefix = futures::stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"o")) });
            let suffix = futures::stream::once(async move {
                gate.wait_for(|released| *released).await.unwrap();
                Ok::<_, std::io::Error>(Bytes::from_static(b"k"))
            });
            Body::from_stream(prefix.chain(suffix))
        }
    })).route("/control", get(|| async { "ready" }));
    let server = runtime.spawn(async move { axum::serve(listener, router).await.unwrap() });
    let config = super::tests::config(&endpoint);
    let clients = (0..3).map(|_| ServerClient::new(config.clone()).unwrap()).collect::<Vec<_>>();
    std::thread::scope(|scope| {
        let jobs = clients[..2].iter().map(|client| {
            scope.spawn(move || client.request(reqwest::Method::GET, "held", &[], None, &[], 1024))
        }).collect::<Vec<_>>();
        for _ in 0..2 { requests.recv_timeout(Duration::from_secs(5)).unwrap(); }
        let (sent, received) = std::sync::mpsc::channel();
        let control = &clients[2];
        let job = scope.spawn(move || sent.send(control.request(reqwest::Method::GET, "control", &[], None, &[], 1024)).unwrap());
        let early = received.recv_timeout(Duration::from_secs(2));
        release.send_replace(true);
        job.join().unwrap();
        for job in jobs {
            let reply = job.join().unwrap().unwrap();
            assert_eq!(reply.status, 200);
            assert_eq!(reply.body, b"ok");
        }
        let reply = early.expect("control request waited behind transfer bodies").unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, b"ready");
    });
    server.abort();
}
