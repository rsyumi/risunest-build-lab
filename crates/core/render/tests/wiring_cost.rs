//! Measures the display render protocol for the wiring decision on the synthetic messages that
//! `benchmarks/rust-core-oracles/render-timing.test.ts` timed in Node. Write that file first, then run in release with
//! `RISUNEST_RENDER_TIMING_INPUT=<file> cargo test --release -p risunest-render --test wiring_cost -- --ignored --nocapture`.
//!
//! For each message the host's answers come from the Node recording, already built, so the host side costs nothing.
//! `loop` is the stateless protocol as the host drives it: one `render_display` call per request, each rerunning the
//! stages before it, with the answers borrowed. `once` is one `render_display` call with every answer present, which
//! runs each stage once. `session` is a `DisplaySession` started from a copy of the input and resumed with a copy of
//! each answer, as a host that deserializes each reply once would. The payload sizes are the JSON bytes each call would
//! carry, counting strings as escaped JSON strings and ignoring the other punctuation. The peaks are the bytes the
//! budget held at most: the stateless loop's answers sit outside it, the session's copies inside. The stage table times
//! each public stage on the text the one before it produced.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::text::Text;
use risunest_render::{
    encode_style, find_inlays, find_math, locate_highlights, parse_additional_assets, parse_inlay_assets, parse_thoughts_and_tools, render_display,
    render_display_markdown, splice_highlights, AssetAnswers, AssetCharacter, AssetInputs, AssetMode, AssetPath, AssetStep, DisplayAnswers,
    DisplayCharacter, DisplayInput, DisplayLabels, DisplayMode, DisplayReply, DisplayRequest, DisplaySession, DisplayStep, Engine, HighlightKind,
    InlayEntry, InlayInputs, InlayKind, InlaySource, JsEngine, QuoteSettings, ThoughtLabels, ThoughtMode,
};
use serde::Deserialize;
use std::hint::black_box;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Labels { cot: String, tool_called: String, error: String }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quotes { custom_quotes: Option<bool>, custom_quotes_data: Option<[String; 4]>, unformat_quotes: Option<bool>, blockquote_styling: Option<bool> }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Host {
    asset_width: Option<f64>,
    hide_all_images: bool,
    character_image: String,
    user_image: String,
    #[serde(rename = "chatID")]
    chat_id: f64,
    cha_id: String,
    reservation: bool,
    quotes: Quotes,
}

#[derive(Deserialize)]
struct Inlay { id: String, #[serde(rename = "type")] kind: String, mime: String, width: Option<f64>, height: Option<f64> }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Found { src_paths: Vec<String>, ext: Option<String> }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    output: String,
    lookups: Vec<(usize, String, String, Option<Found>)>,
    files: Vec<(usize, String, String)>,
    scripts: Vec<(String, String)>,
    inlay_calls: Vec<String>,
    math: Vec<(String, Option<String>)>,
    highlights: Vec<(String, String, Option<String>)>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct NodeTime { total_ms: f64, string_path_ms: f64 }

#[derive(Deserialize)]
struct Input { id: String, length: usize, source: String, record: Record, ts: NodeTime }

#[derive(Deserialize)]
struct File { schema: u32, node: String, labels: Labels, host: Host, inlays: Vec<Inlay>, inputs: Vec<Input> }

fn budget() -> Budget {
    let limits = Limits { max_owned_bytes: 1 << 32, max_json_bytes: 1 << 26, max_text_units: 1 << 26, max_values: 1 << 24, max_depth: 128,
        max_work: usize::MAX };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
}

fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).expect("text") }

/// The bytes of `value` as a JSON string.
fn json_len(value: &Text) -> usize {
    let value = String::from_utf16_lossy(&value.units().collect::<Vec<u16>>());
    2 + value.len() + value.bytes().filter(|byte| matches!(byte, b'"' | b'\\' | b'\n' | b'\r' | b'\t')).count()
}

/// Every answer, built from the recording.
#[derive(Default)]
struct Full {
    lookups: [Option<Vec<Option<AssetPath>>>; 2],
    urls: [Option<Vec<Text>>; 2],
    scripted: Option<Text>,
    inlays: Option<Vec<InlayEntry>>,
    math: Option<Vec<Option<Text>>>,
    highlights: Option<Vec<Option<Text>>>,
}

impl Full {
    fn view(&self) -> DisplayAnswers<'_> {
        DisplayAnswers {
            assets: [0, 1].map(|pass| AssetAnswers { lookups: self.lookups[pass].as_deref(), urls: self.urls[pass].as_deref() }),
            scripted: self.scripted.as_ref(),
            inlays: self.inlays.as_deref(),
            math: self.math.as_deref(),
            highlights: self.highlights.as_deref(),
        }
    }

    /// Adds the answer to `step` to `view`, as a host that answers at once would.
    fn answer<'a>(&'a self, step: &DisplayStep, view: &mut DisplayAnswers<'a>) {
        match step {
            DisplayStep::AssetLookups { pass, .. } => view.assets[*pass].lookups = self.lookups[*pass].as_deref(),
            DisplayStep::AssetUrls { pass, .. } => view.assets[*pass].urls = self.urls[*pass].as_deref(),
            DisplayStep::Scripts(_) => view.scripted = self.scripted.as_ref(),
            DisplayStep::Inlays(_) => view.inlays = self.inlays.as_deref(),
            DisplayStep::Math(_) => view.math = self.math.as_deref(),
            DisplayStep::Highlights(_) => view.highlights = self.highlights.as_deref(),
            DisplayStep::Done(_) => {}
        }
    }

    /// The answer to `step` as a session reply, copied.
    fn reply(&self, step: &DisplayStep, budget: &Budget) -> DisplayReply {
        let copy = |value: &Text| value.copy(budget).expect("copy");
        let values = |list: &Option<Vec<Option<Text>>>| list.as_ref().expect("answer").iter().map(|value| value.as_ref().map(copy)).collect();
        match step {
            DisplayStep::AssetLookups { pass, .. } => DisplayReply::AssetLookups(self.lookups[*pass].as_ref().expect("lookups").iter()
                .map(|found| found.as_ref().map(|found| AssetPath { src_paths: found.src_paths.iter().map(copy).collect(), ext: found.ext.as_ref().map(copy) }))
                .collect()),
            DisplayStep::AssetUrls { pass, .. } => DisplayReply::AssetUrls(self.urls[*pass].as_ref().expect("URLs").iter().map(copy).collect()),
            DisplayStep::Scripts(_) => DisplayReply::Scripts(copy(self.scripted.as_ref().expect("scripts"))),
            DisplayStep::Inlays(_) => DisplayReply::Inlays(self.inlays.as_ref().expect("inlays").iter().map(|entry| InlayEntry {
                id: copy(&entry.id),
                source: InlaySource { kind: entry.source.kind, mime: copy(&entry.source.mime), width: entry.source.width, height: entry.source.height },
            }).collect()),
            DisplayStep::Math(_) => DisplayReply::Math(values(&self.math)),
            DisplayStep::Highlights(_) => DisplayReply::Highlights(values(&self.highlights)),
            DisplayStep::Done(_) => unreachable!("no reply to the result"),
        }
    }

    /// The JSON bytes of the answer to `step`.
    fn answer_len(&self, step: &DisplayStep) -> usize {
        let paths = |found: &Option<AssetPath>| found.as_ref().map_or(4, |found| found.src_paths.iter().map(json_len).sum::<usize>() + found.ext.as_ref().map_or(4, json_len));
        let values = |list: &Option<Vec<Option<Text>>>| list.iter().flatten().map(|value| value.as_ref().map_or(4, json_len)).sum::<usize>();
        match step {
            DisplayStep::AssetLookups { pass, .. } => self.lookups[*pass].iter().flatten().map(paths).sum(),
            DisplayStep::AssetUrls { pass, .. } => self.urls[*pass].iter().flatten().map(json_len).sum(),
            DisplayStep::Scripts(_) => self.scripted.as_ref().map_or(0, json_len),
            DisplayStep::Inlays(_) => self.inlays.iter().flatten().map(|entry| json_len(&entry.id) + json_len(&entry.source.mime) + 24).sum(),
            DisplayStep::Math(_) => values(&self.math),
            DisplayStep::Highlights(_) => values(&self.highlights),
            DisplayStep::Done(_) => 0,
        }
    }
}

/// The JSON bytes of a step.
fn step_len(step: &DisplayStep) -> usize {
    match step {
        DisplayStep::AssetLookups { lookups, .. } => lookups.iter().map(|lookup| json_len(&lookup.name) + 8).sum(),
        DisplayStep::AssetUrls { requests, .. } => requests.iter().map(|request| json_len(&request.path) + 8).sum(),
        DisplayStep::Scripts(text) => json_len(text),
        DisplayStep::Inlays(ids) | DisplayStep::Math(ids) => ids.iter().map(json_len).sum(),
        DisplayStep::Highlights(found) => found.iter().map(|highlight| json_len(&highlight.lang) + json_len(&highlight.code) + 16).sum(),
        DisplayStep::Done(output) => json_len(&output.html) + output.registrations.iter().map(|entry| json_len(&entry.id) + 8).sum::<usize>(),
    }
}

fn step_name(step: &DisplayStep) -> &'static str {
    match step {
        DisplayStep::AssetLookups { pass: 0, .. } => "lookups0",
        DisplayStep::AssetLookups { .. } => "lookups1",
        DisplayStep::AssetUrls { pass: 0, .. } => "urls0",
        DisplayStep::AssetUrls { .. } => "urls1",
        DisplayStep::Scripts(_) => "scripts",
        DisplayStep::Inlays(_) => "inlays",
        DisplayStep::Math(_) => "math",
        DisplayStep::Highlights(_) => "highlights",
        DisplayStep::Done(_) => "done",
    }
}

/// Median milliseconds per run over 11 samples, each sized to run about 50 ms.
fn measure(mut operation: impl FnMut()) -> f64 {
    let started = Instant::now();
    operation();
    let once = started.elapsed().as_nanos().max(1) as f64;
    let iterations = ((50_000_000.0 / once) as usize).clamp(1, 100_000);
    let mut samples = (0..11).map(|_| {
        let started = Instant::now();
        for _ in 0..iterations { operation(); }
        started.elapsed().as_nanos() as f64 / iterations as f64 / 1e6
    }).collect::<Vec<_>>();
    samples.sort_by(f64::total_cmp);
    samples[5]
}

struct Owned { source: Text, cha_id: Text, character_image: Text, user_image: Text, cot: Text, tool_called: Text, error: Text, quotes: QuoteSettings }

impl Owned {
    /// The session form of `input`, copied.
    fn request(&self, input: &DisplayInput, quotes: &Quotes, budget: &Budget) -> DisplayRequest {
        let copy = |value: &Text| value.copy(budget).expect("copy");
        let character = input.character.expect("character");
        DisplayRequest {
            text: copy(&self.source), mode: input.mode,
            character: Some(DisplayCharacter { asset_width: character.asset_width, chat_id: character.chat_id, cha_id: copy(&self.cha_id),
                character_image: Some(copy(&self.character_image)), user_image: Some(copy(&self.user_image)), sources_throw: false }),
            hide_all_images: input.hide_all_images, first_slot: input.first_slot, image_reservation: input.image_reservation, thought_mode: input.thought_mode,
            cot: copy(&self.cot), tool_called: copy(&self.tool_called), error: copy(&self.error), quotes: quote_settings(quotes, budget), js: *input.js,
        }
    }
}

fn quote_settings(quotes: &Quotes, budget: &Budget) -> QuoteSettings {
    QuoteSettings::from_settings(quotes.custom_quotes.unwrap_or(false),
        quotes.custom_quotes_data.as_ref().map(|data| data.each_ref().map(|quote| text(quote, budget))), quotes.unformat_quotes.unwrap_or(false),
        quotes.blockquote_styling.unwrap_or(false), budget).expect("quotes")
}

/// Runs a session to its result, answering from `full`.
fn run_session(owned: &Owned, input: &DisplayInput, quotes: &Quotes, full: &Full, budget: &Budget) {
    let (mut session, mut step) = DisplaySession::start(owned.request(input, quotes, budget), budget).expect("start");
    while !matches!(step, DisplayStep::Done(_)) {
        let reply = full.reply(&step, budget);
        step = session.resume(reply, budget).expect("resume");
    }
    black_box(step);
}

/// Runs the stateless loop to its result, answering from `full`.
fn run_loop(input: &DisplayInput, full: &Full, budget: &Budget) {
    let mut view = DisplayAnswers::default();
    loop {
        let step = render_display(input, &view, budget).expect("render");
        if matches!(step, DisplayStep::Done(_)) {
            black_box(step);
            break;
        }
        full.answer(&step, &mut view);
    }
}

/// The time of each public stage on the text the one before it produced, with the recorded answers.
fn stage_times(input: &DisplayInput, full: &Full, budget: &Budget) -> String {
    let character = input.character.expect("character");
    let inputs = AssetInputs { character, hide_all_images: input.hide_all_images, mode: AssetMode::Normal };
    let pass = |data: &Text, pass: usize| {
        let answers = AssetAnswers { lookups: full.lookups[pass].as_deref(), urls: full.urls[pass].as_deref() };
        match parse_additional_assets(data, &inputs, &answers, budget).expect("assets") {
            AssetStep::Done(text) => text,
            other => panic!("asset pass {pass}: {other:?}"),
        }
    };
    let mut parts = Vec::new();
    let first = pass(input.text, 0);
    parts.push(("assets", measure(|| { black_box(pass(input.text, 0)); })));
    let scripted = full.scripted.as_ref().expect("scripts");
    let data = if scripted.equals(&first) {
        parts.push(("assets again", 0.0));
        scripted.copy(budget).expect("copy")
    } else {
        parts.push(("assets again", measure(|| { black_box(pass(scripted, 1)); })));
        pass(scripted, 1)
    };
    parts.push(("find inlays", measure(|| { black_box(find_inlays(&data, budget).expect("inlays")); })));
    let inlay_inputs = InlayInputs { sources: full.inlays.as_deref().unwrap_or(&[]), hide_all_images: input.hide_all_images, first_slot: input.first_slot,
        image_reservation: input.image_reservation };
    let inlaid = parse_inlay_assets(&data, &inlay_inputs, budget).expect("inlays");
    parts.push(("inlays", measure(|| { black_box(parse_inlay_assets(&data, &inlay_inputs, budget).expect("inlays")); })));
    let labels = ThoughtLabels { cot: input.labels.cot, tool_called: input.labels.tool_called };
    let thoughts = parse_thoughts_and_tools(&inlaid.text, input.thought_mode, &labels, input.js, budget).expect("thoughts");
    parts.push(("thoughts", measure(|| { black_box(parse_thoughts_and_tools(&inlaid.text, input.thought_mode, &labels, input.js, budget).expect("thoughts")); })));
    let styled = encode_style(&thoughts, budget).expect("style");
    parts.push(("style", measure(|| { black_box(encode_style(&thoughts, budget).expect("style")); })));
    parts.push(("find math", measure(|| { black_box(find_math(&styled, budget).expect("math")); })));
    let math = full.math.as_deref().unwrap_or(&[]);
    let rendered = render_display_markdown(&styled, Engine::Highlight, input.quotes, math, input.js, budget).expect("markdown");
    parts.push(("markdown", measure(|| { black_box(render_display_markdown(&styled, Engine::Highlight, input.quotes, math, input.js, budget).expect("markdown")); })));
    let found = locate_highlights(&rendered, budget).expect("highlights");
    parts.push(("find highlights", measure(|| { black_box(locate_highlights(&rendered, budget).expect("highlights")); })));
    let values = full.highlights.as_deref().unwrap_or(&[]);
    parts.push(("splice", measure(|| { black_box(splice_highlights(&rendered, &found, values, input.labels.error, budget).expect("splice")); })));
    parts.iter().map(|(name, ms)| format!("{name} {ms:.3}")).collect::<Vec<_>>().join(", ")
}

fn run(file: &File) {
    let budget = budget();
    let host = &file.host;
    let quotes = &host.quotes;
    let js = JsEngine::UNICODE_16;
    println!("node {}, {} messages", file.node, file.inputs.len());
    let mut stages = Vec::new();
    println!("| message | units | calls | loop ms | once ms | session ms | loop/session | per call ms | node total ms | node string ms | stateless KB in/out | kept KB in/out | peak KB loop/session |");
    println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: |");
    for message in &file.inputs {
        let owned = Owned {
            source: text(&message.source, &budget), cha_id: text(&host.cha_id, &budget), character_image: text(&host.character_image, &budget),
            user_image: text(&host.user_image, &budget), cot: text(&file.labels.cot, &budget), tool_called: text(&file.labels.tool_called, &budget),
            error: text(&file.labels.error, &budget),
            quotes: quote_settings(quotes, &budget),
        };
        let character = AssetCharacter { asset_width: host.asset_width, chat_id: host.chat_id, cha_id: &owned.cha_id,
            character_image: Some(&owned.character_image), user_image: Some(&owned.user_image), sources_throw: false };
        let input = DisplayInput {
            text: &owned.source, mode: DisplayMode::Notrim, character: Some(&character), hide_all_images: host.hide_all_images, first_slot: Some(0),
            image_reservation: host.reservation, thought_mode: ThoughtMode::Off,
            labels: DisplayLabels { cot: &owned.cot, tool_called: &owned.tool_called, error: &owned.error }, quotes: &owned.quotes, js: &js,
        };
        let record = &message.record;
        // Answers each request from the recording, as the pipeline replay does, and checks the result.
        let mut full = Full::default();
        let mut steps = Vec::new();
        loop {
            let step = render_display(&input, &full.view(), &budget).expect("render");
            match &step {
                DisplayStep::AssetLookups { pass, lookups } => {
                    let recorded: Vec<_> = record.lookups.iter().filter(|lookup| lookup.0 == *pass).collect();
                    assert_eq!(recorded.len(), lookups.len(), "{} lookups", message.id);
                    full.lookups[*pass] = Some(recorded.iter().map(|lookup| lookup.3.as_ref().map(|found| AssetPath {
                        src_paths: found.src_paths.iter().map(|path| text(path, &budget)).collect(), ext: found.ext.as_ref().map(|ext| text(ext, &budget)),
                    })).collect());
                }
                DisplayStep::AssetUrls { pass, requests } => {
                    full.urls[*pass] = Some(requests.iter().map(|request| {
                        let path = String::from_utf16_lossy(&request.path.units().collect::<Vec<u16>>());
                        let found = record.files.iter().find(|file| file.0 == *pass && file.1 == path).expect("recorded URL");
                        text(&found.2, &budget)
                    }).collect());
                }
                DisplayStep::Scripts(_) => full.scripted = Some(text(&record.scripts[0].1, &budget)),
                DisplayStep::Inlays(ids) => {
                    assert_eq!(ids.len(), record.inlay_calls.len(), "{} inlays", message.id);
                    full.inlays = Some(file.inlays.iter().map(|inlay| InlayEntry {
                        id: text(&inlay.id, &budget),
                        source: InlaySource { kind: if inlay.kind == "image" { InlayKind::Image } else { InlayKind::Video }, mime: text(&inlay.mime, &budget),
                            width: inlay.width, height: inlay.height },
                    }).collect());
                }
                DisplayStep::Math(tex) => {
                    assert_eq!(tex.len(), record.math.len(), "{} math", message.id);
                    full.math = Some(record.math.iter().map(|(_, value)| value.as_ref().map(|value| text(value, &budget))).collect());
                }
                DisplayStep::Highlights(found) => {
                    let languages = found.iter().filter(|highlight| matches!(highlight.kind, HighlightKind::Language { .. })).count();
                    assert_eq!(languages, record.highlights.len(), "{} highlights", message.id);
                    full.highlights = Some(record.highlights.iter().map(|(_, _, value)| value.as_ref().map(|value| text(value, &budget))).collect());
                }
                DisplayStep::Done(output) => {
                    let html = String::from_utf16(&output.html.units().collect::<Vec<u16>>()).expect("html");
                    let at = html.chars().zip(record.output.chars()).take_while(|(rust, node)| rust == node).count();
                    let near = |value: &str| value.chars().skip(at.saturating_sub(80)).take(200).collect::<String>();
                    assert!(html == record.output, "{}: the render differs from Node's at {at}\n  rust {:?}\n  node {:?}", message.id, near(&html),
                        near(&record.output));
                }
            }
            let done = matches!(step, DisplayStep::Done(_));
            steps.push(step);
            if done { break; }
        }
        let calls = steps.len();
        let (mut session, mut step) = DisplaySession::start(owned.request(&input, quotes, &budget), &budget).expect("start");
        while !matches!(step, DisplayStep::Done(_)) {
            let reply = full.reply(&step, &budget);
            step = session.resume(reply, &budget).expect("resume");
        }
        let DisplayStep::Done(output) = step else { unreachable!() };
        assert!(String::from_utf16(&output.html.units().collect::<Vec<u16>>()).expect("html") == record.output, "{}: the session differs from Node's", message.id);
        let loop_ms = measure(|| run_loop(&input, &full, &budget));
        let once_ms = measure(|| { black_box(render_display(&input, &full.view(), &budget).expect("render")); });
        let session_ms = measure(|| run_session(&owned, &input, quotes, &full, &budget));
        let peak = |run: &dyn Fn(&Budget)| {
            let fresh = self::budget();
            run(&fresh);
            fresh.peak_bytes() as f64 / 1024.0
        };
        let (loop_peak, session_peak) = (peak(&|fresh: &Budget| run_loop(&input, &full, fresh)), peak(&|fresh: &Budget| run_session(&owned, &input, quotes, &full, fresh)));
        stages.push(format!("| {} | {} |", message.id, stage_times(&input, &full, &budget)));
        // The time of each call of the loop, every earlier answer present.
        let mut per_call = Vec::new();
        let mut view = DisplayAnswers::default();
        for step in &steps {
            per_call.push(format!("{} {:.3}", step_name(step), measure(|| { black_box(render_display(&input, &view, &budget).expect("render")); })));
            full.answer(step, &mut view);
        }
        // Payloads: the stateless host sends the input and every answer so far on each call; a kept session sends the
        // input once and then each answer once. Both receive every step.
        let input_len = json_len(&owned.source) + json_len(&owned.cha_id) + json_len(&owned.character_image) + json_len(&owned.user_image)
            + json_len(&owned.cot) + json_len(&owned.tool_called) + json_len(&owned.error) + 128;
        let answers: Vec<usize> = steps.iter().map(|step| full.answer_len(step)).collect();
        let replies: usize = steps.iter().map(step_len).sum();
        let stateless_in: usize = (0..calls).map(|call| input_len + answers[..call].iter().sum::<usize>()).sum();
        let kept_in: usize = input_len + answers.iter().sum::<usize>();
        let kb = |bytes: usize| bytes as f64 / 1024.0;
        println!("| {} | {} | {} | {:.3} | {:.3} | {:.3} | {:.2} | {} | {:.3} | {:.3} | {:.1} / {:.1} | {:.1} / {:.1} | {:.0} / {:.0} |", message.id, message.length,
            calls, loop_ms, once_ms, session_ms, loop_ms / session_ms, per_call.join(", "), message.ts.total_ms, message.ts.string_path_ms, kb(stateless_in),
            kb(replies), kb(kept_in), kb(replies), loop_peak, session_peak);
    }
    println!();
    println!("| message | stage ms |");
    println!("| --- | --- |");
    for row in stages { println!("{row}"); }
}

#[test]
#[ignore = "release measurement"]
fn display_render_wiring_cost() {
    let path = std::env::var("RISUNEST_RENDER_TIMING_INPUT").expect("RISUNEST_RENDER_TIMING_INPUT names the Node timing file");
    let file: File = serde_json::from_slice(&std::fs::read(path).expect("timing file")).expect("timing file");
    assert_eq!(file.schema, 1);
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || run(&file)).unwrap().join().unwrap();
}
