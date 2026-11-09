//! Replays `tests/fixtures/rust-core/lorebook/selection.*.json`, recorded from the real `loadLoreBookV3Prompt` by
//! `benchmarks/rust-core-oracles/lorebook.oracle.test.ts`.
//!
//! CBS is a literal processor that refuses any text `hasCbsSyntax` would parse, so
//! only cases marked `native: "unsupported-cbs"` reach CBS; a case marked `native: "unsupported-<name>"` must be
//! declined with that name. Regex keys run through `risunest-regex::engine::JsPatternEngine`. Token counts go through
//! the tokenizer crate's plugin counter, with its memo under `useTokenizerCaching`, and the harness answers
//! `Math.ceil(length / 4)` tokens, as the oracle's `tokenizerFunc` does. `Math.random()` is the case's tape.
//!
//! Each case runs twice: under the root case mapping the oracle recorded, and with `tailored_case`, where the
//! selector may only decline (`case-locale`) and only a case the oracle marks `tailoredCase`.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::chat::Role;
use risunest_ir::json::parse_json;
use risunest_ir::num::number_to_text;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::rng::Tape;
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, InjectOperation, LoreOutput, LoreSelector, Pattern, PatternEngine,
    StageError, Step, TextContext, TextOutput, TextProcessor, TokenCounter, TokenInput,
};
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_lorebook::matching::CaseTables;
use risunest_lorebook::selector::{LoreFailureSite, LoreInput, LorePhase, LorePolicy, LoreSelection};
use risunest_cbs_experimental::cbs::processor::{CaseTables as CbsCaseTables, Processor, Snapshot};
use risunest_regex::engine::JsPatternEngine;
use risunest_tokenizer::counter::{ChatAccounting, Counter};
use risunest_tokenizer::id::TokenizerId;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/lorebook/selection.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/lorebook/selection.expected.json");

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
    actives: Option<Vec<Active>>,
    #[serde(rename = "matchLog")]
    match_log: Option<Vec<Match>>,
    writes: Vec<(String, String)>,
    random: usize,
    tokenizer: Vec<TokenCall>,
    // Some text selection may lowercase maps differently under `tr`, `az`, `lt` or `el`.
    #[serde(rename = "tailoredCase")]
    tailored_case: bool,
    #[serde(rename = "regex", default)]
    _regex: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    error: Option<Thrown>,
    #[serde(rename = "orderedObservation", default)]
    ordered_observation: Option<OrderedObservation>,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct Active {
    depth: String,
    pos: String,
    prompt: String,
    role: String,
    order: String,
    tokens: u32,
    priority: String,
    source: String,
    inject: Option<Inject>,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct Inject { operation: String, location: String, param: String, lore: bool }

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct Match { prompt: String, source: String, activated: String }

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct TokenCall { text: String, tokens: usize }

#[derive(Deserialize, Debug, PartialEq)]
struct Thrown { name: String, message: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderedObservation {
    events: Vec<OrderedEvent>,
    #[serde(rename = "heldScriptstate")]
    held_scriptstate: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum OrderedEvent {
    CountEnter { text: String },
    CountComplete { text: String, tokens: usize },
    CountError { text: String, error: Thrown },
    SetterEnter { name: String, value: String },
    SetterComplete { name: String, value: String },
    SetterError { name: String, error: Thrown },
    LocaleLower { input: String, output: String },
    GateHeld,
    GateRelease,
}

fn budget() -> Budget {
    Budget::new(Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 22, max_values: 1 << 22,
        max_depth: 64, max_work: 1 << 40 }, Arc::new(AtomicBool::new(false))).unwrap()
}

fn units(text: &str) -> Vec<u16> { text.encode_utf16().collect() }
fn string(text: &Text) -> String { String::from_utf16_lossy(&text.units().collect::<Vec<_>>()) }
fn field<'a>(value: &'a Value, key: &str, budget: &Budget) -> Option<&'a Value> { value.get(key.as_bytes(), budget).unwrap() }

/// `String(Number(value))`, with `-0` spelled out as the oracle records it.
fn number(value: f64, budget: &Budget) -> String {
    if value == 0.0 && value.is_sign_negative() { return "-0".into(); }
    string(&number_to_text(value, budget).unwrap())
}

// JSON text for the database a case describes, so `parse_json` gives the selector JS key order.

fn write_string(text: impl IntoIterator<Item = u16>, out: &mut String) {
    out.push('"');
    for decoded in char::decode_utf16(text) {
        match decoded {
            Ok('"') => out.push_str("\\\""),
            Ok('\\') => out.push_str("\\\\"),
            Ok(character) if (character as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", character as u32)),
            Ok(character) => out.push(character),
            Err(error) => out.push_str(&format!("\\u{:04x}", error.unpaired_surrogate())),
        }
    }
    out.push('"');
}

fn write_json(value: &Value, out: &mut String, budget: &Budget) {
    match value {
        Value::Undefined | Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => out.push_str(&number(*value, budget)),
        Value::Text(text) => write_string(text.units(), out),
        Value::Array(values) => {
            out.push('[');
            for (index, value) in values.as_slice().iter().enumerate() {
                if index != 0 { out.push(','); }
                write_json(value, out, budget);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            let fields: Vec<(Vec<u16>, String)> = fields.as_slice().iter().map(|(name, value)| {
                let mut text = String::new();
                write_json(value, &mut text, budget);
                (name.units().collect(), text)
            }).collect();
            write_object(&fields, out);
        }
    }
}

fn write_object(fields: &[(Vec<u16>, String)], out: &mut String) {
    out.push('{');
    for (index, (name, value)) in fields.iter().enumerate() {
        if index != 0 { out.push(','); }
        write_string(name.iter().copied(), out);
        out.push(':');
        out.push_str(value);
    }
    out.push('}');
}

/// `{ ...layers[0], ...layers[1], ...extra }`: a later key replaces the value in place, a new key goes last.
fn spread(layers: &[Option<&Value>], extra: Vec<(&str, String)>, budget: &Budget) -> String {
    let mut fields: Vec<(Vec<u16>, String)> = Vec::new();
    let mut set = |name: Vec<u16>, value: String| match fields.iter_mut().find(|(key, _)| *key == name) {
        Some((_, slot)) => *slot = value,
        None => fields.push((name, value)),
    };
    for layer in layers.iter().flatten() {
        for (name, value) in layer.object().expect("case records are objects") {
            let mut text = String::new();
            write_json(value, &mut text, budget);
            set(name.units().collect(), text);
        }
    }
    for (name, value) in extra { set(units(name), value); }
    let mut out = String::new();
    write_object(&fields, &mut out);
    out
}

fn database_text(base: &Value, case: &Value, budget: &Budget) -> String {
    let chat = spread(&[field(base, "chat", budget), field(case, "chat", budget)], Vec::new(), budget);
    let character = spread(&[field(base, "character", budget), field(case, "character", budget)], vec![("chats", format!("[{chat}]"))], budget);
    let mut characters = vec![character];
    if let Some(others) = field(case, "others", budget) {
        for other in others.array().expect("others is an array") {
            let mut text = String::new();
            write_json(other, &mut text, budget);
            characters.push(text);
        }
    }
    spread(&[field(base, "root", budget), field(case, "root", budget)], vec![("characters", format!("[{}]", characters.join(",")))], budget)
}

// Stage doubles.

/// Identity for text without CBS syntax (`hasCbsSyntax`, `src/ts/parser/cbsSyntax.ts:2`), which is what
/// `risuChatParser` returns for it.
struct LiteralText;

fn has_cbs_syntax(text: &[u16]) -> bool {
    let lower = |unit: u16| if (b'A' as u16..=b'Z' as u16).contains(&unit) { unit + 32 } else { unit };
    (0..text.len()).any(|at| {
        let rest = &text[at..];
        if rest[0] == b'{' as u16 { return rest.get(1).is_some_and(|next| *next == b'{' as u16 || *next == b'#' as u16); }
        if rest[0] != b'<' as u16 { return false; }
        ["user>", "char>", "bot>"].iter().any(|tag| {
            let tag = units(tag);
            rest.len() > tag.len() && rest[1..=tag.len()].iter().zip(&tag).all(|(unit, expected)| lower(*unit) == *expected)
        })
    })
}

impl TextProcessor for LiteralText {
    fn process(&mut self, input: &Text, _context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
        if has_cbs_syntax(&input.units().collect::<Vec<_>>()) { return Err(StageError::Unsupported(Cow::Borrowed("cbs"))); }
        Ok(TextOutput { text: input.copy(budget)?, effects: Vec::new() })
    }
    fn apply(&mut self, _effect: &Effect, _budget: &Budget) -> Result<(), StageError> { Ok(()) }
}

// Replay.

#[derive(PartialEq, Clone)]
enum Stored { Text(String), Other }

struct Replayed {
    result: Result<LoreOutput, StageError>,
    /// `take_effects` after an error: the writes made before it.
    effects: Vec<Effect>,
    match_log: Vec<Match>,
    random: usize,
    tokenizer: Vec<TokenCall>,
}

fn select(database: &Value, random: Vec<f64>, patterns: impl PatternEngine, tailored: bool, budget: &Budget) -> Replayed {
    let mut input = match LoreInput::from_database(database, 0, budget) {
        Ok(input) => input,
        Err(error) => return Replayed { result: Err(error), effects: Vec::new(), match_log: Vec::new(), random: 0, tokenizer: Vec::new() },
    };
    input.tailored_case = tailored;
    // A memo case names its own plugin provider, which is part of the TS cache key, so the TS cache starts empty
    // like this memo.
    let mut counter = Counter::new(TokenizerId::Plugin, ChatAccounting::default());
    if matches!(field(database, "useTokenizerCaching", budget), Some(Value::Bool(true))) { counter = counter.with_plugin_memo(); }
    let mut selection = LoreSelection::new(input, LiteralText, counter, patterns, Tape::new(random));
    let mut tokenizer = Vec::new();
    let mut step = selection.select(budget);
    let result = loop {
        match step {
            Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text })) => {
                let tokens = text.len_units().div_ceil(4);
                tokenizer.push(TokenCall { text: string(&text), tokens });
                step = selection.resume(HostReply::Tokens(vec![0.0; tokens]), budget);
            }
            Ok(Step::NeedsHost(other)) => panic!("unexpected host request {other:?}"),
            Ok(Step::Done(output)) => break Ok(output),
            Err(error) => break Err(error),
        }
    };
    let match_log = match &result {
        Ok(output) => {
            assert!(selection.match_log().is_empty(), "completed match log belongs to the output");
            output.match_log.as_slice()
        }
        Err(_) => selection.match_log(),
    }.iter()
        .map(|entry| Match { prompt: string(&entry.prompt), source: string(&entry.source), activated: string(&entry.activated) }).collect();
    let effects = selection.take_effects();
    let (_, _, _, tape) = selection.into_parts();
    Replayed { result, effects, match_log, random: tape.consumed(), tokenizer }
}

fn actives(output: &LoreOutput, budget: &Budget) -> Vec<Active> {
    output.fragments.iter().map(|fragment| Active {
        depth: number(fragment.depth, budget),
        pos: string(&fragment.position),
        prompt: string(&fragment.prompt),
        role: match fragment.role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" }.into(),
        order: number(fragment.order, budget),
        tokens: fragment.tokens,
        priority: number(fragment.priority, budget),
        source: string(&fragment.source),
        inject: fragment.inject.as_ref().map(|inject| Inject {
            operation: match inject.operation { InjectOperation::Append => "append", InjectOperation::Prepend => "prepend", InjectOperation::Replace => "replace" }.into(),
            location: string(&inject.location),
            param: string(&inject.param),
            lore: inject.lore,
        }),
    }).collect()
}

/// The oracle's scriptstate diff: writes land on `'$' + name` in place or last, then every key whose value changed.
fn writes(initial: Option<&Value>, effects: &[Effect]) -> Result<Vec<(String, String)>, String> {
    let before: Vec<(String, Stored)> = initial.and_then(Value::object).unwrap_or(&[]).iter().map(|(name, value)| {
        (string(name), match value { Value::Text(text) => Stored::Text(string(text)), _ => Stored::Other })
    }).collect();
    let mut after = before.clone();
    for effect in effects {
        let Effect::ChatVariable { name, value: Value::Text(value) } = effect else { return Err(format!("unexpected effect {effect:?}")) };
        let key = format!("${}", string(name));
        let value = Stored::Text(string(value));
        match after.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, slot)) => *slot = value,
            None => after.push((key, value)),
        }
    }
    Ok(after.into_iter().filter(|(key, value)| !before.iter().any(|(old, previous)| old == key && previous == value)).map(|(key, value)| {
        let Stored::Text(value) = value else { unreachable!("writes are strings") };
        (key[1..].to_string(), value)
    }).collect())
}

fn compare_writes(database: &Value, effects: &[Effect], result: &Recorded, budget: &Budget, differ: &mut dyn FnMut(&str, String)) {
    let chat = field(database, "characters", budget).and_then(Value::array).and_then(|characters| characters.first())
        .and_then(|character| field(character, "chats", budget)).and_then(Value::array).and_then(|chats| chats.first()).unwrap();
    match writes(field(chat, "scriptstate", budget), effects) {
        Ok(writes) if writes == result.writes => {}
        Ok(writes) => differ("writes", format!("ts {:?} rust {writes:?}", result.writes)),
        Err(error) => differ("writes", error),
    }
}

/// Reports every difference between a replay and the TS result; returns whether TS threw.
fn compare(replayed: &Replayed, result: &Recorded, database: &Value, budget: &Budget, differ: &mut dyn FnMut(&str, String)) -> bool {
    if replayed.random != result.random { differ("random", format!("ts {} rust {}", result.random, replayed.random)); }
    if replayed.tokenizer != result.tokenizer { differ("tokenizer", format!("ts {:?}\n rust {:?}", result.tokenizer, replayed.tokenizer)); }
    match (&replayed.result, &result.error) {
        (Ok(output), None) => {
            let actives = actives(output, budget);
            if Some(&actives) != result.actives.as_ref() { differ("actives", format!("ts {:?}\n rust {actives:?}", result.actives)); }
            if Some(&replayed.match_log) != result.match_log.as_ref() {
                differ("matchLog", format!("ts {:?}\n rust {:?}", result.match_log, replayed.match_log));
            }
            compare_writes(database, &output.effects, result, budget, differ);
            false
        }
        (Err(error), Some(thrown)) => {
            let same = match error {
                StageError::Invalid(_) => thrown.name == "TypeError",
                StageError::Limit(name) => name == "rng-tape" && thrown.message == "rng-tape exhausted",
                _ => false,
            };
            if !same { differ("error", format!("ts {}: {} rust {error:?}", thrown.name, thrown.message)); }
            // TS keeps what it wrote before throwing; the selection keeps the same writes for its owner.
            compare_writes(database, &replayed.effects, result, budget, differ);
            true
        }
        (Ok(_), Some(thrown)) => {
            differ("error", format!("ts threw {}: {}, rust finished", thrown.name, thrown.message));
            true
        }
        (Err(error), None) => {
            differ("error", format!("rust failed with {error:?}"));
            false
        }
    }
}

fn declined(result: &Result<LoreOutput, StageError>, name: &str) -> bool {
    matches!(result, Err(StageError::Unsupported(declined)) if declined == name)
}

#[test]
fn replays_the_typescript_lorebook_oracle() {
    let expected: ExpectedFile = serde_json::from_slice(EXPECTED).unwrap();
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest, "the expected file was recorded from other cases");
    assert_eq!(expected.schema, 1);
    assert!(!CASES.contains(&b'\r'));

    let file_budget = budget();
    let file = parse_json(std::str::from_utf8(CASES).unwrap(), &file_budget).unwrap();
    let base = field(&file, "base", &file_budget).unwrap();
    let cases = field(&file, "cases", &file_budget).and_then(Value::array).unwrap();
    assert_eq!(cases.len(), expected.cases.len());

    let mut mismatches = Vec::new();
    let (mut compared, mut failures, mut memo) = (0, 0, 0);
    let mut unsupported = std::collections::BTreeMap::<String, usize>::new();
    let (mut tailored_compared, mut locale_declines) = (0, Vec::new());
    for (case, recorded) in cases.iter().zip(&expected.cases) {
        let budget = budget();
        let id = string(field(case, "id", &budget).and_then(|id| if let Value::Text(id) = id { Some(id) } else { None }).unwrap());
        assert_eq!(id, recorded.id);
        if recorded.result.ordered_observation.is_some() {
            assert!(field(case, "orderedObservation", &budget).is_some());
            continue; // The owned phase oracle below consumes these separately from legacy fallback replay.
        }
        let native = field(case, "native", &budget).map(|native| match native { Value::Text(text) => string(text), _ => panic!("native is text") });
        let random: Vec<f64> = field(case, "random", &budget).and_then(Value::array).unwrap_or(&[]).iter()
            .map(|value| if let Value::Number(value) = value { *value } else { panic!("tape values are numbers") }).collect();
        let database = parse_json(&database_text(base, case, &budget), &budget).unwrap();
        let result = &recorded.result;
        if matches!(field(&database, "useTokenizerCaching", &budget), Some(Value::Bool(true))) { memo += 1; }

        for tailored in [false, true] {
            let replayed = select(&database, random.clone(), JsPatternEngine::default(), tailored, &budget);
            let locale = if tailored { "tailored " } else { "" };
            let mut differ = |what: &str, detail: String| mismatches.push(format!("{id}: {locale}{what}: {detail}"));
            if tailored && declined(&replayed.result, "case-locale") {
                if !result.tailored_case { differ("case-locale", "declined although no tailored locale lowercases its text differently".into()); }
                if !replayed.tokenizer.is_empty() || !replayed.effects.is_empty() {
                    differ("case-locale", format!("declined after {} host requests and {} writes", replayed.tokenizer.len(), replayed.effects.len()));
                }
                locale_declines.push(id.clone());
                continue;
            }
            if let Some(native) = &native {
                let name = native.strip_prefix("unsupported-").expect("native names a decline");
                if !tailored { *unsupported.entry(name.to_string()).or_insert(0) += 1; }
                if !declined(&replayed.result, name) {
                    differ("native", format!("expected Unsupported({name}), got {:?}", replayed.result.as_ref().map(|output| output.fragments.len())));
                }
                if !replayed.tokenizer.is_empty() || !replayed.effects.is_empty() {
                    differ("native", format!("declined after {} host requests and {} writes", replayed.tokenizer.len(), replayed.effects.len()));
                }
                continue;
            }
            let threw = compare(&replayed, result, &database, &budget, &mut differ);
            if tailored {
                tailored_compared += 1;
            } else {
                compared += 1;
                if threw { failures += 1; }
            }
        }
    }
    println!("lorebook oracle: {compared} compared ({failures} errors, {memo} with the tokenizer memo), declined {unsupported:?}; \
        tailored locale: {tailored_compared} compared, {} declined case-locale; {} mismatches", locale_declines.len(), mismatches.len());
    assert!(compared >= 120, "only {compared} cases compared");
    assert!(memo >= 5, "only {memo} cases with the tokenizer memo");
    for witness in ["locale-key-capital-i", "locale-message-capital-i", "locale-message-dotted-i", "locale-lithuanian-grave",
        "locale-lithuanian-j-accent", "locale-username", "locale-recursive-content"] {
        assert!(locale_declines.iter().any(|id| id == witness), "{witness} was not declined under a tailored locale");
    }
    // Its `I` sits only in a message beyond the scan depth, which selection never lowercases.
    assert!(!locale_declines.iter().any(|id| id == "locale-unscanned-message"), "locale-unscanned-message was declined");
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}


/// A database from the oracle's base records and a case written as the generator writes one.
fn case_database(case: &str, budget: &Budget) -> Value {
    let file = parse_json(std::str::from_utf8(CASES).unwrap(), budget).unwrap();
    let case = parse_json(case, budget).unwrap();
    parse_json(&database_text(field(&file, "base", budget).unwrap(), &case, budget), budget).unwrap()
}

fn sources(result: &Result<LoreOutput, StageError>) -> Vec<String> {
    result.as_ref().unwrap().fragments.iter().map(|fragment| string(&fragment.source)).collect()
}

/// An engine that refuses every pattern with one error.
struct RefusingEngine(StageError);

impl PatternEngine for RefusingEngine {
    fn compile(&mut self, _source: &Text, _flags: &Text, _budget: &Budget) -> Result<Box<dyn Pattern + Send>, StageError> {
        Err(self.0.clone())
    }
}

#[test]
fn only_a_regex_syntax_error_fails_the_key_and_other_engine_errors_propagate() {
    let budget = budget();
    let database = case_database(r#"{"character":{"globalLore":[
        {"key":"/dra/","content":"Regex.","useRegex":true,"insertorder":100,"comment":"Regex"},
        {"key":"","content":"Always.","alwaysActive":true,"insertorder":100,"comment":"Always"}]},
        "chat":{"message":[{"role":"user","data":"dragon"}]}}"#, &budget);
    let run = |error: &str| -> Result<LoreOutput, StageError> {
        let error = match error.strip_prefix("unsupported:") {
            Some(name) => StageError::Unsupported(Cow::Owned(name.into())),
            None => StageError::Invalid(Cow::Owned(error.into())),
        };
        select(&database, Vec::new(), RefusingEngine(error), false, &budget).result
    };
    assert_eq!(sources(&run("regex-syntax")), ["Always"]);
    assert_eq!(run("unsupported:regex-lookbehind").unwrap_err(), StageError::Unsupported(Cow::Borrowed("regex-lookbehind")));
    assert_eq!(run("regex-state").unwrap_err(), StageError::Invalid(Cow::Borrowed("regex-state")));
}

#[test]
fn a_comment_that_is_not_a_string_declines_only_where_it_names_an_active_entry() {
    let budget = budget();
    let run = |lore: &str| {
        let database = case_database(&format!(r#"{{"character":{{"globalLore":[{lore}]}}}}"#), &budget);
        select(&database, Vec::new(), JsPatternEngine::default(), false, &budget).result
    };
    assert_eq!(run(r#"{"key":"","content":"A.","alwaysActive":true,"comment":5}"#).unwrap_err(),
        StageError::Unsupported(Cow::Borrowed("lore-comment")));
    assert_eq!(sources(&run(r#"{"key":"","content":"A.","comment":5},{"key":"","content":"B.","alwaysActive":true,"comment":"B"}"#)), ["B"]);
}

struct Driven { result: Result<LoreOutput, StageError>, requests: usize, effects: Vec<Effect> }

/// Selection over generated lore, the host answering its `n`th request with `reply(n)`.
fn drive(lore: &str, tailored: bool, patterns: impl PatternEngine, counter: impl TokenCounter, reply: impl FnMut(usize) -> HostReply) -> Driven {
    drive_case(&format!(r#"{{"character":{{"globalLore":[{lore}]}}}}"#), tailored, patterns, counter, reply)
}

fn drive_case(case: &str, tailored: bool, patterns: impl PatternEngine, counter: impl TokenCounter, mut reply: impl FnMut(usize) -> HostReply) -> Driven {
    let budget = budget();
    let database = case_database(case, &budget);
    let mut input = LoreInput::from_database(&database, 0, &budget).unwrap();
    input.tailored_case = tailored;
    let mut selection = LoreSelection::new(input, LiteralText, counter, patterns, Tape::new(Vec::new()));
    let selection: &mut dyn LoreSelector = &mut selection;
    let mut requests = 0;
    let mut step = selection.select(&budget);
    let result = loop {
        match step {
            Ok(Step::NeedsHost(_)) => {
                step = selection.resume(reply(requests), &budget);
                requests += 1;
            }
            Ok(Step::Done(output)) => break Ok(output),
            Err(error) => break Err(error),
        }
    };
    let effects = selection.take_effects();
    assert!(selection.take_effects().is_empty());
    Driven { result, requests, effects }
}

fn plugin() -> Counter { Counter::new(TokenizerId::Plugin, ChatAccounting::default()) }
fn two_tokens(_: usize) -> HostReply { HostReply::Tokens(vec![0.0; 2]) }
fn no_regex() -> RefusingEngine { RefusingEngine(StageError::Invalid(Cow::Borrowed("regex-syntax"))) }
fn variables(effects: &[Effect]) -> Vec<String> {
    effects.iter().map(|effect| match effect {
        Effect::ChatVariable { name, .. } => string(name),
        other => panic!("unexpected effect {other:?}"),
    }).collect()
}

/// Counted first through the host, then writes its keep variable.
const LEAD: &str = r#"{"key":"","content":"@@keep_activate_after_match\nLead.","alwaysActive":true,"insertorder":100,"comment":"Lead","id":"lead"}"#;
const SECOND: &str = r#"{"key":"","content":"@@keep_activate_after_match\nSecond.","alwaysActive":true,"insertorder":100,"comment":"Second","id":"second"}"#;

#[test]
fn every_decline_returns_before_the_first_host_request_and_write() {
    let lead = drive(LEAD, false, no_regex(), plugin(), two_tokens);
    assert_eq!(lead.requests, 1);
    assert_eq!(variables(&lead.result.unwrap().effects), ["__internal_ka_lead"]);
    for (name, entry, tailored) in [
        ("lore-insertorder", r#"{"key":"","content":"B.","alwaysActive":true,"insertorder":[1],"comment":"B"}"#, false),
        ("lore-content", r#"{"key":"","alwaysActive":true,"insertorder":100,"comment":"B"}"#, false),
        ("lore-key", r#"{"key":5,"content":"B.","insertorder":100,"comment":"B"}"#, false),
        ("lore-secondkey", r#"{"key":"b","secondkey":5,"selective":true,"content":"B.","insertorder":100,"comment":"B"}"#, false),
        ("lore-id", r#"{"key":"","content":"@@dont_activate_after_match\nB.","alwaysActive":true,"insertorder":100,"comment":"B","id":[1]}"#, false),
        ("lore-comment", r#"{"key":"","content":"B.","alwaysActive":true,"insertorder":100,"comment":5}"#, false),
        ("case-locale", r#"{"key":"Ice","content":"B.","insertorder":100,"comment":"B"}"#, true),
        ("case-locale", r#"{"key":"","content":"In the recursive scan.","alwaysActive":true,"insertorder":100,"comment":"B"}"#, true),
        ("cbs", r#"{"key":"","content":"{{user}}","alwaysActive":true,"insertorder":100,"comment":"B"}"#, false),
        ("regex-lookbehind", r#"{"key":"/b/","content":"B.","useRegex":true,"insertorder":100,"comment":"B"}"#, false),
    ] {
        let engine = RefusingEngine(StageError::Unsupported(Cow::Borrowed("regex-lookbehind")));
        let driven = drive(&format!("{LEAD},{entry}"), tailored, engine, plugin(), two_tokens);
        assert_eq!(driven.result.unwrap_err(), StageError::Unsupported(Cow::Owned(name.into())), "{entry}");
        assert_eq!((driven.requests, driven.effects.len()), (0, 0), "{entry}");
    }
    // Only the scanned message needs tailored lowercasing. The already planned lead still makes no host call or write.
    let case = format!(r#"{{"character":{{"globalLore":[{LEAD},{{"key":"dragon","content":"B.","insertorder":100,"comment":"B"}}]}},"chat":{{"message":[{{"role":"user","data":"I saw dragon"}}]}}}}"#);
    let message = drive_case(&case, true, no_regex(), plugin(), two_tokens);
    assert_eq!(message.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("case-locale")));
    assert_eq!((message.requests, message.effects.len()), (0, 0));
}

/// Counts the first text without the host and declines the next.
struct DecliningCounter { counts: usize }

impl TokenCounter for DecliningCounter {
    fn count(&mut self, _input: TokenInput<'_>, _budget: &Budget) -> Result<Step<u32>, StageError> {
        self.counts += 1;
        if self.counts == 1 { Ok(Step::Done(1)) } else { Err(StageError::Unsupported(Cow::Borrowed("tokenizer"))) }
    }
    fn resume(&mut self, _reply: HostReply, _budget: &Budget) -> Result<Step<u32>, StageError> {
        Err(StageError::Invalid(Cow::Borrowed("token-counter-resume")))
    }
}

/// Counts every text through an HTTP request, as the Google Cloud tokenizer does: two tokens per reply.
struct HttpCounter;

impl TokenCounter for HttpCounter {
    fn count(&mut self, _input: TokenInput<'_>, _budget: &Budget) -> Result<Step<u32>, StageError> {
        Ok(Step::NeedsHost(HostRequest::Http(HttpRequestSpec { method: HttpMethod::Post, url: String::new(), headers: Vec::new(),
            body: Vec::new(), response: ResponseKind::Buffered })))
    }
    fn resume(&mut self, reply: HostReply, _budget: &Budget) -> Result<Step<u32>, StageError> {
        match reply {
            HostReply::Unsupported(name) => Err(StageError::Unsupported(name)),
            _ => Ok(Step::Done(2)),
        }
    }
}

#[test]
fn a_throw_follows_the_counts_before_it_and_a_decline_after_a_committing_request_fails() {
    let budget = budget();
    // TS counts the lead and writes its keep variable, then throws on `@@position` without an argument.
    let thrown = drive(&format!(r#"{LEAD},{{"key":"","content":"@@position\nB.","alwaysActive":true,"insertorder":100}}"#),
        false, no_regex(), plugin(), two_tokens);
    assert_eq!(thrown.result.unwrap_err(), StageError::Invalid(Cow::Borrowed("lore-position-argument")));
    assert_eq!((thrown.requests, variables(&thrown.effects)), (1, vec!["__internal_ka_lead".to_string()]));
    let ordered = drive(&format!(r#"{LEAD},{SECOND},{{"key":"","content":"@@position\nB.","alwaysActive":true,"insertorder":100}}"#),
        false, no_regex(), plugin(), two_tokens);
    assert_eq!(ordered.result.unwrap_err(), StageError::Invalid(Cow::Borrowed("lore-position-argument")));
    assert_eq!((ordered.requests, variables(&ordered.effects)),
        (2, vec!["__internal_ka_lead".to_string(), "__internal_ka_second".to_string()]));
    // A failed count fails selection as it fails the counter, keeping the lead's write, made after its count, and not
    // the second entry's.
    let failure = || HostReply::Failed { message: Text::from_str("Tokenizer failed.", &budget).unwrap() };
    let counter_failure = {
        let mut counter = plugin();
        let text = Text::from_str("Second.", &budget).unwrap();
        assert!(matches!(counter.count(TokenInput::Text(&text), &budget), Ok(Step::NeedsHost(_))));
        counter.resume(failure(), &budget).unwrap_err()
    };
    assert!(!matches!(counter_failure, StageError::Unsupported(_)));
    let failed = drive(&format!("{LEAD},{SECOND}"), false, no_regex(), plugin(), |n| match n {
        0 => two_tokens(n),
        _ => failure(),
    });
    assert_eq!(failed.result.unwrap_err(), counter_failure);
    assert_eq!((failed.requests, variables(&failed.effects)), (2, vec!["__internal_ka_lead".to_string()]));
    // The host refusing a plugin tokenizer count leaves the send to TS, which repeats nothing visible by counting
    // again; refusing a request after one TS could not repeat, such as a Google Cloud `countTokens` call, does not.
    for refused_at in [0, 1] {
        let refuse = |n| if n == refused_at { HostReply::Unsupported(Cow::Borrowed("host-tokenizer")) } else { two_tokens(n) };
        let late = drive(&format!("{LEAD},{SECOND}"), false, no_regex(), plugin(), refuse);
        assert_eq!(late.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("host-tokenizer")));
        assert_eq!((late.requests, late.effects.len()), (refused_at + 1, 0));
        let committed = drive(&format!("{LEAD},{SECOND}"), false, no_regex(), HttpCounter, refuse);
        assert_eq!(committed.result.unwrap_err(), StageError::Invalid(Cow::Borrowed("host-tokenizer")));
        assert_eq!(committed.requests, refused_at + 1);
        let expected = if refused_at == 0 { Vec::<String>::new() } else { vec!["__internal_ka_lead".to_string()] };
        assert_eq!(variables(&committed.effects), expected);
    }
    // A counter that declines before any request still declines, and returns no writes.
    let early = drive(&format!("{LEAD},{SECOND}"), false, no_regex(), DecliningCounter { counts: 0 }, two_tokens);
    assert_eq!(early.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("tokenizer")));
    assert_eq!((early.requests, early.effects.len()), (0, 0));

    // A speculative later throw is not the terminal site while an earlier count remains pending.
    for counted in [false, true] {
        let database = case_database(&format!(r#"{{"character":{{"globalLore":[{LEAD},
            {{"key":"","content":"@@position\nB.","alwaysActive":true,"insertorder":100}}]}}}}"#), &budget);
        let input = LoreInput::from_database(&database, 0, &budget).unwrap();
        let mut selection = LoreSelection::new(input, processor(&[]), plugin(), JsPatternEngine::default(), Tape::new(Vec::new()));
        assert_eq!(requested_text(selection.select(&budget).unwrap()), "Lead.");
        assert_eq!(selection.failure_site(), None);
        let reply = if counted { two_tokens(0) } else { failure() };
        assert!(selection.resume(reply, &budget).is_err());
        assert_eq!(selection.failure_site(), Some(LoreFailureSite {
            entry: Some(usize::from(counted)), phase: if counted { LorePhase::Evaluate } else { LorePhase::Count },
        }));
    }
}

type OwnedSelection = LoreSelection<Processor<Tape>, Counter, JsPatternEngine, Tape>;

fn processor(draws: &[f64]) -> Processor<Tape> {
    Processor::new(Snapshot { case_tables: CbsCaseTables::Native, ..Snapshot::default() }, Tape::new(draws.to_vec()))
}

fn owned_case(case: &str, processor: Processor<Tape>, tailored: bool, budget: &Budget) -> OwnedSelection {
    let database = case_database(case, budget);
    let mut input = LoreInput::from_database(&database, 0, budget).unwrap();
    input.tailored_case = tailored;
    LoreSelection::with_policy(input, processor, plugin(), JsPatternEngine::default(), Tape::new(Vec::new()), LorePolicy::Owned)
}

fn requested_text(step: Step<LoreOutput>) -> String {
    match step {
        Step::NeedsHost(HostRequest::PluginTokenizer { text }) => string(&text),
        other => panic!("expected plugin count, got {other:?}"),
    }
}

fn assert_variable_prefix(effects: &[Effect], names: &[&str]) {
    assert_eq!(variables(effects), names);
    assert!(effects.iter().all(|effect| matches!(effect,
        Effect::ChatVariable { value: Value::Text(value), .. } if value.equals_ascii(b"true"))));
}

#[test]
fn owned_current_count_and_writes_precede_recursive_refusal() {
    let case = format!(r#"{{"character":{{"globalLore":[{LEAD},
        {{"key":"","content":"@@keep_activate_after_match\n@@dont_activate_after_match\nI current.",
        "alwaysActive":true,"insertorder":100,"comment":"Current","id":"current"}}]}}}}"#);
    for reply_kind in ["counted", "failed", "unsupported"] {
        let budget = budget();
        let mut selection = owned_case(&case, processor(&[]), true, &budget);
        assert_eq!(requested_text(selection.select(&budget).unwrap()), "Lead.");
        assert_eq!(requested_text(selection.resume(two_tokens(0), &budget).unwrap()), "I current.");
        assert_eq!(selection.failure_site(), None);
        let (reply, error, phase, names): (_, _, _, &[&str]) = match reply_kind {
            "counted" => (two_tokens(1), StageError::Unsupported("case-locale".into()), LorePhase::Recursive,
                &["__internal_ka_lead", "__internal_ka_current", "__internal_da_current"]),
            "failed" => (HostReply::Failed { message: Text::from_str("ordered count rejected", &budget).unwrap() },
                StageError::Host(units("ordered count rejected")), LorePhase::Count, &["__internal_ka_lead"]),
            _ => (HostReply::Unsupported("owned-count-refusal".into()), StageError::Unsupported("owned-count-refusal".into()),
                LorePhase::Count, &["__internal_ka_lead"]),
        };
        assert_eq!(selection.resume(reply, &budget).unwrap_err(), error, "{reply_kind}");
        let site = Some(LoreFailureSite { entry: Some(1), phase });
        assert_eq!(selection.failure_site(), site);
        assert_variable_prefix(&selection.take_effects(), names);
        assert!(selection.take_effects().is_empty());
        assert!(matches!(selection.select(&budget), Err(StageError::Invalid(name)) if name == "select"));
        assert!(matches!(selection.resume(two_tokens(2), &budget), Err(StageError::Invalid(name)) if name == "resume"));
        assert_eq!(selection.failure_site(), site);
        assert!(selection.take_effects().is_empty());
    }
}

#[test]
fn owned_resume_keeps_evaluated_text_and_live_variables() {
    let budget = budget();
    let case = r#"{"character":{"loreSettings":{"recursiveScanning":false},"globalLore":[
        {"key":"","content":"@@keep_activate_after_match\n@@dont_activate_after_match\n{{random::first::second}}",
            "alwaysActive":true,"insertorder":100,"comment":"Current","id":"current"},
        {"key":"","content":"{{getvar::__internal_ka_current}}/{{getvar::__internal_da_current}}",
            "alwaysActive":true,"insertorder":100,"comment":"Later","id":"later"}]}}"#;
    // Only one CBS draw is available. Re-evaluating the suspended entry would exhaust it.
    let mut selection = owned_case(case, processor(&[0.0]), false, &budget);
    assert_eq!(requested_text(selection.select(&budget).unwrap()), "first");
    assert!(selection.take_effects().is_empty());
    assert_eq!(requested_text(selection.resume(two_tokens(0), &budget).unwrap()), "true/true");
    let Step::Done(output) = selection.resume(HostReply::Tokens(vec![0.0; 3]), &budget).unwrap() else { panic!("unfinished count") };
    assert_variable_prefix(&output.effects, &["__internal_ka_current", "__internal_da_current"]);
    assert_eq!(output.fragments.iter().map(|fragment| (string(&fragment.source), fragment.tokens)).collect::<Vec<_>>(),
        [("Later".to_string(), 3), ("Current".to_string(), 2)]);
    assert_eq!(selection.failure_site(), None);
    assert!(selection.take_effects().is_empty());
    assert!(selection.take_effects().is_empty());
}

#[test]
fn owned_variable_prefix_survives_real_cbs_propagation_and_next_write_limits() {
    let case = r#"{"character":{"loreSettings":{"recursiveScanning":false},"globalLore":[
        {"key":"","content":"@@keep_activate_after_match\n@@dont_activate_after_match\npayload",
            "alwaysActive":true,"insertorder":100,"comment":"Current","id":"current"}]}}"#;
    let mut witnessed = [false; 3];
    // Bound the resumed call, without copying the implementation's allocation/work totals into the test.
    for work in 1..=256 {
        let budget = budget();
        let mut selection = owned_case(case, processor(&[]), false, &budget);
        assert_eq!(requested_text(selection.select(&budget).unwrap()), "payload");
        let limited = Budget::new(Limits { max_work: work, ..*budget.limits() }, Arc::new(AtomicBool::new(false))).unwrap();
        let result = selection.resume(two_tokens(0), &limited);
        let effects = selection.take_effects();
        if matches!(result, Err(StageError::Limit(ref name)) if name == "work") {
            let phase = selection.failure_site().unwrap().phase;
            let names = variables(&effects);
            match (phase, names.as_slice()) {
                (LorePhase::Keep, [keep]) if keep == "__internal_ka_current" => witnessed[0] = true,
                (LorePhase::Dont, [keep]) if keep == "__internal_ka_current" => witnessed[1] = true,
                (LorePhase::Dont, [keep, dont]) if keep == "__internal_ka_current" && dont == "__internal_da_current" => witnessed[2] = true,
                _ => {},
            }
        }
        assert!(selection.take_effects().is_empty());
        if witnessed.iter().all(|seen| *seen) { break; }
    }
    assert_eq!(witnessed, [true; 3], "missing completed Keep, pre-Dont or completed Dont failure prefix");
}

#[test]
fn owned_case_guards_follow_consumed_keys_and_history() {
    let cases = [
        r#"{"character":{"globalLore":[{"key":"\u1acf","content":"Key.","insertorder":100}]}}"#,
        r#"{"character":{"globalLore":[{"key":"dragon","secondkey":"\u1acf","selective":true,"content":"Secondary.","insertorder":100}]}}"#,
        r#"{"character":{"globalLore":[{"key":"","content":"@@additional_keys \u1acf\nDirective.","alwaysActive":true,"insertorder":100}]}}"#,
        r#"{"character":{"globalLore":[{"key":"dragon","content":"History.","insertorder":100}]},"chat":{"message":[{"role":"user","data":"dragon\u1acf"}]}}"#,
    ];
    for case in cases {
        let budget = budget();
        let mut selection = owned_case(case, processor(&[]), false, &budget).with_case_tables(CaseTables::Unicode16);
        assert_eq!(selection.select(&budget).unwrap_err(), StageError::Unsupported("case-table".into()));
        assert_eq!(selection.failure_site(), Some(LoreFailureSite { entry: Some(0), phase: LorePhase::Evaluate }));
        assert!(selection.take_effects().is_empty());
    }
    // An older message outside scanDepth is not normalized.
    let budget = budget();
    let case = r#"{"character":{"loreSettings":{"scanDepth":1,"recursiveScanning":false},"globalLore":[
        {"key":"dragon","content":"Matched.","insertorder":100}]},"chat":{"message":[
        {"role":"user","data":"\u1acf"},{"role":"user","data":"dragon"}]}}"#;
    let mut selection = owned_case(case, processor(&[]), false, &budget).with_case_tables(CaseTables::Unicode16);
    assert_eq!(requested_text(selection.select(&budget).unwrap()), "Matched.");
    assert!(matches!(selection.resume(two_tokens(0), &budget), Ok(Step::Done(_))));
}

#[test]
fn owned_recursive_case_guard_reads_raw_content_not_the_counted_cbs_result() {
    let budget = budget();
    let case = r#"{"character":{"globalLore":[{"key":"","content":"@@keep_activate_after_match\n{{user}}",
        "alwaysActive":true,"insertorder":100,"comment":"Current","id":"current"}]}}"#;
    let text = Processor::new(Snapshot { user: units("\u{1acf}"), case_tables: CbsCaseTables::Native, ..Snapshot::default() }, Tape::new(Vec::new()));
    let mut selection = owned_case(case, text, false, &budget).with_case_tables(CaseTables::Unicode16);
    assert_eq!(requested_text(selection.select(&budget).unwrap()), "\u{1acf}");
    let Step::Done(output) = selection.resume(two_tokens(0), &budget).unwrap() else { panic!("unfinished selection") };
    assert_variable_prefix(&output.effects, &["__internal_ka_current"]);
    assert_eq!(string(&output.fragments[0].prompt), "{{user}}");

    let case = r#"{"character":{"globalLore":[{"key":"","content":"@@keep_activate_after_match\n\u1acf",
        "alwaysActive":true,"insertorder":100,"comment":"Current","id":"current"}]}}"#;
    let mut selection = owned_case(case, processor(&[]), false, &budget).with_case_tables(CaseTables::Unicode16);
    assert_eq!(requested_text(selection.select(&budget).unwrap()), "\u{1acf}");
    assert_eq!(selection.resume(two_tokens(0), &budget).unwrap_err(), StageError::Unsupported("case-table".into()));
    assert_eq!(selection.failure_site(), Some(LoreFailureSite { entry: Some(0), phase: LorePhase::Recursive }));
    assert_variable_prefix(&selection.take_effects(), &["__internal_ka_current"]);
    assert!(selection.take_effects().is_empty());
}

struct ObservedCount<'a> {
    text: &'a str,
    result: Result<usize, &'a Thrown>,
    before: Vec<(String, String)>,
}

struct OrderedTrace<'a> {
    counts: Vec<ObservedCount<'a>>,
    writes: Vec<(String, String)>,
    recursive_prefix: Option<Vec<(String, String)>>,
}

fn ordered_trace<'a>(observation: &'a OrderedObservation, mode: &str, target: &str) -> OrderedTrace<'a> {
    let mut counts = Vec::new();
    let mut writes = Vec::new();
    let mut pending_count = None;
    let mut pending_write = None;
    let mut lowered = Vec::new();
    let mut recursive_prefix = None;
    let (mut held, mut released) = (false, false);
    for event in &observation.events {
        match event {
            OrderedEvent::CountEnter { text } => {
                assert!(pending_count.is_none() && pending_write.is_none());
                pending_count = Some((text.as_str(), writes.clone()));
            }
            OrderedEvent::CountComplete { text, tokens } => {
                let (input, before) = pending_count.take().expect("count completion has an entry");
                assert_eq!(input, text);
                counts.push(ObservedCount { text: input, result: Ok(*tokens), before });
            }
            OrderedEvent::CountError { text, error } => {
                let (input, before) = pending_count.take().expect("count failure has an entry");
                assert_eq!(input, text);
                assert_eq!(mode, "count-reject");
                assert_eq!((error.name.as_str(), error.message.as_str()), ("Error", "ordered-current-tokenizer-failure"));
                counts.push(ObservedCount { text: input, result: Err(error), before });
            }
            OrderedEvent::SetterEnter { name, value } => {
                assert!(pending_count.is_none() && pending_write.is_none());
                pending_write = Some((name, value));
            }
            OrderedEvent::SetterComplete { name, value } => {
                assert_eq!(pending_write.take(), Some((name, value)));
                writes.push((name.clone(), value.clone()));
            }
            OrderedEvent::SetterError { name, error } => panic!("unexpected setter failure {name}: {error:?}"),
            OrderedEvent::LocaleLower { input, output } => {
                assert!(pending_count.is_none() && pending_write.is_none());
                assert_eq!(output, &input.to_lowercase());
                if input == target && recursive_prefix.is_none() { recursive_prefix = Some(writes.clone()); }
                lowered.push(input.as_str());
            }
            OrderedEvent::GateHeld => {
                assert!(!held && !released);
                assert_eq!(pending_count.as_ref().map(|(text, _)| *text), Some(target));
                let state: std::collections::BTreeMap<_, _> = writes.iter().map(|(name, value)| (format!("${name}"), value.clone())).collect();
                assert_eq!(observation.held_scriptstate.as_ref(), Some(&state));
                held = true;
            }
            OrderedEvent::GateRelease => { assert!(held && !released); released = true; }
        }
    }
    assert!(pending_count.is_none() && pending_write.is_none());
    assert_eq!((held, released), (mode == "count-defer", mode == "count-defer"));
    assert_eq!(observation.held_scriptstate.is_some(), mode == "count-defer");
    let mut normalization = vec!["lead-one.", "lead-one.", "lead-two.", "lead-two."];
    if mode != "count-reject" { normalization.extend([target, target]); }
    assert_eq!(lowered, normalization, "preserve both actual normalization calls at each recursive site");
    assert_eq!(counts.len(), 3);
    OrderedTrace { counts, writes, recursive_prefix }
}

fn ordered_writes(effects: &[Effect]) -> Vec<(String, String)> {
    effects.iter().map(|effect| match effect {
        Effect::ChatVariable { name, value: Value::Text(value) } => (string(name), string(value)),
        effect => panic!("unexpected ordered effect {effect:?}"),
    }).collect()
}

#[test]
fn owned_selection_matches_real_typescript_phase_observations() {
    let expected: ExpectedFile = serde_json::from_slice(EXPECTED).unwrap();
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest);
    let budget = budget();
    let file = parse_json(std::str::from_utf8(CASES).unwrap(), &budget).unwrap();
    let base = field(&file, "base", &budget).unwrap();
    let cases = field(&file, "cases", &budget).and_then(Value::array).unwrap();
    assert_eq!(cases.len(), expected.cases.len());
    let mut observed = Vec::new();
    for (case, recorded) in cases.iter().zip(&expected.cases) {
        let Some(observation) = &recorded.result.ordered_observation else { continue };
        let id = field(case, "id", &budget).unwrap().to_text(&budget).unwrap();
        assert_eq!(string(&id), recorded.id);
        observed.push(recorded.id.as_str());
        let config = field(case, "orderedObservation", &budget).unwrap();
        let mode = field(config, "mode", &budget).unwrap().to_text(&budget).unwrap();
        let mode = mode.as_unicode().unwrap();
        let target = field(config, "targetText", &budget).unwrap().to_text(&budget).unwrap();
        let target = target.as_unicode().unwrap();
        let trace = ordered_trace(observation, mode, target);
        let database = parse_json(&database_text(base, case, &budget), &budget).unwrap();
        for tailored in [false, true] {
            let mut input = LoreInput::from_database(&database, 0, &budget).unwrap();
            input.tailored_case = tailored;
            let mut selection = LoreSelection::with_policy(input, processor(&[]), plugin(), JsPatternEngine::default(),
                Tape::new(Vec::new()), LorePolicy::Owned);
            let mut step = selection.select(&budget);
            let mut writes = Vec::new();
            for counted in &trace.counts {
                assert_eq!(requested_text(step.unwrap()), counted.text, "{}", recorded.id);
                writes.extend(ordered_writes(&selection.take_effects()));
                assert_eq!(writes, counted.before, "completed prefix before {}", counted.text);
                assert_eq!(selection.failure_site(), None);
                let reply = match counted.result {
                    Ok(tokens) => HostReply::Tokens(vec![0.0; tokens]),
                    Err(error) => HostReply::Failed { message: Text::from_str(&error.message, &budget).unwrap() },
                };
                step = selection.resume(reply, &budget);
            }
            if mode == "count-reject" {
                assert_eq!(step.unwrap_err(), StageError::Host(units("ordered-current-tokenizer-failure")));
                assert_eq!(selection.failure_site(), Some(LoreFailureSite { entry: Some(2), phase: LorePhase::Count }));
                writes.extend(ordered_writes(&selection.take_effects()));
                assert_eq!(writes, trace.writes);
                assert!(trace.recursive_prefix.is_none());
                let error = recorded.result.error.as_ref().expect("real TS count threw");
                assert_eq!((error.name.as_str(), error.message.as_str()), ("Error", "ordered-current-tokenizer-failure"));
            } else if tailored {
                assert_eq!(step.unwrap_err(), StageError::Unsupported("case-locale".into()));
                assert_eq!(selection.failure_site(), Some(LoreFailureSite { entry: Some(2), phase: LorePhase::Recursive }));
                writes.extend(ordered_writes(&selection.take_effects()));
                assert_eq!(Some(&writes), trace.recursive_prefix.as_ref());
                assert!(recorded.result.error.is_none(), "the native-only guard is not a TS exception");
            } else {
                let Step::Done(output) = step.unwrap() else { panic!("unfinished selection") };
                writes.extend(ordered_writes(&output.effects));
                assert_eq!(writes, trace.writes);
                assert_eq!(Some(actives(&output, &budget)), recorded.result.actives);
                assert_eq!(selection.failure_site(), None);
                assert!(recorded.result.error.is_none());
            }
            assert!(selection.take_effects().is_empty());
        }
    }
    assert_eq!(observed, ["ordered-current-keep-dont-recursive", "ordered-current-count-reject", "ordered-current-count-defer"],
        "fresh real TS observations are required for all three owned cases");
}

// Far above the linear cost of each shape below and far below the quadratic one.
const WORK_LIMIT: usize = 1_000_000;

/// Selection over generated lore under a work limit, with a token budget every entry fits.
fn under_work_limit(lore: &[String], work: usize) -> Result<LoreOutput, StageError> {
    let budget = budget();
    let case = format!(r#"{{"root":{{"loreBookToken":1000000000}},"character":{{"globalLore":[{}]}}}}"#, lore.join(","));
    let database = case_database(&case, &budget);
    let limited = Budget::new(Limits { max_work: work, ..*budget.limits() }, Arc::new(AtomicBool::new(false))).unwrap();
    select(&database, Vec::new(), JsPatternEngine::default(), false, &limited).result
}

#[test]
fn copies_of_a_growing_target_and_parent_scans_are_charged_as_work() {
    // 600 entries injected into one 5,000-unit target: appending grows it in place, prepending copies it each time.
    let injected = |operation: &str| -> Vec<String> {
        let mut lore = vec![format!(r#"{{"key":"","content":"{}","alwaysActive":true,"insertorder":100,"comment":"T"}}"#, "x".repeat(5000))];
        for index in 0..600 {
            lore.push(format!(r#"{{"key":"","content":"@@inject_lore T\n{operation}P{index}","alwaysActive":true,"insertorder":100,"comment":"I{index}"}}"#));
        }
        lore
    };
    assert_eq!(sources(&under_work_limit(&injected(""), WORK_LIMIT)), ["T"]);
    assert_eq!(under_work_limit(&injected(r"@@inject_prepend x\n"), WORK_LIMIT).unwrap_err(), StageError::Limit(Cow::Borrowed("work")));
    // 1,500 child entries without a parent each compare their id with every entry before them.
    let entries = |mode: &str| -> Vec<String> {
        (0..1500).map(|index| format!(r#"{{"key":"zzz","content":"C.","mode":"{mode}","id":"id-{index:04}","insertorder":100}}"#)).collect()
    };
    assert!(under_work_limit(&entries("normal"), WORK_LIMIT).unwrap().fragments.is_empty());
    assert_eq!(under_work_limit(&entries("child"), WORK_LIMIT).unwrap_err(), StageError::Limit(Cow::Borrowed("work")));
}
