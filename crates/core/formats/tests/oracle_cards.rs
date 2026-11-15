//! Replays `tests/fixtures/rust-core/formats/cards.*.json`, recorded from `mapPreparedNativeCharacterCard`,
//! `createBaseV3`, `createBaseV2` and the CharX projection of `exportChar` by
//! `benchmarks/rust-core-oracles/formats-cards.oracle.test.ts`.
//!
//! A mapped card or exported card must stringify to the same text as TS's `JSON.stringify`: the same properties in the
//! same order, with the same values. An error TS throws on purpose must be the `Invalid` it names. A case marked
//! `decline` must answer that `Unsupported`, and no other case may decline.

use risunest_formats::card_export::{create_base_v2, create_base_v3, project_charx};
use risunest_formats::card_import::{map_prepared_card, PreparedAsset, PreparedCard};
use risunest_formats::ids::Ids;
use risunest_ir::budget::{Budget, Limits};
use risunest_ir::clock::Fixed;
use risunest_ir::json::{parse_json, stringify};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/formats/cards.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/formats/cards.expected.json");

/// The oracle's `uuid` mock: `00000000-0000-4000-8000-000000000001` and up, restarted for every case.
struct Counter(u64);

impl Ids for Counter {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        self.0 += 1;
        Ok(Text::from_str(&format!("00000000-0000-4000-8000-{:012}", self.0), budget)?)
    }
}

/// The messages TS throws on purpose, by the `Invalid` field the port names for each.
const INVALID: &[(&str, &str)] = &[
    ("Prepared native card data URI payloads are not allowed", "dataUri"),
    ("Prepared native asset token is missing:", "missingAsset"),
    ("Prepared native ccdefault asset requires a portrait logical ID", "ccdefaultPortrait"),
    ("Duplicate prepared native asset token:", "duplicateAssetToken"),
    ("Prepared native asset token and logical ID must not be empty", "assetToken"),
    ("Prepared native portrait logical ID must not be empty", "portraitLogicalId"),
    ("Prepared native v2 payload fields require a logical asset reference", "assetReference"),
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

fn portrait(case: &Value, budget: &Budget) -> Option<Text> {
    match member(case, "portraitLogicalId", budget) {
        Some(Value::Text(text)) => Some(text.copy(budget).expect("copy")),
        None => None,
        other => panic!("portraitLogicalId: {other:?}"),
    }
}

fn map(card: &Value, pairs: &[(Text, Text)], portrait: Option<&Text>, module: Option<&Value>, ids: &mut Counter, budget: &Budget) -> Result<Value, StageError> {
    let assets: Vec<PreparedAsset<'_>> = pairs.iter().map(|(token, logical_id)| PreparedAsset { token, logical_id }).collect();
    map_prepared_card(&PreparedCard { card, assets: &assets, portrait_logical_id: portrait, module }, ids, budget)
}

fn import(case: &Value, budget: &Budget) -> Result<Value, StageError> {
    let pairs: Vec<(Text, Text)> = member(case, "assets", budget).map(items).unwrap_or(&[]).iter().map(|asset| {
        let field = |key| match member(asset, key, budget) { Some(Value::Text(text)) => text.copy(budget).expect("copy"), other => panic!("{key}: {other:?}") };
        (field("token"), field("logicalId"))
    }).collect();
    let card = member(case, "card", budget).expect("card");
    map(card, &pairs, portrait(case, budget).as_ref(), member(case, "module", budget), &mut Counter(0), budget)
}

/// The object `{card, module}` the CharX closure returns.
fn charx(character: &Value, ids: &mut Counter, now: &Fixed, budget: &Budget) -> Result<Value, StageError> {
    let projected = project_charx(character, ids, now, budget)?;
    let mut value = parse_json("{}", budget).expect("object");
    value.define(text("card", budget), projected.card, budget).expect("card");
    value.define(text("module", budget), projected.module, budget).expect("module");
    Ok(value)
}

/// What the container carries: JSON text, read back.
fn reparse(value: &Value, budget: &Budget) -> Value {
    let written = stringify(value, budget).expect("stringify").expect("an object");
    parse_json(&string(&Value::Text(written)), budget).expect("reparse")
}

/// The asset references an exported card's container hands the import. The PNG export (`exportCharacterCard`) embeds
/// every asset whose URI `isKnownUri` does not name (`http://`, `https://`, `ccdefault:`, `embeded://`) as the chunk
/// `chara-ext-asset_:<n>`, `n` from 1, and the import stages it under the token `n`. A CharX archive embeds every asset
/// but remote ones and the import names it `__asset:native-charx-<index>`, as the oracle's `stageCharxAssets` writes.
fn member_mut<'a>(value: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    match value {
        Value::Object(entries) => entries.as_mut_slice().iter_mut().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value),
        other => panic!("expected an object, found {other:?}"),
    }
}

fn stage_chain_assets(card: &mut Value, via: &str, budget: &Budget) -> Vec<(Text, Text)> {
    let mut staged = Vec::new();
    let Some(Value::Array(assets)) = member_mut(member_mut(card, "data").expect("data"), "assets") else { return staged };
    for (index, asset) in assets.as_mut_slice().iter_mut().enumerate() {
        let uri = member_mut(asset, "uri").expect("uri");
        let current = string(uri);
        let known = match via {
            "png-v3" => ["http://", "https://", "ccdefault:", "embeded://"].iter().any(|prefix| current.starts_with(prefix)),
            _ => current.starts_with("http://") || current.starts_with("https://"),
        };
        if known { continue; }
        let (token, logical_id) = match via {
            "png-v3" => { let next = staged.len() + 1; (format!("{next}"), format!("png-asset-{next}")) }
            _ => (format!("native-charx-{index}"), format!("charx-asset-{index}")),
        };
        *uri = Value::Text(text(&format!("__asset:{token}"), budget));
        staged.push((text(&token, budget), text(&logical_id, budget)));
    }
    staged
}

/// An export fed back into import: the exported card (and CharX module) as their container stores them.
fn chain(case: &Value, budget: &Budget) -> Result<Value, StageError> {
    let now = Fixed(number(member(case, "now", budget).expect("now")));
    let character = member(case, "character", budget).expect("character");
    let via = string(member(case, "via", budget).expect("via"));
    let mut ids = Counter(0);
    let (card, module) = match via.as_str() {
        "png-v2" => (create_base_v2(character, budget)?, None),
        "png-v3" => (create_base_v3(character, &now, budget)?, None),
        "charx" => {
            let projected = project_charx(character, &mut ids, &now, budget)?;
            (projected.card, Some(projected.module))
        }
        other => panic!("unknown chain {other}"),
    };
    let mut card = reparse(&card, budget);
    let staged = stage_chain_assets(&mut card, &via, budget);
    // The CharX import reads `trigger` and `regex` (`[]` when absent) and `lorebook` from `module.risum`.
    let overlay = module.map(|module| {
        let mut module = reparse(&module, budget);
        let mut overlay = parse_json("{}", budget).expect("object");
        for key in ["trigger", "regex", "lorebook"] {
            let value = match module.take(key.as_bytes(), budget).expect("take") {
                Some(value) => value,
                None if key == "lorebook" => continue,
                None => parse_json("[]", budget).expect("array"),
            };
            overlay.define(text(key, budget), value, budget).expect(key);
        }
        overlay
    });
    map(&card, &staged, portrait(case, budget).as_ref(), overlay.as_ref(), &mut ids, budget)
}

fn run(kind: &str, case: &Value, budget: &Budget) -> Result<Value, StageError> {
    let now = || Fixed(number(member(case, "now", budget).expect("now")));
    let character = || member(case, "character", budget).expect("character");
    match kind {
        "import" => import(case, budget),
        "export" => create_base_v3(character(), &now(), budget),
        "export-v2" => create_base_v2(character(), budget),
        "charx" => charx(character(), &mut Counter(0), &now(), budget),
        "roundtrip" => create_base_v3(&import(case, budget)?, &now(), budget),
        "chain" => chain(case, budget),
        other => panic!("unknown kind {other}"),
    }
}

/// The replay runs on a thread with the 8 MiB stack the deep copies of a stage thread assume.
#[test]
fn replays_the_card_oracle() {
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
    let (mut mapped, mut exported, mut round_trips, mut chains, mut rejected, mut declined) = (0, 0, 0, 0, 0, 0);
    let (mut exported_v2, mut projected) = (0, 0);
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
        let failure = match (member(result, "ok", &budget), member(result, "error", &budget), &outcome) {
            (_, _, Err(StageError::Unsupported(name))) => Some(format!("undeclared decline {name}")),
            (Some(Value::Text(text)), _, Ok(value)) => {
                match kind.as_str() {
                    "import" => mapped += 1,
                    "export" => exported += 1,
                    "export-v2" => exported_v2 += 1,
                    "charx" => projected += 1,
                    "chain" => chains += 1,
                    _ => round_trips += 1,
                }
                let confirms = member(result, "confirms", &budget).map(number).unwrap_or(0.0);
                let written = stringify(value, &budget).expect("stringify").expect("an object");
                if confirms != 0.0 {
                    Some(format!("TS asked {confirms} times without a decline"))
                } else if !written.equals(text) {
                    Some(format!("mapped\n  rust {}\n  js   {}", string(&Value::Text(written)), string(&Value::Text(text.copy(&budget).unwrap()))))
                } else {
                    None
                }
            }
            (_, Some(message), Err(StageError::Invalid(found))) => {
                rejected += 1;
                let message = string(message);
                match INVALID.iter().find(|(prefix, _)| message.starts_with(prefix)) {
                    Some((_, name)) if found == name => None,
                    Some((_, name)) => Some(format!("TS threw {message:?} ({name}), rust Invalid {found}")),
                    None => Some(format!("TS threw {message:?}, which is not an error it raises on purpose, rust Invalid {found}")),
                }
            }
            (ok, error, other) => Some(format!("TS {} {}, rust {}", ok.map_or(String::new(), |v| format!("ok {v:?}")),
                error.map_or(String::new(), string), describe(other, &budget))),
        };
        if let Some(failure) = failure { failures.push(format!("{id}: {failure}")); }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
    assert!(mapped > 0 && exported > 0 && exported_v2 > 0 && projected > 0 && round_trips > 0 && chains > 0 && rejected > 0 && declined > 0);
    println!("card oracle: {mapped} mapped, {exported} exported as V3, {exported_v2} exported as V2, {projected} CharX projections, \
        {round_trips} round trips, {chains} export-import chains, {rejected} rejected, {declined} declined");
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
