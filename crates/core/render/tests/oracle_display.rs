//! Replays `tests/fixtures/rust-core/render/display.*.json`, recorded by
//! `benchmarks/rust-core-oracles/render-display.oracle.test.ts` from `ParseMarkdown(source, null, 'notrim', ...)`.
//!
//! The pipeline runs the stages in the app's order: inlays, thoughts and tools, styles, the KaTeX spans with what
//! KaTeX returned in the recording, `renderMarkdown` on `mdHighlight`, then the highlight placeholders with what
//! highlight.js returned. Every TeX string and every highlighted block must be the one JS passed, and the HTML and
//! the inlay registrations must match. A case marked `decline` must answer that `Unsupported`, and no other may. A case
//! with `segmenter` runs for an engine with those `Intl.Segmenter` tables (`none`: without one, as the recording ran
//! it); the others for the recording's Unicode 16 engine. `registry: disposed` renders with a disposed inlay registry.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::text::Text;
use risunest_render::{
    encode_style, find_math, locate_highlights, parse_inlay_assets, parse_thoughts_and_tools, render_display_markdown, splice_highlights,
    Engine, HighlightKind, InlayEntry, InlayInputs, InlayKind, InlaySource, JsEngine, QuoteSettings, RenderError, ThoughtLabels, ThoughtMode,
    UnicodeVersion,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/display.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/display.expected.json");

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum Units {
    Plain(String),
    Units { u: Vec<u16> },
}
impl Units {
    fn units(&self) -> Vec<u16> {
        match self {
            Units::Plain(text) => text.encode_utf16().collect(),
            Units::Units { u } => u.clone(),
        }
    }
}

#[derive(Deserialize)]
struct Labels {
    cot: String,
    #[serde(rename = "toolCalled")]
    tool_called: String,
    error: String,
}

#[derive(Deserialize)]
struct CaseFile { schema: u32, labels: Labels, cases: Vec<Case> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Settings {
    custom_quotes: Option<bool>,
    custom_quotes_data: Option<[String; 4]>,
    unformat_quotes: Option<bool>,
    blockquote_styling: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inlay { id: String, #[serde(rename = "type")] kind: String, mime: String, width: Option<f64>, height: Option<f64> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Case {
    id: String,
    source: Units,
    settings: Option<Settings>,
    thought_mode: Option<String>,
    hide_all_images: Option<bool>,
    reservation: Option<bool>,
    inlays: Option<Vec<Inlay>>,
    decline: Option<String>,
    segmenter: Option<String>,
    registry: Option<String>,
}

#[derive(Deserialize)]
struct ExpectedFile {
    schema: u32,
    #[serde(rename = "fixtureSha256")]
    fixture_sha256: String,
    cases: Vec<ExpectedCase>,
}

#[derive(Deserialize)]
struct ExpectedCase { id: String, result: Recorded }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    html: Units,
    math: Vec<(Units, Option<Units>)>,
    highlights: Vec<(Units, String, Option<Units>)>,
    registrations: Vec<(u64, Units)>,
}

fn budget() -> Budget {
    let limits = Limits {
        max_owned_bytes: 1 << 30,
        max_json_bytes: 1 << 24,
        max_text_units: 1 << 24,
        max_values: 1 << 22,
        max_depth: 128,
        max_work: 1 << 30,
    };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
}

fn text(units: &[u16], budget: &Budget) -> Text { Text::from_units(units, budget).expect("text") }
fn plain(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).expect("text") }
fn units(value: &Text) -> Vec<u16> { value.units().collect() }
fn show(units: &[u16]) -> String { String::from_utf16_lossy(units) }

/// The display pipeline of one case, or the first difference from the recording.
fn replay_case(case: &Case, recorded: &Recorded, labels: &Labels) -> Result<(), String> {
    let budget = budget();
    let sources: Vec<InlayEntry> = case.inlays.iter().flatten().map(|inlay| InlayEntry {
        id: plain(&inlay.id, &budget),
        source: InlaySource {
            kind: match inlay.kind.as_str() {
                "image" => InlayKind::Image,
                "video" => InlayKind::Video,
                "audio" => InlayKind::Audio,
                _ => InlayKind::Signature,
            },
            mime: plain(&inlay.mime, &budget),
            width: inlay.width,
            height: inlay.height,
        },
    }).collect();
    let first_slot = match case.registry.as_deref() {
        None => Some(0),
        Some("disposed") => None,
        Some(other) => panic!("registry {other}"),
    };
    let inputs = InlayInputs { sources: &sources, hide_all_images: case.hide_all_images.unwrap_or(false), first_slot,
        image_reservation: case.reservation.unwrap_or(false) };
    let segmenter = match case.segmenter.as_deref() {
        None => Some(UnicodeVersion::Unicode16),
        Some("none") => None,
        Some("before16") => Some(UnicodeVersion::Before16),
        Some("unicode17") => Some(UnicodeVersion::Unicode17),
        Some("from18") => Some(UnicodeVersion::From18),
        Some(other) => panic!("segmenter {other}"),
    };
    let js = JsEngine { segmenter, ..JsEngine::UNICODE_16 };
    let source = text(&case.source.units(), &budget);
    let unsupported = |error: RenderError| format!("{error:?}");
    let inlaid = parse_inlay_assets(&source, &inputs, &budget).map_err(unsupported)?;
    let registrations: Vec<(u64, Vec<u16>)> = inlaid.registrations.iter().map(|entry| (entry.slot, units(&entry.id))).collect();
    let expected: Vec<(u64, Vec<u16>)> = recorded.registrations.iter().map(|(slot, id)| (*slot, id.units())).collect();
    if registrations != expected { return Err(format!("registrations {registrations:?}, js {expected:?}")); }
    let mode = match case.thought_mode.as_deref() {
        Some("recent") => ThoughtMode::Recent,
        Some("collapsed") => ThoughtMode::Collapsed,
        _ => ThoughtMode::Off,
    };
    let (cot, tool_called, error) = (plain(&labels.cot, &budget), plain(&labels.tool_called, &budget), plain(&labels.error, &budget));
    let thoughts = parse_thoughts_and_tools(&inlaid.text, mode, &ThoughtLabels { cot: &cot, tool_called: &tool_called }, &js, &budget)
        .map_err(unsupported)?;
    let styled = encode_style(&thoughts, &budget).map_err(unsupported)?;
    let tex: Vec<Vec<u16>> = find_math(&styled, &budget).map_err(unsupported)?.iter().map(units).collect();
    let js_tex: Vec<Vec<u16>> = recorded.math.iter().map(|(tex, _)| tex.units()).collect();
    if tex != js_tex { return Err(format!("math {:?}, js {:?}", tex.iter().map(|t| show(t)).collect::<Vec<_>>(), js_tex.iter().map(|t| show(t)).collect::<Vec<_>>())); }
    let math: Vec<Option<Text>> = recorded.math.iter().map(|(_, value)| value.as_ref().map(|value| text(&value.units(), &budget))).collect();
    let settings = case.settings.as_ref();
    let flag = |pick: fn(&Settings) -> Option<bool>| settings.and_then(pick).unwrap_or(false);
    let data = settings.and_then(|settings| settings.custom_quotes_data.as_ref()).map(|data| data.each_ref().map(|quote| plain(quote, &budget)));
    let quotes = QuoteSettings::from_settings(flag(|s| s.custom_quotes), data, flag(|s| s.unformat_quotes), flag(|s| s.blockquote_styling), &budget)
        .map_err(unsupported)?;
    if !math.is_empty() {
        let missing = render_display_markdown(&styled, Engine::Highlight, &quotes, &[], &js, &budget);
        if !matches!(missing, Err(RenderError::Unsupported("markdown-katex"))) { return Err(format!("without math: {missing:?}")); }
    }
    let rendered = render_display_markdown(&styled, Engine::Highlight, &quotes, &math, &js, &budget).map_err(unsupported)?;
    let highlights = locate_highlights(&rendered, &budget).map_err(unsupported)?;
    let calls: Vec<(Vec<u16>, &str)> = highlights.iter().filter_map(|highlight| match highlight.kind {
        HighlightKind::Language { language, .. } => Some((units(&highlight.code), language)),
        _ => None,
    }).collect();
    let js_calls: Vec<(Vec<u16>, &str)> = recorded.highlights.iter().map(|(code, language, _)| (code.units(), language.as_str())).collect();
    if calls != js_calls { return Err(format!("highlight calls {calls:?}, js {js_calls:?}")); }
    let values: Vec<Option<Text>> = recorded.highlights.iter().map(|(_, _, value)| value.as_ref().map(|value| text(&value.units(), &budget))).collect();
    let html = units(&splice_highlights(&rendered, &highlights, &values, &error, &budget).map_err(unsupported)?);
    let js = recorded.html.units();
    if html != js { return Err(format!("rendered\n  rust {:?}\n  js   {:?}", show(&html), show(&js))); }
    Ok(())
}

/// The replay runs on a thread with the 8 MiB stack of a stage thread, like the Markdown replay.
#[test]
fn replays_the_display_oracle() {
    std::thread::Builder::new().stack_size(8 << 20).spawn(replay).unwrap().join().unwrap();
}

fn replay() {
    let cases: CaseFile = serde_json::from_slice(CASES).expect("cases");
    let expected: ExpectedFile = serde_json::from_slice(EXPECTED).expect("expected");
    assert_eq!(expected.schema, cases.schema);
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest, "regenerate the expected file");
    assert_eq!(expected.cases.len(), cases.cases.len());
    let mut failures = Vec::new();
    let (mut compared, mut declined) = (0, 0);
    for (case, recorded) in cases.cases.iter().zip(&expected.cases) {
        assert_eq!(case.id, recorded.id);
        let outcome = replay_case(case, &recorded.result, &cases.labels);
        let failure = match (&case.decline, outcome) {
            (Some(name), Err(found)) if found == format!("Unsupported({name:?})") => None,
            (Some(name), other) => Some(format!("expected decline {name}, got {other:?}")),
            (None, Err(error)) => Some(error),
            (None, Ok(())) => None,
        };
        if let Some(failure) = failure { failures.push(format!("{}: {failure}", case.id)); }
        if case.decline.is_some() { declined += 1 } else { compared += 1 }
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
    println!("display oracle: {compared} compared, {declined} declined");
}
