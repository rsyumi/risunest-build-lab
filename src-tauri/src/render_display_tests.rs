use super::*;
use serde_json::{json, Value};

fn request(text: Value, mode: &str) -> RenderDisplayRequest {
    request_with_character(text, mode, Value::Null)
}

fn request_with_character(text: Value, mode: &str, character: Value) -> RenderDisplayRequest {
    serde_json::from_value(json!({
        "text": text, "mode": mode, "character": character, "hideAllImages": false, "firstSlot": 0, "imageReservation": false,
        "thoughtMode": "off", "cot": "Thoughts", "toolCalled": "Tool {{tool}}", "error": "Error",
        "quotes": { "customQuotes": false, "customQuotesData": null, "unformatQuotes": false, "blockquoteStyling": false },
        "js": { "caseUnicode": "unicode16", "segmenter": "unicode16" },
    }))
    .expect("request")
}

fn reply(value: Value) -> RenderDisplayReply {
    serde_json::from_value(value).expect("reply")
}

fn wire(response: &RenderDisplayResponse) -> Value {
    serde_json::to_value(response).expect("response")
}

/// Starts a render that waits for its KaTeX answer and returns its session.
fn waiting(sessions: &RenderDisplaySessions) -> u32 {
    let response = wire(&sessions.start(request(json!("$$x$$"), "normal")));
    assert_eq!(response["step"], json!({ "kind": "math", "tex": ["x"] }), "{response}");
    response["session"].as_u64().expect("session") as u32
}

fn math() -> RenderDisplayReply {
    reply(json!({ "kind": "math", "values": ["<math>x</math>"] }))
}

/// A render asks for one answer at a time in the wire form and ends its session with the HTML.
#[test]
fn a_session_takes_one_answer_per_step_and_ends_with_the_html() {
    let sessions = RenderDisplaySessions::default();
    let response = wire(&sessions.start(request(json!("$$x$$\n\n```js\nf()\n```"), "normal")));
    assert_eq!(response["step"], json!({ "kind": "math", "tex": ["x"] }), "{response}");
    let id = response["session"].as_u64().expect("session") as u32;
    let response = wire(&sessions.resume(id, math()));
    assert_eq!(response["session"], json!(id), "{response}");
    assert_eq!(response["step"]["kind"], "highlights", "{response}");
    assert_eq!(response["step"]["blocks"][0]["language"], "javascript", "{response}");
    let response = wire(&sessions.resume(id, reply(json!({ "kind": "highlights", "values": ["<b>f()</b>"] }))));
    assert_eq!(response["session"], Value::Null, "{response}");
    assert_eq!(response["step"]["kind"], "done", "{response}");
    let html = response["step"]["html"].as_str().expect("html");
    assert!(html.contains("<math>x</math>") && html.contains("<b>f()</b>"), "{html}");
    assert_eq!(response["step"]["trim"], true);
    assert_eq!(wire(&sessions.resume(id, math())), json!({ "declined": "display-session" }));
}

/// The camelCase character wire fields reach the render, including whether source tags must fall back to TS.
#[test]
fn character_wire_sources_throw_controls_the_source_decline() {
    let sessions = RenderDisplaySessions::default();
    for sources_throw in [false, true] {
        let request = request_with_character(json!("{{source::char}}"), "normal", json!({
            "assetWidth": 100, "chatId": 3, "chaId": "synthetic-character",
            "characterImage": null, "userImage": null, "sourcesThrow": sources_throw,
        }));
        let response = wire(&sessions.start(request));
        if sources_throw {
            assert_eq!(response, json!({ "declined": "asset-source" }));
        } else {
            assert_eq!(response["step"]["kind"], "scripts", "{response}");
            let id = response["session"].as_u64().expect("session") as u32;
            sessions.cancel(id);
        }
    }
}

/// A string with an isolated surrogate goes in and comes back as UTF-16 units; a well-formed one as a string.
#[test]
fn isolated_surrogates_travel_as_units() {
    let sessions = RenderDisplaySessions::default();
    let response = wire(&sessions.start(request(json!({ "u": [0x61, 0xd800] }), "pretranslate")));
    assert_eq!(response["session"], Value::Null, "{response}");
    assert_eq!(response["step"]["html"], json!({ "u": [0x61, 0xd800] }), "{response}");
    let response = wire(&sessions.start(request(json!({ "u": [0x61, 0xd83d, 0xde00] }), "pretranslate")));
    assert_eq!(response["step"]["html"], json!("a\u{1f600}"), "{response}");
}

/// A reply of another kind declines and leaves the request open for the right one.
#[test]
fn a_reply_of_another_kind_keeps_the_session() {
    let sessions = RenderDisplaySessions::default();
    let id = waiting(&sessions);
    assert_eq!(wire(&sessions.resume(id, reply(json!({ "kind": "scripts", "text": "x" })))), json!({ "declined": "display-reply" }));
    assert_eq!(wire(&sessions.resume(id, math()))["step"]["kind"], "done");
}

/// Past the cap the oldest session is dropped and the newest still answers.
#[test]
fn the_oldest_session_is_dropped_past_the_cap() {
    let sessions = RenderDisplaySessions::default();
    let ids: Vec<u32> = (0..=MAX_SESSIONS).map(|_| waiting(&sessions)).collect();
    assert_eq!(wire(&sessions.resume(ids[0], math())), json!({ "declined": "display-session" }));
    assert_eq!(wire(&sessions.resume(ids[1], math()))["step"]["kind"], "done");
    assert_eq!(wire(&sessions.resume(ids[MAX_SESSIONS], math()))["step"]["kind"], "done");
}

/// A cancel or a reload ends a waiting session, and a render that runs, a start included, does not keep its session
/// when it finishes.
#[test]
fn cancel_and_reload_end_sessions_even_while_they_run() {
    let sessions = RenderDisplaySessions::default();
    let (cancelled, reloaded) = (waiting(&sessions), waiting(&sessions));
    sessions.cancel(cancelled);
    assert_eq!(wire(&sessions.resume(cancelled, math())), json!({ "declined": "display-session" }));
    sessions.reset_renderer_session();
    assert_eq!(wire(&sessions.resume(reloaded, math())), json!({ "declined": "display-session" }));

    let ends: [fn(&RenderDisplaySessions, u32); 2] = [RenderDisplaySessions::cancel, |sessions, _| sessions.reset_renderer_session()];
    for end in ends {
        let id = waiting(&sessions);
        let (run, entry) = sessions.take(id).expect("waiting");
        end(&sessions, id);
        // The resume that was running answers with another request, which no session waits for any more.
        assert_eq!(wire(&sessions.finish(run, Some(entry), Ok(DisplayStep::Math(Vec::new())))), json!({ "failed": "budget" }));
        assert_eq!(wire(&sessions.resume(id, math())), json!({ "declined": "display-session" }));

        let (run, budget) = sessions.begin().expect("room");
        end(&sessions, run.id);
        assert_eq!(wire(&sessions.render_start(run, budget, request(json!("$$x$$"), "normal"))), json!({ "failed": "budget" }));
    }
}

/// A render that does not fit in the total beside the bytes the waiting sessions hold and a session's bytes for each
/// render that runs is declined, and fits again once another ends.
#[test]
fn renders_that_do_not_fit_together_are_declined() {
    let done = || request(json!("a"), "pretranslate");
    let sessions = RenderDisplaySessions::with_caps(1 << 20, 1 << 20);
    let id = waiting(&sessions);
    assert_eq!(wire(&sessions.start(done())), json!({ "declined": "display-memory" }));
    sessions.cancel(id);
    assert_eq!(wire(&sessions.start(done()))["step"]["kind"], "done");

    let sessions = RenderDisplaySessions::with_caps(1 << 20, (2 << 20) - 1);
    let id = waiting(&sessions);
    let (run, _budget) = sessions.begin().expect("room beside a waiting session");
    // A resume would make two renders run, so it ends its session.
    assert_eq!(wire(&sessions.resume(id, math())), json!({ "declined": "display-memory" }));
    assert_eq!(wire(&sessions.resume(id, math())), json!({ "declined": "display-session" }));
    assert_eq!(wire(&sessions.start(done())), json!({ "declined": "display-memory" }));
    drop(run);
    assert_eq!(wire(&sessions.start(done()))["step"]["kind"], "done");
}

/// A render past a session's bytes fails as `budget`.
#[test]
fn a_render_past_its_session_bytes_fails() {
    let text = json!("a".repeat(100_000));
    assert_eq!(wire(&RenderDisplaySessions::with_caps(64 << 10, 1 << 20).start(request(text.clone(), "pretranslate"))), json!({ "failed": "budget" }));
    assert_eq!(wire(&RenderDisplaySessions::default().start(request(text, "pretranslate")))["step"]["kind"], "done");
}

/// A render that panics no longer counts against the total.
#[test]
fn a_render_that_panics_gives_its_room_back() {
    let sessions = RenderDisplaySessions::with_caps(1 << 20, 1 << 20);
    let panicking = sessions.clone();
    let worker = std::thread::spawn(move || {
        let (_run, _budget) = panicking.begin().expect("room");
        panic!("the render panicked");
    });
    assert!(worker.join().is_err());
    assert_eq!(wire(&sessions.start(request(json!("a"), "pretranslate")))["step"]["kind"], "done");
}

/// Every page load handler resets the sessions, so a reload stops the renders of the page it replaces. The mock runtime
/// loads no page, so this reads the handlers of each platform's branch.
#[test]
fn every_page_load_handler_resets_the_sessions() {
    let source = include_str!("lib.rs");
    assert!(source.contains(".manage(render_display::RenderDisplaySessions::default())"));
    let marker = "try_state::<render_display::RenderDisplaySessions>() {";
    let handlers: Vec<&str> = source.split(".on_page_load(").skip(1).collect();
    assert!(!handlers.is_empty());
    assert_eq!(source.matches(marker).count(), handlers.len());
    for handler in handlers {
        let after = handler.find(marker).map(|at| handler[at + marker.len()..].trim_start());
        assert!(after.is_some_and(|after| after.starts_with("state.reset_renderer_session();")), "{handler}");
    }
}
