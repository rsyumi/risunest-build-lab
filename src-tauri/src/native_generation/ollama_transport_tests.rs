// The public transport entry point against owned 127.0.0.1 HTTP/1.1 replies.
mod generation {
    use super::super::{
        inputs::PreparationSnapshot, operation::StoredHistory, prepare::{prepare_ollama, PreparedOllamaTurn},
        ollama_decode::{tests as wire, CancelOrigin, Cancellation},
        ollama_transport::{generate, BodyFailure, CapturedOllamaConfig, ConsumerStop, Failure, OfferResult, Outcome, OutputSink,
            Settlement, Snapshot, Stage, Think, ThinkLevel, TransportContext, TransportLimits},
    };
    use loopback::{respond, serve, End, Reply, Server};
    use risunest_ir::{Error as CompatError, Limits, Value};
    use serde_json::{json, Value as Json};
    use std::sync::Arc;
    use tokio::sync::Notify;

    mod loopback {
        use std::{collections::VecDeque, future::Future, sync::{Arc, Mutex}, time::Duration};
        use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::Notify, task::{JoinHandle, JoinSet}};

        #[derive(Debug, Clone)]
        pub struct Request { pub method: String, pub path: String, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
        impl Request {
            pub fn header(&self, name: &str) -> Option<&str> { self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str()) }
        }
        pub enum End { Finish, Hold, Reset }
        pub enum Reply { Respond { status: String, headers: Vec<(String, String)>, parts: Vec<Vec<u8>>, end: End }, Close, HoldHeaders }
        pub fn respond(status: &str, headers: &[(&str, &str)], parts: Vec<Vec<u8>>, end: End) -> Reply {
            Reply::Respond { status: status.to_owned(), headers: headers.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(), parts, end }
        }
        #[derive(Default)]
        struct Shared { requests: Mutex<Vec<Request>>, seen: Notify, closed: Notify }
        pub struct Server { pub origin: String, pub port: u16, shared: Arc<Shared>, task: JoinHandle<()> }
        impl Drop for Server { fn drop(&mut self) { self.task.abort(); } }
        impl Server {
            pub fn requests(&self) -> Vec<Request> { self.shared.requests.lock().unwrap().clone() }
            pub async fn seen(&self) { bounded(self.shared.seen.notified()).await }
            pub async fn client_closed(&self) { bounded(self.shared.closed.notified()).await }
        }
        // A guard against a hung transport, not a timing assertion.
        async fn bounded(condition: impl Future<Output = ()>) {
            tokio::time::timeout(Duration::from_secs(30), condition).await.expect("loopback condition was not reached");
        }
        // Each accepted connection takes the next reply; connections beyond the list are closed unanswered.
        pub async fn serve(build: impl FnOnce(&str, u16) -> Vec<Reply>) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let origin = format!("http://127.0.0.1:{port}");
            let mut replies = VecDeque::from(build(&origin, port));
            let shared = Arc::new(Shared::default());
            let handlers = shared.clone();
            let task = tokio::spawn(async move {
                // Aborting the accept task drops this set, which aborts every held connection.
                let mut connections = JoinSet::new();
                while let Ok((stream, _)) = listener.accept().await {
                    while connections.try_join_next().is_some() {}
                    connections.spawn(handle(stream, replies.pop_front().unwrap_or(Reply::Close), handlers.clone()));
                }
            });
            Server { origin, port, shared, task }
        }
        async fn handle(mut stream: TcpStream, reply: Reply, shared: Arc<Shared>) {
            // The request is drained before any reply, so closing never resets a client that is still sending.
            let Some(request) = read_request(&mut stream).await else { return };
            shared.requests.lock().unwrap().push(request);
            shared.seen.notify_one();
            let (status, headers, parts, end) = match reply {
                Reply::Close => return,
                Reply::HoldHeaders => return closed(&mut stream, &shared).await,
                Reply::Respond { status, headers, parts, end } => (status, headers, parts, end),
            };
            let mut head = format!("HTTP/1.1 {status}\r\n");
            for (name, value) in headers { head.push_str(&format!("{name}: {value}\r\n")); }
            head.push_str("Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
            if stream.write_all(head.as_bytes()).await.is_err() { return; }
            for part in parts.into_iter().filter(|part| !part.is_empty()) {
                let mut chunk = format!("{:x}\r\n", part.len()).into_bytes();
                chunk.extend_from_slice(&part); chunk.extend_from_slice(b"\r\n");
                if stream.write_all(&chunk).await.is_err() || stream.flush().await.is_err() { return; }
            }
            match end {
                End::Finish => if stream.write_all(b"0\r\n\r\n").await.is_ok() { let _ = stream.shutdown().await; },
                End::Hold => closed(&mut stream, &shared).await,
                // The connection closes before the terminating chunk.
                End::Reset => (),
            }
        }
        async fn closed(stream: &mut TcpStream, shared: &Shared) {
            // The client sends nothing after its request, so this read returns only once it closes.
            let _ = stream.read(&mut [0; 1]).await;
            shared.closed.notify_one();
        }
        async fn read_request(stream: &mut TcpStream) -> Option<Request> {
            let mut data = Vec::new(); let mut block = [0; 4096];
            let head = loop {
                if let Some(index) = data.windows(4).position(|window| window == b"\r\n\r\n") { break index + 4; }
                if data.len() > 65536 { return None; }
                let read = stream.read(&mut block).await.ok()?;
                if read == 0 { return None; }
                data.extend_from_slice(&block[..read]);
            };
            let text = std::str::from_utf8(&data[..head]).ok()?;
            let mut lines = text.split("\r\n");
            let mut start = lines.next()?.split(' ');
            let (method, path) = (start.next()?.to_owned(), start.next()?.to_owned());
            let headers = lines.filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned())).collect::<Vec<_>>();
            let length = match headers.iter().find(|(name, _)| name == "content-length") { Some((_, value)) => value.parse::<usize>().ok()?, None => 0 };
            let mut body = data[head..].to_vec();
            while body.len() < length {
                let read = stream.read(&mut block).await.ok()?;
                if read == 0 { return None; }
                body.extend_from_slice(&block[..read]);
            }
            Some(Request { method, path, headers, body })
        }
    }

    const AGENT: &str = "Mozilla/5.0 (Synthetic RisuNest test) AppleWebKit/537.36";
    const MODEL: &str = "synthetic-model";
    fn transport_limits() -> TransportLimits {
        TransportLimits { decode: wire::limits(), max_request_bytes: 65536, max_url_bytes: 4096, max_user_agent_bytes: 4096,
            max_buffered_bytes: 65536, max_line_bytes: 65536, max_output_bytes: 65536, max_output_value_units: 65536 }
    }
    fn turn(character: &str, user: &str) -> PreparedOllamaTurn {
        PreparedOllamaTurn { messages: Vec::new(), input_tokens: 0, output_tokens: 1, max_context: 4096, last_memory: None,
            character_name: character.into(), user_name: user.into() }
    }
    fn json_value(value: &Value) -> Json {
        match value {
            Value::Undefined => panic!("transport values come from JSON"),
            Value::Null => Json::Null,
            Value::Bool(value) => Json::Bool(*value),
            Value::Number(value) => serde_json::Number::from_f64(*value).map_or(Json::Null, Json::Number),
            Value::Text(text) => match text.as_unicode() { Some(value) => Json::from(value), None => json!({ "utf16": text.units().collect::<Vec<_>>() }) },
            Value::Array(_) => value.array().unwrap().iter().map(json_value).collect(),
            Value::Object(_) => Json::Object(value.object().unwrap().iter()
                .map(|(key, value)| (key.as_unicode().expect("synthetic keys are Unicode").to_owned(), json_value(value))).collect()),
        }
    }
    // The transport keeps JavaScript numbers as f64, so fixture integers compare in that form.
    fn numbers_as_f64(value: &Json) -> Json {
        match value {
            Json::Number(number) => serde_json::Number::from_f64(number.as_f64().unwrap()).map_or(Json::Null, Json::Number),
            Json::Array(values) => values.iter().map(numbers_as_f64).collect(),
            Json::Object(values) => Json::Object(values.iter().map(|(key, value)| (key.clone(), numbers_as_f64(value))).collect()),
            other => other.clone(),
        }
    }
    fn caught(value: BodyFailure) -> &'static str {
        match value { BodyFailure::Reader => "reader", BodyFailure::Codec => "codec", BodyFailure::Json => "json", BodyFailure::Property => "property" }
    }

    struct Recorder { snapshots: Vec<Arc<Snapshot>>, accepted: Arc<Notify> }
    #[async_trait::async_trait]
    impl OutputSink for Recorder {
        async fn credit(&mut self) -> Result<(), ConsumerStop> { Ok(()) }
        async fn offer(&mut self, snapshot: Arc<Snapshot>) -> OfferResult {
            self.snapshots.push(snapshot); self.accepted.notify_one(); OfferResult::Accepted
        }
    }
    enum Stop<'a> { Never, AfterFirstOffer, BeforeHeaders(&'a Server) }
    struct Run { outcome: Outcome, snapshots: Vec<Json>, sequences: Vec<u64> }
    async fn run(prepared: &PreparedOllamaTurn, host: &str, streaming: bool, think: Option<Think>, stop: Stop<'_>) -> Run {
        let cancel = Cancellation::new();
        let context = TransportContext::new(Limits { max_owned_bytes: 8 * 1024 * 1024, max_json_bytes: 65536, max_text_units: 65536,
            max_values: 4096, max_depth: 32, max_work: 16 * 1024 * 1024 }, cancel.clone()).unwrap();
        let accepted = Arc::new(Notify::new());
        let mut sink = Recorder { snapshots: Vec::new(), accepted: accepted.clone() };
        let config = CapturedOllamaConfig { host, model: MODEL, streaming, think, browser_user_agent: AGENT };
        let outcome = {
            let pending = generate(prepared, &config, transport_limits(), &context, &mut sink); tokio::pin!(pending);
            let origin = match stop {
                Stop::Never => None,
                Stop::AfterFirstOffer => {
                    tokio::select! { outcome = &mut pending => panic!("settled before the first offer: {outcome:?}"), _ = accepted.notified() => () }
                    Some(CancelOrigin::Caller)
                }
                Stop::BeforeHeaders(server) => {
                    tokio::select! { outcome = &mut pending => panic!("settled before the request arrived: {outcome:?}"), _ = server.seen() => () }
                    Some(CancelOrigin::Owner)
                }
            };
            if let Some(origin) = origin { cancel.cancel_as(origin); }
            pending.await
        };
        Run { outcome, snapshots: sink.snapshots.iter().map(|snapshot| json_value(&snapshot.value)).collect(),
            sequences: sink.snapshots.iter().map(|snapshot| snapshot.sequence).collect() }
    }

    fn transport_fixture() -> Json {
        serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-transport.json")).unwrap()
    }
    fn case<'a>(fixture: &'a Json, id: &str) -> &'a Json {
        fixture["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap_or_else(|| panic!("missing fixture case {id}"))
    }
    fn body_parts(case: &Json) -> Vec<Vec<u8>> {
        let parts = case["body"].as_array().unwrap().iter().map(|part| match (part.get("utf8"), part.get("hex")) {
            (Some(text), None) => text.as_str().unwrap().as_bytes().to_vec(),
            (None, Some(hex)) => hex::decode(hex.as_str().unwrap()).unwrap(),
            _ => panic!("unsupported body part {part}"),
        }).collect::<Vec<_>>();
        match case.get("fragmentBytes") {
            Some(size) => parts.concat().chunks(size.as_u64().unwrap() as usize).map(<[u8]>::to_vec).collect(),
            None => parts,
        }
    }
    fn reply(case: &Json, end: End) -> Reply {
        let status = case.get("status").map_or(200, |value| value.as_u64().unwrap() as u16);
        // Node writes the canonical reason unless the case sets statusText.
        let reason = match case.get("statusText") { Some(value) => value.as_str().unwrap(),
            None => reqwest::StatusCode::from_u16(status).unwrap().canonical_reason().unwrap_or("") };
        let content_type = case.get("contentType").and_then(Json::as_str).unwrap_or("application/json");
        respond(&format!("{status} {reason}"), &[("Content-Type", content_type)], body_parts(case), end)
    }
    fn redirect(code: u64, location: &str) -> Reply { respond(&format!("{code} Redirect"), &[("Location", location)], Vec::new(), End::Finish) }
    fn buffered_reply(body: &str) -> Reply { respond("200 OK", &[("Content-Type", "application/json")], vec![body.as_bytes().to_vec()], End::Finish) }

    #[tokio::test]
    async fn generate_sends_one_exact_chat_request_and_types_each_transport_failure() {
        let text: Json = serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-text.json")).unwrap();
        let prepared = prepare_ollama(&PreparationSnapshot { history: StoredHistory { revision: 1, character: text["character"].clone(),
            conversation: text["conversation"].clone(), messages: text["messages"].as_array().unwrap().clone() },
            root: text["root"].clone(), preset: text["preset"].clone(), selected_preset_id: Some("preset-a".into()), entropy: None, host: None }).unwrap();
        assert!(!prepared.messages.is_empty());
        let server = serve(|_, _| vec![buffered_reply(r#"{"message":{"content":"reply"}}"#)]).await;
        let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
        assert!(matches!(result.outcome.settlement, Settlement::Normal), "{:?}", result.outcome);
        assert_eq!(result.snapshots, [json!("reply")]);
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!((request.method.as_str(), request.path.as_str()), ("POST", "/api/chat"));
        let messages = prepared.messages.iter().map(|message| format!(r#"{{"role":{},"content":{}}}"#,
            serde_json::to_string(&message.role).unwrap(), serde_json::to_string(&message.content).unwrap())).collect::<Vec<_>>().join(",");
        // ollama-js writes model, messages and stream in this order and omits an unset think.
        assert_eq!(String::from_utf8(request.body.clone()).unwrap(), format!(r#"{{"model":"{MODEL}","messages":[{messages}],"stream":false}}"#));
        let mut names = request.headers.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(); names.sort_unstable();
        assert_eq!(names, ["accept", "accept-encoding", "content-length", "content-type", "host", "user-agent"]);
        for (name, value) in [("accept", "application/json"), ("accept-encoding", "gzip, deflate, br, zstd"), ("content-type", "application/json"), ("user-agent", AGENT)] {
            assert_eq!(request.header(name), Some(value), "{name}");
        }
        assert_eq!(request.header("content-length"), Some(request.body.len().to_string().as_str()));
        assert_eq!(request.header("host"), Some(format!("127.0.0.1:{}", server.port).as_str()));

        // A connection closed before headers is one failed attempt, never retried.
        let prepared = turn("Synthetic", "User");
        let server = serve(|_, _| vec![Reply::Close]).await;
        let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
        assert!(matches!(result.outcome.settlement, Settlement::Failed(Failure::Network { stage: Stage::Headers })), "{:?}", result.outcome);
        assert_eq!(server.requests().len(), 1);
        let server = serve(|_, _| vec![buffered_reply("not JSON")]).await;
        let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
        assert!(matches!(result.outcome.settlement, Settlement::Failed(Failure::Compat { stage: Stage::BufferedJson, error: CompatError::Syntax { .. } })), "{:?}", result.outcome);
        assert!(result.snapshots.is_empty());
        // Owner cancellation while waiting for headers settles with its origin and closes the connection.
        let server = serve(|_, _| vec![Reply::HoldHeaders]).await;
        let result = run(&prepared, &server.origin, true, None, Stop::BeforeHeaders(&server)).await;
        assert!(matches!(result.outcome.settlement, Settlement::Cancelled(CancelOrigin::Owner)), "{:?}", result.outcome);
        assert!(result.outcome.last_accepted.is_none() && result.outcome.in_flight.is_none());
        server.client_closed().await;
    }

    #[tokio::test]
    async fn redirects_and_sdk_hosts_follow_fetch_rules_before_and_after_dispatch() {
        let fixture = transport_fixture(); let prepared = turn("Synthetic", "User");
        let sent = format!(r#"{{"model":"{MODEL}","messages":[],"stream":false}}"#).into_bytes();
        for (id, keeps_body) in [("redirect-302", false), ("redirect-303", false), ("redirect-307", true), ("redirect-308", true)] {
            let entry = case(&fixture, id);
            let code = entry["redirect"].as_u64().unwrap();
            let server = serve(|origin, _| vec![
                redirect(code, &format!("{origin}/redirect-target/api/chat")), reply(entry, End::Finish)]).await;
            let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
            assert!(matches!(result.outcome.settlement, Settlement::Normal), "{id}: {:?}", result.outcome);
            assert_eq!(result.snapshots, [json!("")], "{id}");
            let requests = server.requests();
            assert_eq!(requests.len(), 2, "{id}");
            assert_eq!((requests[0].method.as_str(), requests[0].path.as_str(), &requests[0].body), ("POST", "/api/chat", &sent), "{id}");
            let follow = &requests[1];
            assert_eq!(follow.path, "/redirect-target/api/chat", "{id}");
            if keeps_body {
                assert_eq!((follow.method.as_str(), &follow.body), ("POST", &sent), "{id}");
                assert_eq!(follow.header("content-type"), Some("application/json"), "{id}");
            } else {
                // A redirected POST becomes a bodiless GET without the request-body headers.
                assert_eq!((follow.method.as_str(), follow.body.is_empty()), ("GET", true), "{id}");
                assert_eq!((follow.header("content-type"), follow.header("content-length")), (None, None), "{id}");
            }
        }
        // Fetch fails at the 21st redirect response; the fixture leaves this count unpinned.
        let code = case(&fixture, "redirect-loop-limit")["redirect"].as_u64().unwrap();
        let server = serve(|origin, _| (0..24).map(|_| redirect(code, &format!("{origin}/redirect-target/api/chat"))).collect()).await;
        let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
        assert!(matches!(result.outcome.settlement, Settlement::Failed(Failure::Network { stage: Stage::Headers })), "{:?}", result.outcome);
        assert_eq!(server.requests().len(), 21);
        for target in ["http://synthetic:synthetic@127.0.0.1:{port}/redirect-target/api/chat", "ftp://127.0.0.1:{port}/redirect-target/api/chat"] {
            let server = serve(|_, port| vec![redirect(307, &target.replace("{port}", &port.to_string()))]).await;
            let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
            assert!(matches!(result.outcome.settlement, Settlement::Failed(Failure::Network { stage: Stage::Headers })), "{target}: {:?}", result.outcome);
            assert_eq!(server.requests().len(), 1, "{target}");
        }
        for id in ["host-prefix", "host-port-shorthand", "host-implicit-protocol", "host-userinfo"] {
            let entry = case(&fixture, id);
            let server = serve(|_, _| vec![reply(entry, End::Finish)]).await;
            let host = entry["hostTemplate"].as_str().unwrap().replace("{origin}", &server.origin).replace("{port}", &server.port.to_string());
            let result = run(&prepared, &host, false, None, Stop::Never).await;
            let requests = server.requests();
            if id == "host-userinfo" {
                assert!(matches!(result.outcome.settlement, Settlement::Failed(Failure::Userinfo)), "{id}: {:?}", result.outcome);
                assert!(requests.is_empty(), "{id}");
            } else {
                assert!(matches!(result.outcome.settlement, Settlement::Normal), "{id}: {:?}", result.outcome);
                assert_eq!(requests.len(), 1, "{id}");
                assert_eq!(requests[0].path, entry.get("expectedPath").and_then(Json::as_str).unwrap_or("/api/chat"), "{id}");
            }
        }
    }

    #[tokio::test]
    async fn http_errors_take_the_wire_status_line_and_keep_output_accepted_before_a_body_failure() {
        let fixture = transport_fixture(); let prepared = turn("Synthetic", "User");
        let empty_reason = json!({ "status": 500, "statusText": "", "body": [] });
        let cases = [
            (case(&fixture, "http-empty-error"), "Error 500: Synthetic status", Some("json")),
            (case(&fixture, "http-invalid-json-error"), "Error 500: Synthetic status", Some("json")),
            (case(&fixture, "http-json-falsy-error"), "Error 500: Internal Server Error", None),
            (case(&fixture, "http-content-type-case"), r#"{"error":"plain by SDK"}"#, None),
            (&empty_reason, "Error 500: ", Some("json")),
        ];
        for (index, (entry, message, failure)) in cases.into_iter().enumerate() {
            let server = serve(|_, _| vec![reply(entry, End::Finish)]).await;
            let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
            let Settlement::Failed(Failure::Http { status_code: 500, message: actual, body_failure, .. }) = &result.outcome.settlement
                else { panic!("case {index}: {:?}", result.outcome) };
            assert_eq!(actual.as_unicode(), Some(message), "case {index}");
            assert_eq!(body_failure.map(caught), failure, "case {index}");
        }
        // A chunked error body cut before its terminator is a caught reader failure.
        let server = serve(|_, _| vec![respond("500 Synthetic status", &[("Content-Type", "application/json")], vec![br#"{"error":"#.to_vec()], End::Reset)]).await;
        let result = run(&prepared, &server.origin, false, None, Stop::Never).await;
        assert!(matches!(&result.outcome.settlement, Settlement::Failed(Failure::Http { body_failure: Some(BodyFailure::Reader), message, .. })
            if message.as_unicode() == Some("Error 500: Synthetic status")), "{:?}", result.outcome);
        // The same cut in a 2xx stream fails the operation and keeps the accepted snapshot.
        let server = serve(|_, _| vec![respond("200 OK", &[("Content-Type", "application/json")],
            vec![b"{\"message\":{\"content\":\"partial\"}}\n".to_vec()], End::Reset)]).await;
        let result = run(&prepared, &server.origin, true, None, Stop::Never).await;
        assert!(matches!(result.outcome.settlement, Settlement::Failed(Failure::Decode(_))), "{:?}", result.outcome);
        assert_eq!(result.snapshots, [json!("partial")]);
        assert_eq!(result.outcome.last_accepted.as_ref().map(|snapshot| json_value(&snapshot.value)), Some(json!("partial")));
        assert!(result.outcome.in_flight.is_none());
    }

    #[tokio::test]
    async fn pinned_fixture_outcomes_replay_through_the_native_transport() {
        // Native preparation never assigns message names, so these name-marker cases have no native input.
        const WITHOUT_NATIVE_INPUT: [&str; 2] = ["buffered-prepared-name", "buffered-earliest-speaker"];
        // thinkingMode is resolved before transport; the transport receives expectedThink.
        const FIELDS: [&str; 13] = ["id", "body", "expected", "gate", "stream", "thinkingMode", "expectedThink", "boundPersona",
            "fragmentBytes", "status", "statusText", "contentType", "lifecycle"];
        let fixture = transport_fixture(); let request = &fixture["request"];
        assert_eq!(request["model"], MODEL);
        let pinned = fixture["cases"].as_array().unwrap().iter().filter(|case| case.get("expected").is_some()).collect::<Vec<_>>();
        assert_eq!(pinned.len(), 26);
        for id in WITHOUT_NATIVE_INPUT { assert!(pinned.iter().any(|case| case["id"] == id), "{id}"); }
        let mut mismatches = Vec::new(); let mut replayed = 0;
        for entry in pinned.into_iter().filter(|case| !WITHOUT_NATIVE_INPUT.contains(&case["id"].as_str().unwrap())) {
            let id = entry["id"].as_str().unwrap();
            for key in entry.as_object().unwrap().keys() { assert!(FIELDS.contains(&key.as_str()), "{id}: unsupported pinned field {key}"); }
            let expected = &entry["expected"];
            let lifecycle = entry.get("lifecycle").and_then(Json::as_str);
            let streaming = entry.get("stream").and_then(Json::as_bool).unwrap_or(false);
            let think = match entry.get("expectedThink") {
                None => None,
                Some(Json::Bool(value)) => Some(Think::Boolean(*value)),
                Some(value) => Some(Think::Level(match value.as_str().unwrap() {
                    "low" => ThinkLevel::Low, "medium" => ThinkLevel::Medium, "high" => ThinkLevel::High, other => panic!("{id}: think {other}") })),
            };
            let user = if entry.get("boundPersona").and_then(Json::as_bool).unwrap_or(false) { "boundUserName" } else { "selectedUserName" };
            let prepared = turn(request["characterName"].as_str().unwrap(), request[user].as_str().unwrap());
            let server = serve(|_, _| vec![reply(entry, if lifecycle.is_some() { End::Hold } else { End::Finish })]).await;
            let stop = match lifecycle {
                // A held body offers no later publication to reject, so the consumer stops through the shared cancellation.
                Some("consumer-cancel") => Stop::AfterFirstOffer,
                Some("terminal-open") | None => Stop::Never,
                Some(other) => panic!("{id}: lifecycle {other}"),
            };
            let result = run(&prepared, &server.origin, streaming, think, stop).await;
            replayed += 1;
            let mut mismatch = |what: String| mismatches.push(format!("{id}: {what}"));

            let mut body = serde_json::Map::new();
            body.insert("model".into(), json!(MODEL)); body.insert("messages".into(), json!([])); body.insert("stream".into(), json!(streaming));
            if let Some(think) = entry.get("expectedThink") { body.insert("think".into(), think.clone()); }
            let body = serde_json::to_vec(&body).unwrap();
            let requests = server.requests();
            if requests.len() != 1 || requests[0].method != "POST" || requests[0].path != "/api/chat" || requests[0].body != body {
                mismatch(format!("requests {:?}", requests.iter().map(|request| (&request.method, &request.path, String::from_utf8_lossy(&request.body))).collect::<Vec<_>>()));
            }
            let snapshots = Json::Array(result.snapshots.clone());
            if result.sequences != (1..=result.sequences.len() as u64).collect::<Vec<_>>() { mismatch(format!("sequences {:?}", result.sequences)); }
            if result.outcome.last_accepted.as_ref().map(|snapshot| json_value(&snapshot.value)) != result.snapshots.last().cloned() || result.outcome.in_flight.is_some() {
                mismatch(format!("retained {:?}", result.outcome));
            }
            match (expected["outcome"].as_str().unwrap(), &result.outcome.settlement) {
                ("success", Settlement::Normal) => if snapshots != json!([expected["result"]]) { mismatch(format!("snapshots {snapshots}")); },
                ("stream-complete", Settlement::Normal) | ("consumer-cancelled", Settlement::Cancelled(CancelOrigin::Caller)) =>
                    if snapshots != expected["snapshots"] { mismatch(format!("snapshots {snapshots}")); },
                ("error", Settlement::Failed(failure)) => {
                    let message = match failure { Failure::Sdk { message, .. } | Failure::Http { message, .. } => message.as_unicode(), _ => None };
                    if message != expected["errorMessage"].as_str() { mismatch(format!("failure {failure:?}")); }
                    if expected.get("snapshots").is_some_and(|value| *value != snapshots) { mismatch(format!("snapshots {snapshots}")); }
                    match failure {
                        Failure::Http { status_code, error, .. } => if expected["errorName"] != "ResponseError" || expected["statusCode"] != *status_code
                            || numbers_as_f64(&expected["sdkError"]) != json_value(error) { mismatch(format!("response error {failure:?}")); },
                        _ => if expected.get("errorName").is_some() || expected.get("statusCode").is_some() { mismatch(format!("not a response error {failure:?}")); },
                    }
                }
                (outcome, settlement) => mismatch(format!("expected {outcome}, settled {settlement:?}")),
            }
            if lifecycle.is_some() { server.client_closed().await; }
        }
        assert_eq!(replayed, 24);
        assert!(mismatches.is_empty(), "{mismatches:#?}");
    }
}
