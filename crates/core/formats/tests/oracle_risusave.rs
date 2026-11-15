//! Replays `tests/fixtures/rust-core/formats/risusave.*.json`, recorded from `prepareExternalDatabaseImport`,
//! `prepareUpstreamImport`, `assignIds`, `checkCharOrder` and the web `.risudat` import route, alone and through the
//! web runtime's `prepareDatabaseForPersistence`, by `benchmarks/rust-core-oracles/formats-risusave.oracle.test.ts`.
//!
//! Each prepared database must stringify to the same text as TS's `JSON.stringify`, and the constants the port carries
//! must be the ones TS has. A case marked `decline` must answer that `Unsupported`, and no other case may decline. TS
//! raises no error on purpose here, so every other case succeeds.

use risunest_formats::ids::Ids;
use risunest_formats::risusave::{
    assign_ids, check_char_order, constants, prepare_external_import, prepare_upstream_import, prepare_web_file_import,
};
use risunest_ir::budget::{Budget, Limits};
use risunest_ir::clock::Fixed;
use risunest_ir::json::{parse_json, stringify};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/formats/risusave.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/formats/risusave.expected.json");

/// The oracle's `uuid` mock and `createId`: `00000000-0000-4000-8000-000000000001` and up, restarted for every case.
struct Counter(u64);

impl Ids for Counter {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        self.0 += 1;
        Ok(Text::from_str(&format!("00000000-0000-4000-8000-{:012}", self.0), budget)?)
    }
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

fn run(kind: &str, case: &Value, budget: &Budget) -> Result<Value, StageError> {
    let database = || member(case, "database", budget).expect("database");
    let mut ids = Counter(0);
    match kind {
        "constants" => constants(budget),
        "external" => prepare_external_import(database(), budget),
        "upstream" => prepare_upstream_import(database(), &mut ids, budget),
        // The route decodes the save, then runs both steps; cold plugin storage is absent in these cases.
        "route" => prepare_upstream_import(&prepare_external_import(database(), budget)?, &mut ids, budget),
        // The whole web import: the route, then the persistence preparation with `Date.now()` at `now`.
        "chain" => prepare_web_file_import(database(), &Fixed(number(member(case, "now", budget).expect("now"))), &mut ids, budget),
        "assign-ids" => assign_ids(database(), &mut ids, budget),
        "char-order" => check_char_order(database(), budget),
        other => panic!("unknown kind {other}"),
    }
}

/// The replay runs on a thread with the 8 MiB stack the deep copies of a stage thread assume.
#[test]
fn replays_the_risusave_oracle() {
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
    let mut declined = 0;
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
        let failure = match (member(result, "ok", &budget), &outcome) {
            (_, Err(StageError::Unsupported(name))) => Some(format!("undeclared decline {name}")),
            (Some(Value::Text(expected)), Ok(value)) => {
                let written = stringify(value, &budget).expect("stringify").expect("an object");
                if written.equals(expected) {
                    *matched.entry(kind.clone()).or_default() += 1;
                    None
                } else {
                    Some(format!("mapped\n  rust {}\n  js   {}", string(&Value::Text(written)), string(&Value::Text(expected.copy(&budget).unwrap()))))
                }
            }
            (ok, other) => Some(format!("TS {}, rust {}", ok.map_or_else(|| format!("{:?}", member(result, "error", &budget)), |v| format!("ok {v:?}")), describe(other, &budget))),
        };
        if let Some(failure) = failure { failures.push(format!("{id}: {failure}")); }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
    for kind in ["constants", "external", "upstream", "route", "chain", "assign-ids", "char-order"] {
        assert!(matched.get(kind).copied().unwrap_or(0) > 0, "no {kind} case matched");
    }
    assert!(declined > 0);
    println!("risusave oracle: matched {matched:?}, {declined} declined");
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
