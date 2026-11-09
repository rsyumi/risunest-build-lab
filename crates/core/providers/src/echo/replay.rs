//! Replays actual `requestChatDataMain` observations, including its requested timer delay.

use super::plan;
use crate::common::js::tests::budget;
use crate::common::replay_kit::{parse, same_hash, Jv};

const CASES: &[u8] = include_bytes!("../../../../../tests/fixtures/rust-core/providers/echo/results.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../../tests/fixtures/rust-core/providers/echo/results.expected.json");

#[test]
fn result_and_delay_match_the_typescript_catalog_route() {
    let b = budget();
    let fixture = parse(CASES, &b);
    let expected = parse(EXPECTED, &b);
    same_hash(CASES, &expected);
    let cases = fixture.need("cases").list();
    let records = expected.need("cases").list();
    assert_eq!(cases.len(), 13);
    assert_eq!(records.len(), cases.len());
    for (entry, record) in cases.iter().zip(records) {
        let id = entry.need("id").string();
        assert_eq!(record.need("id").string(), id);
        let setting = |name| entry.get("db").and_then(|db| db.get(name)).filter(|value| !matches!(value, Jv::Null));
        let message = setting("echoMessage").map(|value| value.text(&b));
        let delay = setting("echoDelay").map(Jv::num);
        let planned = plan(message, delay, &b).unwrap();
        let result = record.need("result");
        assert_eq!(result.need("model").need("id").string(), "echo_model", "{id}");
        assert_eq!(result.need("model").need("format").num(), 19.0, "{id}");
        let delays = result.need("requestedDelaysMs").list();
        assert_eq!(delays.len(), usize::from(planned.requested_delay_ms.is_some()), "{id}");
        if let Some(ms) = planned.requested_delay_ms { assert_eq!(delays[0].num(), ms, "{id}"); }
        let outcome = result.need("outcome");
        assert_eq!(outcome.need("type").string(), "success", "{id}");
        let expected_units: Vec<u16> = outcome.need("textUnits").list().iter().map(|value| value.num() as u16).collect();
        assert_eq!(planned.output.texts.len(), 1, "{id}");
        assert_eq!(planned.output.texts[0].units().collect::<Vec<_>>(), expected_units, "{id}");
        assert!(planned.output.thoughts.is_empty(), "{id}");
        assert!(planned.output.tool_calls.is_empty(), "{id}");
        assert!(planned.output.usage.is_none(), "{id}");
        assert!(planned.output.finish.is_none(), "{id}");
    }
}
