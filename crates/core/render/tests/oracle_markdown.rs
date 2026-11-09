//! Replays `tests/fixtures/rust-core/render/markdown.*.json`, recorded from markdown-it 14.3.2 under the app's
//! configuration by `benchmarks/rust-core-oracles/render-markdown.oracle.test.ts`.
//!
//! Every case renders through both engines and must give the same UTF-16 units as `md.render` and
//! `mdHighlight.render`. A case marked `decline` must answer that `Unsupported` from both engines, and no other case may
//! decline. A case with `caseUnicode` renders for an engine with those case tables; the others for the recording's
//! Unicode 16 engine.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::text::Text;
use risunest_render::{render_markdown, Engine, JsEngine, RenderError, UnicodeVersion};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/markdown.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/markdown.expected.json");

#[derive(Deserialize)]
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
struct CaseFile { schema: u32, cases: Vec<Case> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    source: Units,
    #[serde(default)]
    decline: Option<String>,
    #[serde(default, rename = "caseUnicode")]
    case_unicode: Option<String>,
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
struct Recorded { md: Units, highlight: Units }

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

fn render(source: &[u16], engine: Engine, js: &JsEngine) -> Result<Vec<u16>, RenderError> {
    let budget = budget();
    let source = Text::from_units(source, &budget).expect("source text");
    render_markdown(&source, engine, js, &budget).map(|text| text.units().collect())
}

fn js_engine(case: &Case) -> JsEngine {
    let case_unicode = match case.case_unicode.as_deref() {
        None => Some(UnicodeVersion::Unicode16),
        Some("before16") => Some(UnicodeVersion::Before16),
        Some("unicode17") => Some(UnicodeVersion::Unicode17),
        Some("from18") => Some(UnicodeVersion::From18),
        Some("unknown") => None,
        Some(other) => panic!("caseUnicode {other}"),
    };
    JsEngine { case_unicode, ..JsEngine::UNICODE_16 }
}

fn show(units: &[u16]) -> String { String::from_utf16_lossy(units) }

/// The replay runs on a thread with the 8 MiB stack of a stage thread, since the nesting cases recurse to the limits.
#[test]
fn replays_the_markdown_oracle() {
    std::thread::Builder::new().stack_size(8 << 20).spawn(replay).unwrap().join().unwrap();
}

fn replay() {
    let cases: CaseFile = serde_json::from_slice(CASES).expect("cases");
    let expected: ExpectedFile = serde_json::from_slice(EXPECTED).expect("expected");
    assert_eq!(expected.schema, cases.schema);
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest, "regenerate the expected file");
    assert_eq!(expected.cases.len(), cases.cases.len());
    assert!(!cases.cases.is_empty());
    let mut failures = Vec::new();
    // Per category (the id prefix): compared and declined cases.
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (case, recorded) in cases.cases.iter().zip(&expected.cases) {
        assert_eq!(case.id, recorded.id);
        let category = case.id.split('-').next().expect("id");
        let source = case.source.units();
        for (engine, js) in [(Engine::Markdown, &recorded.result.md), (Engine::Highlight, &recorded.result.highlight)] {
            let outcome = render(&source, engine, &js_engine(case));
            let failure = match (&case.decline, outcome) {
                (Some(name), Err(RenderError::Unsupported(found))) if found == name => None,
                (Some(name), other) => Some(format!("expected decline {name}, got {:?}", other.map(|units| show(&units)))),
                (None, Err(error)) => Some(format!("{error:?}")),
                (None, Ok(units)) => {
                    let js = js.units();
                    (units != js).then(|| format!("rendered\n  rust {:?}\n  js   {:?}", show(&units), show(&js)))
                }
            };
            if let Some(failure) = failure { failures.push(format!("{} ({engine:?}): {failure}", case.id)); }
        }
        let entry = counts.entry(category).or_default();
        if case.decline.is_some() { entry.1 += 1 } else { entry.0 += 1 }
    }
    assert!(failures.is_empty(), "{} renders differ:\n{}", failures.len(), failures.join("\n"));
    let compared: usize = counts.values().map(|count| count.0).sum();
    let declined: usize = counts.values().map(|count| count.1).sum();
    assert!(compared > 0 && declined > 0);
    println!("markdown oracle: {compared} compared, {declined} declined, each through both engines");
    for (category, (compared, declined)) in &counts { println!("  {category}: {compared} compared, {declined} declined"); }
}
