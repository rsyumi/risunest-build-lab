//! Replays `tests/fixtures/rust-core/formats/modules.*.json`, recorded from `importModuleData`,
//! `activatePreparedNativeModuleContent`, `convertModuleToCharacter`, `exportModuleLegacy` and `exportModule` by
//! `benchmarks/rust-core-oracles/formats-modules.oracle.test.ts`.
//!
//! A mapped module, character or exported file must stringify to the same text as TS's `JSON.stringify`. A dialog or
//! error TS raises on purpose must be the `Invalid` the port names for it. A case marked `decline` must answer that
//! `Unsupported`, and no other case may decline.

use risunest_formats::card_export::project_charx;
use risunest_formats::card_import::{PreparedAsset, PreparedCard};
use risunest_formats::ids::Ids;
use risunest_formats::module_export::{convert_module_to_character, needs_none_image, risum_metadata, set_none_image};
use risunest_formats::module_import::{map_module_json, map_prepared_charx_module, map_prepared_risum};
use risunest_ir::budget::{Budget, Limits};
use risunest_ir::clock::Fixed;
use risunest_ir::json::{parse_json, stringify};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/formats/modules.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/formats/modules.expected.json");

/// The oracle's `uuid` mock and `createId`: `00000000-0000-4000-8000-000000000001` and up, restarted for every case.
struct Counter(u64);

impl Ids for Counter {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        self.0 += 1;
        Ok(Text::from_str(&format!("00000000-0000-4000-8000-{:012}", self.0), budget)?)
    }
}

/// What TS reports on purpose, as `<dialog kind or "error"> <message>`, by the `Invalid` field the port names for
/// each. `importModuleData` shows `noData` as an error dialog for a module without a name or id, and as a notice when
/// no branch matches.
const INVALID: &[(&str, &str)] = &[
    ("error-dialog noData", "moduleIdentity"),
    ("notice noData", "noData"),
    ("error-dialog This module cannot be imported because it contains a local MCP program configuration.", "stdioMcp"),
    ("error This module cannot be imported because it contains a local MCP program configuration.", "stdioMcp"),
    ("error Prepared RISUM asset metadata does not match its descriptors", "assetMetadata"),
    ("error Prepared RISUM asset tuple ", "assetTuple"),
    ("error MCP modules are not supported for character conversion.", "mcpModule"),
];

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

fn member<'a>(value: &'a Value, key: &str, budget: &Budget) -> Option<&'a Value> {
    value.get(key.as_bytes(), budget).expect("lookup")
}

fn member_mut<'a>(value: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match value {
        Value::Object(entries) => entries.as_mut_slice().iter_mut().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value),
        other => panic!("expected an object, found {other:?}"),
    }
}

fn string(value: &Value) -> String {
    match value {
        Value::Text(text) => String::from_utf16_lossy(&text.units().collect::<Vec<_>>()),
        other => panic!("expected a string, found {other:?}"),
    }
}

fn items(value: &Value) -> &[Value] {
    match value {
        Value::Array(values) => values.as_slice(),
        other => panic!("expected an array, found {other:?}"),
    }
}

fn number(value: &Value) -> f64 {
    match value {
        Value::Number(number) => *number,
        other => panic!("expected a number, found {other:?}"),
    }
}

fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).expect("text") }

fn texts(value: Option<&Value>, budget: &Budget) -> Vec<Text> {
    value.map(items).unwrap_or(&[]).iter().map(|item| match item {
        Value::Text(text) => text.copy(budget).expect("copy"),
        other => panic!("expected a string, found {other:?}"),
    }).collect()
}

fn object(pairs: Vec<(&str, Value)>, budget: &Budget) -> Value {
    let mut value = parse_json("{}", budget).expect("object");
    for (key, item) in pairs { value.define(text(key, budget), item, budget).expect(key); }
    value
}

/// The module the native CharX import appends: the card mapped as a character, converted, with a fresh id.
fn charx_module(case: &Value, budget: &Budget) -> Result<Value, StageError> {
    let pairs: Vec<(Text, Text)> = member(case, "assets", budget).map(items).unwrap_or(&[]).iter().map(|asset| {
        let field = |key| match member(asset, key, budget) { Some(Value::Text(text)) => text.copy(budget).expect("copy"), other => panic!("{key}: {other:?}") };
        (field("token"), field("logicalId"))
    }).collect();
    let assets: Vec<PreparedAsset<'_>> = pairs.iter().map(|(token, logical_id)| PreparedAsset { token, logical_id }).collect();
    let portrait = match member(case, "portraitLogicalId", budget) {
        Some(Value::Text(text)) => Some(text.copy(budget).expect("copy")),
        None => None,
        other => panic!("portraitLogicalId: {other:?}"),
    };
    let card = member(case, "card", budget).expect("card");
    let input = PreparedCard { card, assets: &assets, portrait_logical_id: portrait.as_ref(), module: member(case, "module", budget) };
    Ok(map_prepared_charx_module(&input, &mut Counter(0), budget)?.module)
}

/// The CharX file `exportModule` writes, as the card with its asset URIs blanked and the `module.risum` metadata.
fn module_charx(case: &Value, root: &Value, budget: &Budget) -> Result<Value, StageError> {
    let mut ids = Counter(0);
    let module = member(case, "module", budget).expect("module");
    let mut character = convert_module_to_character(module, root, &mut ids, budget)?;
    if needs_none_image(&character, budget)? {
        let Some(Value::Text(image)) = member(case, "noneImage", budget) else { panic!("noneImage") };
        set_none_image(&mut character, image, budget)?;
    }
    let now = Fixed(number(member(case, "now", budget).expect("now")));
    let projected = project_charx(&character, &mut ids, &now, budget)?;
    let mut card = projected.card;
    if let Some(Value::Array(assets)) = member_mut(member_mut(&mut card, "data").expect("data"), "assets") {
        for asset in assets.as_mut_slice() { *member_mut(asset, "uri").expect("uri") = Value::Text(text("", budget)); }
    }
    let risum = risum_metadata(&projected.module, budget)?;
    Ok(object(vec![("card", card), ("risum", risum)], budget))
}

fn run(kind: &str, case: &Value, budget: &Budget) -> Result<Value, StageError> {
    let empty = parse_json("{}", budget).expect("object");
    let root = member(case, "root", budget).unwrap_or(&empty);
    let module = || member(case, "module", budget).expect("module");
    match kind {
        "json" => map_module_json(member(case, "data", budget).expect("data"), &mut Counter(0), budget),
        "risum" => {
            let logical_ids = texts(member(case, "logicalIds", budget), budget);
            let logical_ids: Vec<&Text> = logical_ids.iter().collect();
            map_prepared_risum(member(case, "metadata", budget).expect("metadata"), &logical_ids, &mut Counter(0), budget)
        }
        "charx" => charx_module(case, budget),
        "to-character" => convert_module_to_character(module(), root, &mut Counter(0), budget),
        "risum-export" => risum_metadata(module(), budget),
        "module-charx" => module_charx(case, root, budget),
        other => panic!("unknown kind {other}"),
    }
}

/// What TS reported instead of a result, labelled as in `INVALID`.
fn rejection(result: &Value, budget: &Budget) -> Option<String> {
    if let Some(message) = member(result, "error", budget) { return Some(format!("error {}", string(message))); }
    let kind = member(result, "alert", budget).map(string)?;
    let label = match kind.as_str() { "error" => "error-dialog", "normal" => "notice", other => panic!("alert {other}") };
    Some(format!("{label} {}", string(member(result, "value", budget).expect("value"))))
}

/// The replay runs on a thread with the 8 MiB stack the deep copies of a stage thread assume.
#[test]
fn replays_the_module_oracle() {
    std::thread::Builder::new().stack_size(8 << 20).spawn(replay).unwrap().join().unwrap();
}

fn replay() {
    let budget = budget();
    let cases = parse_json(std::str::from_utf8(CASES).expect("utf-8"), &budget).expect("cases");
    let expected = parse_json(std::str::from_utf8(EXPECTED).expect("utf-8"), &budget).expect("expected");
    assert_eq!(number(member(&expected, "schema", &budget).unwrap()), number(member(&cases, "schema", &budget).unwrap()));
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(string(member(&expected, "fixtureSha256", &budget).unwrap()), digest, "regenerate the expected file");
    let cases = items(member(&cases, "cases", &budget).unwrap());
    let recorded = items(member(&expected, "cases", &budget).unwrap());
    assert_eq!(cases.len(), recorded.len());
    let mut failures = Vec::new();
    let mut matched = std::collections::BTreeMap::<String, usize>::new();
    let (mut rejected, mut declined) = (0, 0);
    for (case, record) in cases.iter().zip(recorded) {
        let id = string(member(case, "id", &budget).unwrap());
        assert_eq!(id, string(member(record, "id", &budget).unwrap()));
        let kind = string(member(case, "kind", &budget).unwrap());
        let result = member(record, "result", &budget).unwrap();
        // Each case gets its own budget, so a long case cannot use up the next one's work.
        let budget = self::budget();
        let outcome = run(&kind, case, &budget);
        if let Some(name) = member(case, "decline", &budget).map(string) {
            match &outcome {
                Err(StageError::Unsupported(found)) if *found == name => declined += 1,
                other => failures.push(format!("{id}: expected decline {name}, got {}", describe(other, &budget))),
            }
            continue;
        }
        let failure = match (member(result, "ok", &budget), rejection(result, &budget), &outcome) {
            (_, _, Err(StageError::Unsupported(name))) => Some(format!("undeclared decline {name}")),
            (Some(Value::Text(expected)), None, Ok(value)) => {
                *matched.entry(kind.clone()).or_default() += 1;
                let confirms = member(result, "confirms", &budget).map(number).unwrap_or(0.0);
                let written = stringify(value, &budget).expect("stringify").expect("an object");
                if confirms != 0.0 {
                    Some(format!("TS asked {confirms} times without a decline"))
                } else if !written.equals(expected) {
                    Some(format!("mapped\n  rust {}\n  js   {}", string(&Value::Text(written)), string(&Value::Text(expected.copy(&budget).unwrap()))))
                } else {
                    None
                }
            }
            (None, Some(label), Err(StageError::Invalid(found))) => {
                rejected += 1;
                match INVALID.iter().find(|(prefix, _)| label.starts_with(prefix)) {
                    Some((_, name)) if found == name => None,
                    Some((_, name)) => Some(format!("TS reported {label:?} ({name}), rust Invalid {found}")),
                    None => Some(format!("TS reported {label:?}, which it does not raise on purpose, rust Invalid {found}")),
                }
            }
            (ok, label, other) => Some(format!("TS {} {}, rust {}", ok.map_or(String::new(), |v| format!("ok {v:?}")),
                label.unwrap_or_default(), describe(other, &budget))),
        };
        if let Some(failure) = failure { failures.push(format!("{id}: {failure}")); }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
    for kind in ["json", "risum", "charx", "to-character", "risum-export", "module-charx"] {
        assert!(matched.get(kind).copied().unwrap_or(0) > 0, "no {kind} case matched");
    }
    assert!(rejected > 0 && declined > 0);
    println!("module oracle: matched {matched:?}, {rejected} rejected, {declined} declined");
}

fn describe(outcome: &Result<Value, StageError>, budget: &Budget) -> String {
    match outcome {
        Ok(value) => match stringify(value, budget) {
            Ok(Some(text)) => format!("ok {}", string(&Value::Text(text))),
            other => format!("ok {other:?}"),
        },
        Err(error) => format!("error {error:?}"),
    }
}
