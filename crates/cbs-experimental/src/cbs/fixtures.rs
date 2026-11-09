// Replays the fixtures that `benchmarks/rust-cbs/run.mjs generate` records from the product
// risuChatParser, and checks the registration table against the CBS ledger.
use super::families::{self, Status, ARRAY_PROTO, BOOLEAN_PROTO, NUMBER_PROTO, OBJECT_PROTO, STRING_PROTO};
use super::parser::{self, ArgState, CaseTables, Options};
use super::local_time::local_to_utc;
use super::processor::{
    text_units, CallArgs, Character, Chat, Database, HistoryAccess, HistorySearch, LocalTime, LocalePart, LocalePattern, Module,
    Platform, Processor, Reads, Snapshot, Source, TimeZone,
};
use super::{json_parse, json_string, replace_literal, to_string, units, Budget, Context, Json, Units};
use risunest_ir::chat::MessageRole;
use risunest_ir::clock::Fixed;
use risunest_ir::rng::Tape;
use risunest_ir::stage::{CbsConditions, Effect, MessageEdit, ScopedTextProcessor, StageError, TextContext,
    TextProcessor, TextScope, TextVariables};
use risunest_ir::{Limits, OwnedVec, Text, Value};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

const GRAMMAR: &[u8] = include_bytes!("../../../../benchmarks/rust-cbs/fixtures/grammar.json");
const FAMILIES: &[u8] = include_bytes!("../../../../benchmarks/rust-cbs/fixtures/families.json");
const HOST: &[u8] = include_bytes!("../../../../benchmarks/rust-cbs/fixtures/host.json");
const LEDGER: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/cbs/ledger.json");
const CASE_TABLE: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/cbs/case-table.expected.json");
const CASE_TABLE_WEBVIEW: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/cbs/case-table-webview.expected.json");

fn document(bytes: &[u8]) -> Json {
    json_parse(&units(std::str::from_utf8(bytes).expect("UTF-8 fixture"))).expect("fixture JSON")
}
fn field<'a>(value: &'a Json, key: &str) -> Option<&'a Json> {
    let key = units(key);
    match value {
        Json::Obj(fields) => fields.iter().rev().find(|(k, _)| *k == key).map(|(_, v)| v),
        _ => None,
    }
}
fn text(value: &Json) -> &Units {
    match value {
        Json::Str(s) => s,
        _ => panic!("expected a string"),
    }
}
fn number(value: &Json) -> f64 {
    match value {
        Json::Num(n) => *n,
        _ => panic!("expected a number"),
    }
}
fn list(value: &Json) -> &[Json] {
    match value {
        Json::Arr(a) => a,
        _ => panic!("expected an array"),
    }
}
fn strings(value: Option<&Json>) -> HashMap<Units, Units> {
    match value {
        Some(Json::Obj(fields)) => fields.iter().map(|(k, v)| (k.clone(), text(v).clone())).collect(),
        _ => HashMap::new(),
    }
}
fn shown(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.map_or_else(|e| format!("\\u{{{:x}}}", e.unpaired_surrogate()), |c| c.escape_debug().to_string()))
        .collect()
}

// SHA-256 over the UTF-16LE bytes of the output, as the generator hashes table cases.
fn sha256(units: &[u16]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let bits = (bytes.len() as u64) * 8;
    bytes.push(0x80);
    while bytes.len() % 64 != 56 {
        bytes.push(0);
    }
    bytes.extend_from_slice(&bits.to_be_bytes());
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    for block in bytes.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

struct Invocation {
    variables: HashMap<Units, Units>,
    globals: HashMap<Units, Units>,
    user: Units,
    index: i64,
    seed: u32,
    functions: HashMap<Units, Units>,
}
#[derive(Debug, PartialEq)]
enum Outcome {
    Text { output: Units, draws: usize, state: u32 },
    Hash { sha256: String, units: usize, draws: usize, state: u32 },
    Steps(Vec<Outcome>),
    Refused(String),
}

fn case_text(case: &Json) -> Units {
    let Some(expand) = field(case, "expand") else { return text(field(case, "text").unwrap()).clone() };
    let op = text(field(expand, "op").unwrap());
    let (from, to) = (number(field(expand, "from").unwrap()) as u32, number(field(expand, "to").unwrap()) as u32);
    let mut out = units("{{");
    out.extend(op);
    out.extend(units("::"));
    for code in (from..=to).filter(|c| !(0xd800..=0xdfff).contains(c)) {
        let mut buf = [0u16; 2];
        out.extend_from_slice(char::from_u32(code).unwrap().encode_utf16(&mut buf));
    }
    out.extend(units("}}"));
    out
}

// The engine raw or folded, or the TextProcessor adapter with the same snapshot and Math.random sequence.
#[derive(Clone, Copy, Debug)]
enum Lane {
    Raw,
    Folded,
    Processor,
}

// The verification LCG the oracle installs as Math.random, as an injected random source.
struct Lcg(u32);
impl risunest_ir::rng::Rng for Lcg {
    fn random(&mut self) -> Result<f64, risunest_ir::Error> {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        Ok(f64::from(self.0) / 4_294_967_296.0)
    }
}

fn stage_budget() -> risunest_ir::Budget {
    let limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 20, max_text_units: 4_194_304, max_values: 1 << 16, max_depth: 64, max_work: 1 << 32 };
    risunest_ir::Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
}
// The engine reason a stage error stands for, so adapter refusals compare with the recorded ones.
fn reason(error: StageError) -> String {
    match error {
        StageError::Unsupported(reason) => reason.into_owned(),
        StageError::Limit(name) if name == "work" => "budget".into(),
        StageError::Limit(name) if name == "text-units" => "output-limit".into(),
        StageError::Limit(name) | StageError::Invalid(name) => name.into_owned(),
        StageError::Cancelled => "cancelled".into(),
        other => format!("{other:?}"),
    }
}

fn run(invocation: &Invocation, case: &Json, lane: Lane) -> Outcome {
    let mut variables = invocation.variables.clone();
    variables.extend(strings(field(case, "patch")));
    let seed = field(case, "seed").map_or(invocation.seed, |s| number(s) as u32);
    // The fixtures record a Chromium whose case tables are Rust's.
    let options = Options { displaying: matches!(field(case, "visualize"), Some(Json::Bool(true))), case_tables: CaseTables::Native,
        ..Options::default() };
    let hashed = matches!(field(case, "hash"), Some(Json::Bool(true)));
    let once = |source: &[u16], state: u32| -> Result<(Units, usize, u32), String> {
        if let Lane::Processor = lane {
            let budget = stage_budget();
            let reads = Reads { chat_variables: variables.clone(), global_variables: invocation.globals.clone(), ..Reads::default() };
            let snapshot = Snapshot {
                user: invocation.user.clone(),
                source: Source::Reads(reads),
                arg: ArgState { call_stack: 0, functions: Some(invocation.functions.clone()) },
                case_tables: options.case_tables,
                ..Snapshot::default()
            };
            let mut processor = Processor::new(snapshot, Lcg(state));
            let context = TextContext { chat_index: invocation.index, visualize: options.displaying, ..TextContext::default() };
            let input = Text::from_units(source, &budget).map_err(|e| format!("{e:?}"))?;
            let output = processor.process(&input, &context, &budget).map_err(reason)?;
            return Ok((text_units(&output.text), processor.draws(), processor.rng().0));
        }
        let mut context = Context {
            variables: variables.clone(),
            globals: invocation.globals.clone(),
            user: invocation.user.clone(),
            index: invocation.index,
            state,
            ..Default::default()
        };
        let prepared = parser::prepare(source, matches!(lane, Lane::Folded), options);
        let output = parser::run(&prepared, &mut context, &invocation.functions, &mut Budget::default())?;
        Ok((output, context.draws, context.state))
    };
    let shape = |(output, draws, state): (Units, usize, u32)| {
        if hashed {
            Outcome::Hash { sha256: sha256(&output), units: output.len(), draws, state }
        } else {
            Outcome::Text { output, draws, state }
        }
    };
    let Some(steps) = field(case, "steps") else {
        return once(&case_text(case), seed).map_or_else(Outcome::Refused, shape);
    };
    let mut source = text(field(case, "text").unwrap()).clone();
    let mut state = seed;
    let mut results = Vec::new();
    for step in list(steps) {
        let step = list(step);
        source = replace_literal(&source, text(&step[0]), text(&step[1]));
        match once(&source, state) {
            Ok(result) => {
                source = result.0.clone();
                state = result.2;
                results.push(shape(result));
            }
            Err(reason) => return Outcome::Refused(reason),
        }
    }
    Outcome::Steps(results)
}

fn expected(case: &Json) -> Option<Outcome> {
    let e = field(case, "expected").unwrap();
    let one = |e: &Json| {
        let draws = number(field(e, "draws").unwrap()) as usize;
        let state = number(field(e, "rngState").unwrap()) as u32;
        match field(e, "outputSha256") {
            Some(hash) => Outcome::Hash {
                sha256: String::from_utf16(text(hash)).unwrap(),
                units: number(field(e, "outputUnits").unwrap()) as usize,
                draws,
                state,
            },
            None => Outcome::Text { output: text(field(e, "output").unwrap()).clone(), draws, state },
        }
    };
    if field(e, "error").is_some() {
        return None;
    }
    Some(match field(e, "steps") {
        Some(steps) => Outcome::Steps(list(steps).iter().map(one).collect()),
        None => one(e),
    })
}

#[derive(Debug, Default, PartialEq)]
struct Counts {
    equal: usize,
    refused: usize,
    exceptions: usize,
    mismatch: usize,
}

fn replay(bytes: &[u8], lanes: &[Lane]) -> Counts {
    let file = document(bytes);
    let context = field(&file, "context").unwrap();
    let invocation = Invocation {
        variables: strings(field(context, "variables")),
        globals: strings(field(context, "globals")),
        user: text(field(context, "user").unwrap()).clone(),
        index: number(field(context, "index").unwrap()) as i64,
        seed: number(field(context, "seed").unwrap()) as u32,
        functions: strings(field(context, "functions")),
    };
    let mut counts = Counts::default();
    let mut failures = Vec::new();
    for case in list(field(&file, "cases").unwrap()) {
        let id = String::from_utf16_lossy(text(field(case, "id").unwrap()));
        let refusal = field(case, "refusal").map(|r| String::from_utf16(text(r)).unwrap());
        let wanted = expected(case);
        for &lane in lanes {
            let actual = run(&invocation, case, lane);
            let ok = match (&refusal, &wanted, &actual) {
                (Some(reason), _, Outcome::Refused(got)) => got == reason,
                (Some(_), _, _) => false,
                // The product threw, so the engine must hand the text back.
                (None, None, Outcome::Refused(_)) => true,
                (None, Some(wanted), actual) => wanted == actual,
                (None, None, _) => false,
            };
            match (ok, &refusal, &wanted) {
                (false, _, _) => {
                    counts.mismatch += 1;
                    if failures.len() < 24 {
                        let describe = |o: &Outcome| match o {
                            Outcome::Text { output, draws, state } => format!("{:?} draws {draws} state {state}", shown(output)),
                            other => format!("{other:?}"),
                        };
                        let wanted = refusal.clone().map(|r| format!("refusal {r}")).or(wanted.as_ref().map(describe));
                        failures.push(format!("{id} {lane:?}\n  expected {}\n  actual   {}", wanted.unwrap_or("exception".into()), describe(&actual)));
                    }
                }
                (true, Some(_), _) => counts.refused += 1,
                (true, None, None) => counts.exceptions += 1,
                (true, None, Some(_)) => counts.equal += 1,
            }
        }
    }
    assert!(failures.is_empty(), "{} mismatches:\n{}", counts.mismatch, failures.join("\n"));
    counts
}

#[test]
fn oracle_grammar() {
    let counts = replay(GRAMMAR, &[Lane::Raw, Lane::Folded]);
    println!("grammar {counts:?}");
    assert_eq!(counts.mismatch, 0);
    assert!(counts.equal > 2000 && counts.refused > 0, "{counts:?}");
}

#[test]
fn oracle_families() {
    let counts = replay(FAMILIES, &[Lane::Raw, Lane::Folded]);
    println!("families {counts:?}");
    assert_eq!(counts.mismatch, 0);
    assert!(counts.equal > 8000 && counts.refused > 100 && counts.exceptions > 0, "{counts:?}");
}

#[test]
fn processor_replays_the_oracle_fixtures() {
    let grammar = replay(GRAMMAR, &[Lane::Processor]);
    let families = replay(FAMILIES, &[Lane::Processor]);
    println!("processor grammar {grammar:?}\nprocessor families {families:?}");
    assert_eq!((grammar.mismatch, families.mismatch), (0, 0));
    assert!(grammar.equal > 1100 && grammar.refused > 0, "{grammar:?}");
    assert!(families.equal > 4200 && families.refused > 50 && families.exceptions > 0, "{families:?}");
}

fn optional_text(value: Option<&Json>) -> Option<Units> {
    match value {
        Some(Json::Str(s)) => Some(s.clone()),
        _ => None,
    }
}
// A host.json value as a stored value.
fn value(json: &Json, budget: &risunest_ir::Budget) -> Value {
    risunest_ir::json::parse_json(&String::from_utf16(&json_string(json)).unwrap(), budget).unwrap()
}
// A stored array; None for undefined or null.
fn values(json: Option<&Json>, budget: &risunest_ir::Budget) -> Option<Vec<Value>> {
    json.filter(|j| !matches!(j, Json::Null)).map(|j| list(j).iter().map(|v| value(v, budget)).collect())
}
fn truthy(json: Option<&Json>) -> bool {
    match json {
        None | Some(Json::Null) => false,
        Some(Json::Bool(b)) => *b,
        Some(Json::Num(n)) => *n != 0.0 && !n.is_nan(),
        Some(Json::Str(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// A captureLocalTime pattern; null when the capture could not express the locale's format.
fn locale_pattern(json: Option<&Json>) -> Option<LocalePattern> {
    let json = json.filter(|j| !matches!(j, Json::Null))?;
    let two_digit = |part: &Json| matches!(field(part, "twoDigit"), Some(Json::Bool(true)));
    let parts = list(field(json, "parts").unwrap())
        .iter()
        .map(|part| match field(part, "literal") {
            Some(literal) => LocalePart::Literal(text(literal).clone()),
            None => match String::from_utf16(text(field(part, "field").unwrap())).unwrap().as_str() {
                "year" => LocalePart::Year,
                "month" => LocalePart::Month { two_digit: two_digit(part) },
                "day" => LocalePart::Day { two_digit: two_digit(part) },
                "hour" => LocalePart::Hour,
                "minute" => LocalePart::Minute { two_digit: two_digit(part) },
                "second" => LocalePart::Second { two_digit: two_digit(part) },
                "dayPeriod" => LocalePart::DayPeriod,
                other => panic!("locale part {other}"),
            },
        })
        .collect();
    let texts = |key: &str| field(json, key).map_or_else(Vec::new, |v| list(v).iter().map(|t| text(t).clone()).collect());
    Some(LocalePattern { parts, hours: texts("hours"), day_periods: texts("dayPeriods") })
}
// The localTime of a host.json environment, as captureLocalTime records it.
fn local_time(json: &Json) -> LocalTime {
    let zone = field(json, "zone").unwrap();
    let transitions = list(field(zone, "transitions").unwrap()).iter().map(|t| (number(&list(t)[0]), number(&list(t)[1]))).collect();
    LocalTime {
        zone: TimeZone { transitions, end: number(field(zone, "end").unwrap()) },
        time_format: locale_pattern(field(json, "timeFormat")),
        date_format: locale_pattern(field(json, "dateFormat")),
    }
}
// A step's `history` as the historyAccess the oracle builds from it.
fn history_access(json: &Json, budget: &risunest_ir::Budget) -> HistoryAccess {
    let rows = |json: &Json| list(json).iter().map(|row| value(row, budget)).collect::<Vec<_>>();
    HistoryAccess {
        length: number(field(json, "length").unwrap()) as usize,
        rows: field(json, "rows").map_or_else(Vec::new, |r| list(r).iter().map(|e| (number(&list(e)[0]) as usize, value(&list(e)[1], budget))).collect()),
        complete: field(json, "complete").map(rows),
        searches: field(json, "searches").map_or_else(Vec::new, |s| {
            list(s)
                .iter()
                .map(|e| {
                    let e = list(e);
                    HistorySearch { role: text(&e[0]).clone(), before: number(&e[1]) as i64, count: number(&e[2]) as i64, rows: rows(&e[3]) }
                })
                .collect()
        }),
    }
}

// A host.json snapshot as the processor's Database, read the way the oracle page's DBState, selectedCharID and
// CurrentTriggerIdStore serve it, getUserName() with no bound persona, the snapshot's capture for the fields read
// through product helpers, and the environment of the case's profile.
fn host_database(file: &Json, case: &Json, budget: &risunest_ir::Budget) -> (Database, Units) {
    let name = String::from_utf16(text(field(case, "snapshot").unwrap())).unwrap();
    let snapshot = field(field(file, "snapshots").unwrap(), &name).unwrap();
    let capture = field(field(file, "captures").unwrap(), &name).unwrap();
    let profile = field(case, "profile").map_or_else(|| "utc".into(), |p| String::from_utf16(text(p)).unwrap());
    let environment = field(field(file, "environments").unwrap(), &profile).unwrap();
    let db = field(snapshot, "database").unwrap();
    let selected = number(field(snapshot, "selected").unwrap()) as usize;
    let captured = field(capture, "character").filter(|c| !matches!(c, Json::Null));
    let character = list(field(db, "characters").unwrap()).get(selected).map(|c| {
        let captured = captured.unwrap();
        let lore = |key: &str| values(field(captured, key), budget).unwrap_or_default();
        let chat = &list(field(c, "chats").unwrap())[number(field(c, "chatPage").unwrap()) as usize];
        let mut variables = HashMap::new();
        if let Some(Json::Obj(state)) = field(chat, "scriptstate") {
            for (key, value) in state {
                if key.first() == Some(&36) && !matches!(value, Json::Null) {
                    variables.insert(key[1..].to_vec(), to_string(value));
                }
            }
        }
        let messages = list(field(chat, "message").unwrap())
            .iter()
            .map(|row| risunest_ir::json::parse_json(&String::from_utf16(&json_string(row)).unwrap(), budget).unwrap())
            .collect();
        Character {
            id: text(field(c, "chaId").unwrap()).clone(),
            name: text(field(c, "name").unwrap()).clone(),
            nickname: optional_text(field(c, "nickname")).unwrap_or_default(),
            personality: optional_text(field(c, "personality")),
            description: optional_text(field(c, "desc")),
            scenario: optional_text(field(c, "scenario")),
            example_message: optional_text(field(c, "exampleMessage")),
            first_message: optional_text(field(c, "firstMessage")),
            alternate_greetings: list(field(c, "alternateGreetings").unwrap()).iter().map(|g| text(g).clone()).collect(),
            default_variables: optional_text(field(c, "defaultVariables")).unwrap_or_default(),
            global_lore: lore("globalLore"),
            emotion_images: values(field(captured, "emotionImages"), budget),
            additional_assets: values(field(captured, "additionalAssets"), budget),
            prebuilt_asset_command: truthy(field(captured, "prebuiltAssetCommand")),
            prebuilt_asset_exclude: values(field(captured, "prebuiltAssetExclude"), budget),
            chat: Chat {
                id: optional_text(field(chat, "id")),
                first_message_index: number(field(chat, "fmIndex").unwrap()) as i64,
                note: field(chat, "note").map(|note| value(note, budget)),
                variables,
                unreadable_variables: Default::default(),
                local_variables: strings(field(chat, "GLGlobalVariables")),
                messages,
                local_lore: lore("localLore"),
                window_start: field(snapshot, "windowStart").map(|s| number(s) as usize),
            },
        }
    });
    let modules = list(field(capture, "modules").unwrap())
        .iter()
        .map(|m| Module {
            namespace: field(m, "namespace").map_or(Value::Undefined, |n| value(n, budget)),
            assets: values(field(m, "assets"), budget),
        })
        .collect();
    let author_notes = list(field(db, "promptTemplate").unwrap())
        .iter()
        .filter(|t| optional_text(field(t, "type")) == Some(units("authornote")))
        .map(|t| optional_text(field(t, "defaultText")).unwrap_or_default())
        .collect();
    let viewport = field(file, "viewport").unwrap();
    let database = Database {
        character,
        persona_prompt: optional_text(field(db, "personaPrompt")).unwrap_or_default(),
        main_prompt: optional_text(field(db, "mainPrompt")),
        jailbreak: optional_text(field(db, "jailbreak")),
        global_note: optional_text(field(db, "globalNote")),
        author_notes,
        template_default_variables: optional_text(field(db, "templateDefaultVariables")).unwrap_or_default(),
        global_variables: strings(field(db, "globalChatVariables")),
        model: optional_text(field(db, "aiModel")),
        sub_model: optional_text(field(db, "subModel")),
        jailbreak_toggle: matches!(field(db, "jailbreakToggle"), Some(Json::Bool(true))),
        max_context: number(field(db, "maxContext").unwrap()),
        trigger_id: optional_text(field(snapshot, "triggerId")),
        viewport: (number(field(viewport, "width").unwrap()) as u32, number(field(viewport, "height").unwrap()) as u32),
        modules,
        module_lorebooks: values(field(capture, "moduleLorebooks"), budget).unwrap_or_default(),
        model_info: Some(value(field(capture, "modelInfo").unwrap(), budget)),
        language: optional_text(field(capture, "language")),
        platform: Platform {
            tauri: matches!(field(environment, "tauri"), Some(Json::Bool(true))),
            app_version: text(field(environment, "appVersion").unwrap()).clone(),
            browser_language: text(field(environment, "language").unwrap()).clone(),
        },
        local_time: Some(local_time(field(environment, "localTime").unwrap())),
    };
    (database, optional_text(field(db, "username")).unwrap_or_else(|| units("User")))
}

#[derive(Debug, PartialEq)]
enum HostStep {
    Output { output: Units, effects: Vec<(Units, Option<Units>)> },
    Refused(String),
}

struct ScopeValues<'a> {
    base: &'a [(Units, Json)],
    writes: OwnedVec<(Option<Text>, Option<Text>)>,
}
impl TextVariables for ScopeValues<'_> {
    fn get(&mut self, key: Option<&Text>, budget: &risunest_ir::Budget) -> Result<Text, StageError> {
        let key = key.map(text_units).unwrap_or_else(|| units("undefined"));
        for (name, value) in self.writes.as_slice().iter().rev() {
            budget.work(1)?;
            if name.as_ref().map(text_units).unwrap_or_else(|| units("undefined")) == key {
                return Ok(match value { Some(value) => value.copy(budget)?, None => Text::from_str("null", budget)? });
            }
        }
        budget.work(self.base.len())?;
        let value = self.base.iter().find(|(name, _)| *name == key).map(|(_, value)| value);
        Ok(match value { Some(Json::Str(value)) => Text::from_units(value, budget)?, _ => Text::from_str("null", budget)? })
    }
    fn set(&mut self, key: Option<&Text>, value: Option<&Text>, budget: &risunest_ir::Budget) -> Result<(), StageError> {
        let key = key.map(|key| key.copy(budget)).transpose()?;
        let value = value.map(|value| value.copy(budget)).transpose()?;
        self.writes.push((key, value))?;
        Ok(())
    }
}

// Every host case through the adapter: each step's output and setChatVar calls and the tape values drawn match the
// oracle, a step with `refusal` declines with that reason, and every step the product threw on carries one.
#[test]
fn oracle_host() {
    let file = document(HOST);
    let budget = stage_budget();
    let flag = |step: &Json, key: &str| matches!(field(step, key), Some(Json::Bool(true)));
    let (mut cases, mut steps, mut effects, mut errors, mut refusals, mut draws) = (0, 0, 0, 0, 0, 0);
    let mut failures = Vec::new();
    for case in list(field(&file, "cases").unwrap()) {
        let id = String::from_utf16_lossy(text(field(case, "id").unwrap()));
        let (database, user) = host_database(&file, case, &budget);
        let snapshot = Snapshot {
            user,
            source: Source::Database(Box::new(database)),
            consistant_char: flag(case, "consistantChar"),
            tokenize_accurate: flag(case, "tokenizeAccurate"),
            case_tables: CaseTables::Native,
            ..Snapshot::default()
        };
        let tape = list(field(case, "tape").unwrap()).iter().map(number).collect();
        let clock = number(field(case, "clock").or_else(|| field(&file, "clock")).unwrap());
        let mut processor = Processor::with_clock(snapshot, Tape::new(tape), Arc::new(Fixed(clock)));
        let mut actual = Vec::new();
        let case_steps = list(field(case, "steps").unwrap());
        for (i, step) in case_steps.iter().enumerate() {
            assert!(field(step, "refusal").is_none() || i + 1 == case_steps.len(), "{id}: a refusal step is not the last");
            let integer = |key: &str| field(step, key).filter(|v| !matches!(v, Json::Null)).map(|v| number(v) as i64);
            processor.set_call(CallArgs {
                projected_chat_id: integer("projectedChatID"),
                history_offset: integer("historyOffset"),
                history_access: field(step, "history").map(|h| history_access(h, &budget)),
                current_time: field(step, "currentTime").filter(|v| !matches!(v, Json::Null)).map(number),
                trigger_id: optional_text(field(step, "triggerId")),
            });
            for write in field(step, "apply").map_or(&[][..], list) {
                let write = list(write);
                let value = match &write[1] {
                    Json::Str(value) => Value::Text(Text::from_units(value, &budget).unwrap()),
                    _ => Value::Null,
                };
                processor.apply(&Effect::ChatVariable { name: Text::from_units(text(&write[0]), &budget).unwrap(), value }, &budget).unwrap();
            }
            for edit in field(step, "edits").map_or(&[][..], list) {
                let data = || Value::Text(Text::from_units(text(field(edit, "data").unwrap()), &budget).unwrap());
                let effect = match String::from_utf16(text(field(edit, "kind").unwrap())).unwrap().as_str() {
                    "setData" => Effect::Message(MessageEdit::SetData { index: number(field(edit, "index").unwrap()), data: data() }),
                    "cut" => Effect::Message(MessageEdit::Cut { start: number(field(edit, "start").unwrap()), end: field(edit, "end").map(number) }),
                    "append" => Effect::Message(MessageEdit::Append {
                        role: if text(field(edit, "role").unwrap()) == &units("user") { MessageRole::User } else { MessageRole::Char },
                        data: data(), chat_id: Text::from_units(text(field(edit, "chatId").unwrap()), &budget).unwrap() }),
                    "note" => Effect::ChatField { field: Text::from_str("note", &budget).unwrap(), value: data() },
                    kind => panic!("{id}: unknown synthetic edit {kind}"),
                };
                processor.apply(&effect, &budget).unwrap();
            }
            let to_text = |key: &str| optional_text(field(step, key)).map(|t| Text::from_units(&t, &budget).unwrap());
            let context = TextContext {
                chat_index: field(step, "chatIndex").map_or(-1, |i| number(i) as i64),
                role: to_text("role"),
                run_var: flag(step, "runVar"),
                rm_var: flag(step, "rmVar"),
                visualize: flag(step, "visualize"),
                conditions: CbsConditions {
                    first_message: field(step, "firstmsg").map(|f| matches!(f, Json::Bool(true))),
                    chat_role: to_text("chatRole"),
                },
                ..TextContext::default()
            };
            let input = Text::from_units(text(field(step, "text").unwrap()), &budget).unwrap();
            let result = if let Some(scope) = field(step, "scope") {
                let Json::Obj(base) = field(scope, "variables").unwrap() else { panic!("{id}: scope variables") };
                let mut variables = ScopeValues { base, writes: OwnedVec::new(&budget).unwrap() };
                let trigger_id = optional_text(field(scope, "triggerId")).map(|text| Text::from_units(&text, &budget).unwrap());
                processor.process_scoped(&input, &context, TextScope { variables: &mut variables, trigger_id: trigger_id.as_ref() }, &budget)
                    .map(|output| HostStep::Output { output: text_units(&output), effects: variables.writes.as_slice().iter()
                        .map(|(key, value)| (key.as_ref().map(text_units).unwrap_or_else(|| units("undefined")), value.as_ref().map(text_units))).collect() })
            } else {
                processor.process(&input, &context, &budget).map(|output| {
                    let writes = output
                        .effects
                        .iter()
                        .map(|effect| match effect {
                            Effect::ChatVariable { name, value: Value::Text(value) } => (text_units(name), Some(text_units(value))),
                            Effect::ChatVariable { name, value: Value::Undefined } => (text_units(name), None),
                            other => panic!("{id}: effect {other:?}"),
                        })
                        .collect();
                    HostStep::Output { output: text_units(&output.text), effects: writes }
                })
            };
            match result {
                Ok(output) => actual.push(output),
                Err(error) => {
                    actual.push(HostStep::Refused(reason(error)));
                    break;
                }
            }
        }
        let expected_case = field(case, "expected").unwrap();
        let mut expected = Vec::new();
        for (step, result) in case_steps.iter().zip(list(field(expected_case, "steps").unwrap())) {
            if field(result, "error").is_some() {
                errors += 1;
            }
            expected.push(match (field(step, "refusal"), field(result, "error")) {
                (Some(refusal), _) => HostStep::Refused(String::from_utf16(text(refusal)).unwrap()),
                (None, Some(error)) => panic!("{id}: the product threw {} on a step without a refusal", shown(text(error))),
                (None, None) => HostStep::Output {
                    output: text(field(result, "output").unwrap()).clone(),
                    effects: list(field(result, "effects").unwrap())
                        .iter()
                        .map(|e| (text(&list(e)[0]).clone(), optional_text(Some(&list(e)[1]))))
                        .collect(),
                },
            });
        }
        let consumed = number(field(expected_case, "consumed").unwrap()) as usize;
        cases += 1;
        steps += expected.len();
        refusals += expected.iter().filter(|s| matches!(s, HostStep::Refused(_))).count();
        effects += expected.iter().map(|s| if let HostStep::Output { effects: e, .. } = s { e.len() } else { 0 }).sum::<usize>();
        draws += consumed;
        if actual != expected || processor.rng().consumed() != consumed {
            failures.push(format!(
                "{id}\n  expected {expected:?} consumed {consumed}\n  actual   {actual:?} consumed {}",
                processor.rng().consumed()
            ));
        }
    }
    println!(
        "host cases {cases} steps {steps} effects {effects} errors {errors} refusals {refusals} draws {draws} mismatch {}",
        failures.len()
    );
    assert!(failures.is_empty(), "{} mismatches:\n{}", failures.len(), failures.join("\n"));
    assert!(cases > 140 && effects > 20 && errors >= 1 && refusals >= 9 && draws > 10, "{cases} {effects} {errors} {refusals} {draws}");
}

// Every probe of the captured zones: the instant local_to_utc gives a local time is the one V8's Date constructor gave.
#[test]
fn local_time_matches_the_captured_date_constructor() {
    let file = document(HOST);
    let Some(Json::Obj(environments)) = field(&file, "environments") else { panic!("no environments") };
    let mut probes = 0;
    for (name, environment) in environments {
        let captured = field(environment, "localTime").unwrap();
        let zone = local_time(captured).zone;
        for probe in list(field(captured, "localDates").unwrap()) {
            let (local, instant) = (number(&list(probe)[0]), number(&list(probe)[1]));
            assert_eq!(local_to_utc(&zone, local), Ok(instant), "{} at local {local}", shown(name));
            probes += 1;
        }
    }
    assert!(probes > 100, "{probes} probes");
}

// The string keys ECMA-262 2025 (sections 20.1.3, 23.1.3, 22.1.3, 21.1.3, 20.3.3) and Annex B.2 define on
// each prototype, `length` included for the Array and String exotic prototypes.
const SPEC_PROTOTYPES: [(&str, &[&str]); 5] = [
    ("object", &[
        "constructor", "hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable", "toLocaleString", "toString", "valueOf",
        "__proto__", "__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__",
    ]),
    ("array", &[
        "at", "concat", "constructor", "copyWithin", "entries", "every", "fill", "filter", "find", "findIndex", "findLast",
        "findLastIndex", "flat", "flatMap", "forEach", "includes", "indexOf", "join", "keys", "lastIndexOf", "length", "map",
        "pop", "push", "reduce", "reduceRight", "reverse", "shift", "slice", "some", "sort", "splice", "toLocaleString",
        "toReversed", "toSorted", "toSpliced", "toString", "unshift", "values", "with",
    ]),
    ("string", &[
        "at", "charAt", "charCodeAt", "codePointAt", "concat", "constructor", "endsWith", "includes", "indexOf",
        "isWellFormed", "lastIndexOf", "length", "localeCompare", "match", "matchAll", "normalize", "padEnd", "padStart",
        "repeat", "replace", "replaceAll", "search", "slice", "split", "startsWith", "substring", "toLocaleLowerCase",
        "toLocaleUpperCase", "toLowerCase", "toString", "toUpperCase", "toWellFormed", "trim", "trimEnd", "trimStart",
        "valueOf", "substr", "anchor", "big", "blink", "bold", "fixed", "fontcolor", "fontsize", "italics", "link", "small",
        "strike", "sub", "sup", "trimLeft", "trimRight",
    ]),
    ("number", &["constructor", "toExponential", "toFixed", "toLocaleString", "toPrecision", "toString", "valueOf"]),
    ("boolean", &["constructor", "toString", "valueOf"]),
];

#[test]
fn prototype_refusal_lists_are_the_page_and_specification_union() {
    let file = document(FAMILIES);
    let prototypes = field(field(&file, "environment").unwrap(), "prototypes").unwrap();
    let constants = [OBJECT_PROTO, ARRAY_PROTO, STRING_PROTO, NUMBER_PROTO, BOOLEAN_PROTO];
    for ((name, spec), constant) in SPEC_PROTOTYPES.into_iter().zip(constants) {
        let mut union: std::collections::BTreeSet<String> =
            list(field(prototypes, name).unwrap()).iter().map(|v| String::from_utf16(text(v)).unwrap()).collect();
        union.extend(spec.iter().map(|s| s.to_string()));
        let constant: std::collections::BTreeSet<String> = constant.iter().map(|s| s.to_string()).collect();
        assert_eq!(constant, union, "{name} prototype");
    }
}

#[test]
fn registry_matches_the_ledger() {
    let ledger = document(LEDGER);
    let mut registry: HashMap<&str, &families::Entry> = families::entries().iter().map(|e| (e.id, e)).collect();
    assert_eq!(registry.len(), families::entries().len(), "duplicate registry ids");
    let mut runtime = 0;
    for registration in list(field(&ledger, "registrations").unwrap()) {
        let id = String::from_utf16(text(field(registration, "id").unwrap())).unwrap();
        let callback = String::from_utf16(text(field(registration, "callback").unwrap())).unwrap();
        let status = String::from_utf16(text(field(registration, "status").unwrap())).unwrap();
        let mut keys: Vec<String> =
            list(field(registration, "reachableKeys").unwrap()).iter().map(|k| String::from_utf16(text(k)).unwrap()).collect();
        if callback == "doc_only" {
            assert!(!registry.contains_key(id.as_str()) && keys.is_empty(), "doc-only {id}");
            continue;
        }
        runtime += 1;
        let entry = registry.remove(id.as_str()).unwrap_or_else(|| panic!("ledger registration {id} is not in the registry"));
        let mut registered: Vec<String> = entry.keys.iter().map(|k| k.to_string()).collect();
        registered.sort();
        keys.sort();
        assert_eq!(registered, keys, "{id} keys");
        for key in &keys {
            assert_eq!(families::find_entry(&units(key)).map(|e| e.id), Some(entry.id), "{key} lookup");
        }
        // A host or display entry the adapter admits reads the captured database; a pending or declined one is refused.
        let adapter = field(registration, "adapter").map(|a| String::from_utf16(text(a)).unwrap());
        let admitted = match (status.as_str(), adapter.as_deref()) {
            ("native", None) => entry.status == Status::Native,
            ("host" | "display", Some("native")) => matches!(entry.status, Status::Snapshot | Status::Host),
            ("host" | "display", Some("pending" | "decline")) => entry.status == Status::Refused,
            ("unsupported", None) => entry.status == Status::Refused,
            other => panic!("{id} has status {other:?}"),
        };
        assert!(admitted, "{id}: ledger {status} adapter {adapter:?}, registry {:?}", entry.status);
    }
    assert!(registry.is_empty(), "registry ids missing from the ledger: {:?}", registry.keys().collect::<Vec<_>>());
    let counts = field(&ledger, "counts").unwrap();
    assert_eq!(runtime as f64, number(field(counts, "runtimeRegistrations").unwrap()));
}

// The units the case-table oracle gives a code point: a surrogate alone, any other code point in UTF-16.
fn code_point_units(code: u32) -> Units {
    char::from_u32(code).map_or_else(|| vec![code as u16], |c| c.encode_utf16(&mut [0; 2]).to_vec())
}
// The Final_Sigma context `units` give a capital sigma, as the oracle probes it: bit 1 when a sigma before them stays
// medial (they are cased and not case-ignorable), bit 0 when a sigma after them is final (they are case-ignorable or
// cased).
fn sigma_class(units: &[u16]) -> u8 {
    let after = super::text::lower(&[&[0x391, 0x3a3][..], units].concat());
    let before = super::text::lower(&[&[0x391][..], units, &[0x3a3]].concat());
    u8::from(after[1] == 0x3c3) * 2 + u8::from(before.last() == Some(&0x3c2))
}

// The result of a case-table set: `lower`, `upper` and `capitalize` of every code point and lone surrogate that maps to
// something else, and the Final_Sigma context class of every one, in runs.
fn recorded_table(result: &Json, name: &str) -> HashMap<u32, Units> {
    list(field(result, name).unwrap()).iter().map(|entry| (number(&list(entry)[0]) as u32, text(&list(entry)[1]).clone())).collect()
}
// For each code point, whether this crate's tables map and class it as the recorded engine does. Both map a string one
// code point at a time and read the context only for a capital sigma, by these classes, so equal code points give
// equal strings.
fn alike_with_recorded_engine(result: &Json) -> Vec<bool> {
    let (lower, upper, capitalize) = (recorded_table(result, "lower"), recorded_table(result, "upper"), recorded_table(result, "capitalize"));
    assert!(lower.len() > 1000 && upper.len() > 1000 && capitalize.len() > 1000);
    let mut classes = list(field(result, "classes").unwrap()).iter().map(|range| {
        let range = list(range);
        (number(&range[0]) as u32, number(&range[1]) as u32, number(&range[2]) as u8)
    }).peekable();
    (0..=0x10ffffu32).map(|code| {
        while classes.peek().is_some_and(|&(_, end, _)| end < code) { classes.next(); }
        let &(start, _, class) = classes.peek().expect("classes cover every code point");
        assert!(start <= code);
        let units = code_point_units(code);
        let recorded = |table: &HashMap<u32, Units>| table.get(&code).cloned().unwrap_or_else(|| units.clone());
        super::text::lower(&units) == recorded(&lower) && super::text::upper(&units) == recorded(&upper)
            && families::capitalize(&units) == recorded(&capitalize) && sigma_class(&units) == class
    }).collect()
}

// The case table `benchmarks/rust-core-oracles/cbs-host.oracle.test.ts` records through the product callbacks in a
// Unicode 16 engine: under Unicode 16 tables every code point outside `CASE_DIFFERS` maps and classes as there, and
// each one in it differs, so the list holds no stale entry.
#[test]
fn unicode16_case_mapping_matches_the_recorded_engine() {
    let file = document(CASE_TABLE);
    let result = field(&list(field(&file, "cases").unwrap())[0], "result").unwrap();
    assert_eq!(text(field(result, "caseUnicode").unwrap()), &units("unicode16"));
    assert!(matches!(field(result, "tailoredCase"), Some(Json::Bool(false))), "the oracle locale must use the root mapping");
    let (mut mismatched, mut stale) = (Vec::new(), Vec::new());
    for (code, alike) in (0..=0x10ffffu32).zip(alike_with_recorded_engine(result)) {
        match (super::text::case_differs(&code_point_units(code)), alike) {
            (false, false) => mismatched.push(code),
            (true, true) => stale.push(code),
            _ => {}
        }
    }
    assert!(mismatched.is_empty(), "map differently from the recorded engine: {mismatched:x?}");
    assert!(stale.is_empty(), "listed in CASE_DIFFERS but alike: {stale:x?}");
}

// The same table recorded by `benchmarks/rust-core-oracles/cbs-case-table-webview.capture.mjs` in the engine of a
// WebView2 runtime, run as the Edge build of the same version (its `userAgent`), which the `caseUnicode` probes of
// `src/ts/process/nativeHostCapture.ts` read as Unicode 17: `Native`, which stands for Unicode 17 tables, maps and
// classes every code point as there.
#[test]
fn native_case_mapping_matches_the_recorded_webview() {
    assert_eq!(char::UNICODE_VERSION, (17, 0, 0), "Native stands for the Unicode 17 tables");
    let file = document(CASE_TABLE_WEBVIEW);
    let result = field(&list(field(&file, "cases").unwrap())[0], "result").unwrap();
    // The probes: U+10D70 and U+A7D3 have capitals (Unicode 16 and 17), and U+0277 and U+A7DD map to themselves.
    let (lower, upper) = (recorded_table(result, "lower"), recorded_table(result, "upper"));
    assert_eq!(upper.get(&0x10d70), Some(&code_point_units(0x10d50)));
    assert_eq!(upper.get(&0xa7d3), Some(&vec![0xa7d2]));
    assert!(!upper.contains_key(&0x277) && !lower.contains_key(&0xa7dd));
    let mismatched: Vec<u32> = (0..=0x10ffffu32).zip(alike_with_recorded_engine(result)).filter(|(_, alike)| !alike).map(|(code, _)| code).collect();
    assert!(mismatched.is_empty(), "map differently from the recorded WebView: {mismatched:x?}");
}

// The measure-mode texts of benchmarks/rust-cbs/run.mjs with its variables, evaluated in process: preparation,
// evaluation and output decoding apart, and each macro of the two templates repeated alone.
// `cargo test --release --lib measure_evaluation_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn measure_evaluation_cost() {
    use std::time::Instant;
    let pairs = [
        ("enabled", "1"), ("score", "7"), ("next", "{{getvar::score}}"), ("name", "equal"), ("sep", "::"), ("a", "a::a"),
        ("list", "[\"x\",1.0,null]"), ("off", "0"), ("expr", "#if 1"), ("close", "}}"), ("open", "{{"),
        ("replacementNext", "{{user}}"), ("replacementName", "replace"), ("replacementTail", "::x::x::y"),
        ("arrayName", "arraysplice"), ("arrayTail", "::[1,2]::1::1::x"), ("arrayInput", "[1,true,null,[2],{\"x\":3}]"),
        ("arrayNext", "{{user}}"), ("hashName", "hash"), ("hashTail", "::Synthetic"), ("hashInput", "Synthetic"),
        ("hashNext", "{{hash::Synthetic}}"), ("scalarNext", "{{fixnum::1.005::2}}"), ("aggregateNext", "{{sum::[true,2]}}"),
    ];
    let mut variables: HashMap<Units, Units> = pairs.iter().map(|(k, v)| (units(k), units(v))).collect();
    variables.insert(units("surrogate"), vec![0xd800]);
    let context = Context {
        variables,
        globals: [(units("toggle_demo"), units("1"))].into_iter().collect(),
        index: 7,
        user: units("Synthetic"),
        state: 12345,
        ..Context::default()
    };
    let functions: HashMap<Units, Units> = [(units("syntheticCall"), units("{{random::a::b}}"))].into_iter().collect();
    let median = |reps: usize, f: &mut dyn FnMut()| -> f64 {
        let mut samples: Vec<f64> = (0..reps)
            .map(|_| {
                let start = Instant::now();
                f();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        samples.sort_by(f64::total_cmp);
        samples[reps / 2]
    };
    let dynamic = "[panel]{{getvar::score}}:{{equal::{{getvar::score}}::7}}/{{arrayelement::{{split::a,b,c::,}}::1}}/{{#when::var::enabled}}{{random::a::b}}{{:else}}x{{/when}}[/panel]";
    let utf16 = "한글😀 {{length::{{getvar::surrogate}}}} {{arrayelement::[1.0,1e-7]::1}} {{split::😀::}}";
    let mut texts: Vec<(String, Units)> = Vec::new();
    for count in [200usize, 1000] {
        texts.push((format!("dynamic-{count}"), units(&vec![dynamic; count].concat())));
        texts.push((format!("utf16-{count}"), units(&vec![utf16; count].join("\n"))));
    }
    for part in [
        "[panel]", "{{getvar::score}}", "{{equal::7::7}}", "{{equal::{{getvar::score}}::7}}", "{{split::a,b,c::,}}",
        "{{arrayelement::[\"a\",\"b\",\"c\"]::1}}", "{{arrayelement::{{split::a,b,c::,}}::1}}", "{{random::a::b}}",
        "{{#when::var::enabled}}a{{:else}}x{{/when}}", "{{#when::var::enabled}}{{random::a::b}}{{:else}}x{{/when}}",
        "한글😀 ", "{{length::{{getvar::surrogate}}}}", "{{length::x}}", "{{arrayelement::[1.0,1e-7]::1}}", "{{split::😀::}}",
    ] {
        texts.push((format!("x1000 {part}"), units(&vec![part; 1000].concat())));
    }
    println!("{:<72} {:>7} {:>8} {:>8} {:>8} {:>8} {:>8}", "text", "units", "prep ms", "eval ms", "out ms", "ns/unit", "us/rep");
    for (name, source) in &texts {
        let prep = median(31, &mut || drop(parser::prepare(source, false, Options::default())));
        let prepared = parser::prepare(source, false, Options::default());
        let mut output = Vec::new();
        let eval = median(31, &mut || {
            let mut current = context.clone();
            output = parser::run(&prepared, &mut current, &functions, &mut Budget::default()).unwrap();
        });
        let out = median(31, &mut || drop(String::from_utf16(&output)));
        let per_unit = eval * 1e6 / source.len() as f64;
        let per_rep = if name.starts_with("x1000") { eval } else { f64::NAN };
        println!("{:<72} {:>7} {:>8.3} {:>8.3} {:>8.3} {:>8.1} {:>8.3}", shown(&units(name)), source.len(), prep, eval, out, per_unit, per_rep);
    }
}
