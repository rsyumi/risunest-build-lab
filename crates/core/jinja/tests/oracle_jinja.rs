//! Replays `tests/fixtures/rust-core/templates/jinja.*.json`, recorded from `@huggingface/jinja` 0.5.10 by
//! `benchmarks/rust-core-oracles/templates-jinja.oracle.test.ts`.
//!
//! A rendered case must render the same UTF-16 units. A case the JS engine rejects must be rejected in the same phase:
//! `new Template` failures by [`Template::new`], render failures by [`Template::render`]; a `raise_exception` message
//! must match. A case marked `decline` must answer that `Unsupported`, and no other case may decline.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::json::parse_json_text;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_jinja::{CaseTables, JinjaError, LocalTime, RenderOptions, Template};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/templates/jinja.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/templates/jinja.expected.json");

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
    template: Units,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    now: Option<Now>,
    #[serde(default)]
    decline: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Now { year: i32, month: u8, day: u8, hour: u8, minute: u8 }

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
#[serde(untagged)]
enum Recorded {
    Ok { ok: Units },
    Error { error: String, message: Units },
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

enum Outcome {
    Rendered(Vec<u16>),
    Parse(JinjaError),
    Render(JinjaError),
}

fn run(case: &Case) -> Outcome {
    let budget = budget();
    let source = Text::from_units(&case.template.units(), &budget).expect("template text");
    let template = match Template::new(&source, &budget) {
        Ok(template) => template,
        Err(error) => return Outcome::Parse(error),
    };
    let context = match &case.context {
        Some(json) => parse_json_text(&Text::from_str(json, &budget).expect("context text"), &budget).expect("context json"),
        None => Value::Undefined,
    };
    let options = RenderOptions {
        now: case.now.as_ref().map(|now| LocalTime { year: now.year, month: now.month, day: now.day, hour: now.hour, minute: now.minute }),
        month_names: None,
        // The recording's Node 24.12 maps case with Unicode 16 tables.
        case_tables: CaseTables::Unicode16,
    };
    match template.render(&context, &options, &budget) {
        Ok(text) => Outcome::Rendered(text.units().collect()),
        Err(error) => Outcome::Render(error),
    }
}

fn show(units: &[u16]) -> String { String::from_utf16_lossy(units) }

/// The replay runs on a thread with the 8 MiB stack the engine assumes of a stage thread, since the depth cases
/// recurse to the limits.
#[test]
fn replays_the_jinja_oracle() {
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
    let (mut rendered, mut rejected, mut declined) = (0, 0, 0);
    for (case, recorded) in cases.cases.iter().zip(&expected.cases) {
        assert_eq!(case.id, recorded.id);
        let outcome = run(case);
        if let Some(name) = &case.decline {
            match &outcome {
                Outcome::Parse(JinjaError::Unsupported(found)) | Outcome::Render(JinjaError::Unsupported(found)) if found == name => declined += 1,
                _ => failures.push(format!("{}: expected decline {name}", case.id)),
            }
            continue;
        }
        let failure = match (&recorded.result, &outcome) {
            (_, Outcome::Parse(JinjaError::Unsupported(name)) | Outcome::Render(JinjaError::Unsupported(name))) => {
                Some(format!("undeclared decline {name}"))
            }
            (_, Outcome::Parse(JinjaError::Budget(error)) | Outcome::Render(JinjaError::Budget(error))) => Some(format!("budget {error:?}")),
            (Recorded::Ok { ok }, Outcome::Rendered(units)) => {
                rendered += 1;
                (ok.units() != *units).then(|| format!("rendered\n  rust {:?}\n  js   {:?}", show(units), show(&ok.units())))
            }
            (Recorded::Error { error, message }, Outcome::Parse(found)) if error == "parse" => {
                rejected += 1;
                matches!(found, JinjaError::Raised(_)).then(|| format!("parse raised {found:?}, js {:?}", show(&message.units())))
            }
            (Recorded::Error { error, message }, Outcome::Render(found)) if error == "render" => {
                rejected += 1;
                match found {
                    JinjaError::Raised(units) if *units != message.units() => {
                        Some(format!("raised {:?}, js {:?}", show(units), show(&message.units())))
                    }
                    _ => None,
                }
            }
            (Recorded::Ok { ok }, other) => Some(format!("js rendered {:?}, rust {}", show(&ok.units()), describe(other))),
            (Recorded::Error { error, message }, other) => {
                Some(format!("js {error} error {:?}, rust {}", show(&message.units()), describe(other)))
            }
        };
        if let Some(failure) = failure { failures.push(format!("{}: {failure}", case.id)); }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.cases.len(), failures.join("\n"));
    assert!(rendered > 0 && rejected > 0 && declined > 0);
    println!("jinja oracle: {rendered} rendered, {rejected} rejected, {declined} declined");
}

fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Rendered(units) => format!("rendered {:?}", show(units)),
        Outcome::Parse(error) => format!("parse error {error:?}"),
        Outcome::Render(error) => format!("render error {error:?}"),
    }
}
