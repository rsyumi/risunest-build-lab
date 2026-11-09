//! What both trigger test files share: the oracle's database layout, the records the route owner reads from it, the
//! CBS processor over that database, the shared `Math.random` tape, the oracle's similarity search and a driver that
//! answers host requests.
#![allow(dead_code)]

use risunest_cbs_experimental::cbs::processor::{Character, Chat, Database, Processor, Snapshot, Source};
use risunest_ir::budget::{Budget, Error, Limits, OwnedVec};
use risunest_ir::json::parse_json;
use risunest_ir::num::number_to_text;
use risunest_ir::rng::{Rng, Tape};
use risunest_ir::stage::{
    EmbedRequest, Effect, HostReply, HostRequest, StageError, Step, TriggerCbsVariables, TriggerMode, TriggerOutput, TriggerRequest,
    TriggerRunner,
};
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_records::modules::{ModuleSources, SelectedModules};
use risunest_records::preset::Preset;
use risunest_records::root::Root;
use risunest_records::settings::Settings;
use risunest_regex::engine::JsPatternEngine;
use risunest_tokenizer::counter::{ChatAccounting, Counter};
use risunest_tokenizer::id::TokenizerId;
use risunest_triggers::input::{MessageIds, TriggerInput};
use risunest_triggers::runner::{SimilaritySearch, TriggerRun};
use std::sync::{atomic::AtomicBool, Arc, Mutex};

pub const CASES: &[u8] = include_bytes!("../../../../../tests/fixtures/rust-core/triggers/run.cases.json");
pub const EXPECTED: &[u8] = include_bytes!("../../../../../tests/fixtures/rust-core/triggers/run.expected.json");
/// The route contract: what the native Send route declines that TS runs (`triggers.oracle.test.ts` restates it).
pub const ROUTE_CASES: &[u8] = include_bytes!("../../../../../tests/fixtures/rust-core/triggers/route-declines.cases.json");
pub const ROUTE_EXPECTED: &[u8] = include_bytes!("../../../../../tests/fixtures/rust-core/triggers/route-declines.expected.json");

pub fn budget() -> Budget {
    Budget::new(Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 22, max_values: 1 << 22,
        max_depth: 64, max_work: 1 << 40 }, Arc::new(AtomicBool::new(false))).unwrap()
}

pub fn units(text: &str) -> Vec<u16> { text.encode_utf16().collect() }
/// The text with each isolated surrogate spelled as the characters `\u{xxxx}`, so they compare exactly.
pub fn string(text: &Text) -> String {
    char::decode_utf16(text.units()).map(|decoded| match decoded {
        Ok(character) => character.to_string(),
        Err(error) => format!("\\u{{{:04x}}}", error.unpaired_surrogate()),
    }).collect()
}
pub fn field<'a>(value: &'a Value, key: &str, budget: &Budget) -> Option<&'a Value> { value.get(key.as_bytes(), budget).unwrap() }
pub fn text_field(value: &Value, key: &str, budget: &Budget) -> Option<String> {
    match field(value, key, budget) { Some(Value::Text(text)) => Some(string(text)), _ => None }
}

/// `String(Number(value))`, with `-0` spelled out.
fn number(value: f64, budget: &Budget) -> String {
    if value == 0.0 && value.is_sign_negative() { return "-0".into(); }
    string(&number_to_text(value, budget).unwrap())
}

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

pub fn write_json(value: &Value, out: &mut String, budget: &Budget) {
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

/// `databaseFor` of the oracle: `{ ...base.root, ...case.root, characters: [{ ...character, chats: [chat] }] }`.
pub fn database_text(base: &Value, case: &Value, budget: &Budget) -> String {
    let chat = spread(&[field(base, "chat", budget), field(case, "chat", budget)], Vec::new(), budget);
    let character = spread(&[field(base, "character", budget), field(case, "character", budget)], vec![("chats", format!("[{chat}]"))], budget);
    spread(&[field(base, "root", budget), field(case, "root", budget)], vec![("characters", format!("[{character}]"))], budget)
}

pub fn base(budget: &Budget) -> Value { base_of(CASES, budget) }

/// The `base` records of a cases file.
pub fn base_of(cases: &[u8], budget: &Budget) -> Value {
    let file = parse_json(std::str::from_utf8(cases).unwrap(), budget).unwrap();
    let Value::Object(mut fields) = file else { panic!("the cases file is an object") };
    let index = fields.as_slice().iter().position(|(name, _)| name.equals_ascii(b"base")).unwrap();
    std::mem::replace(&mut fields.as_mut_slice()[index].1, Value::Null)
}

/// What the route owner builds the run from (`src-tauri/src/native_generation/modules.rs` and `lore.rs`): the settings
/// the selected preset gives over the stored root mirrors (a case's preset is `botPresets[botPresetsId]`), the modules
/// selected with the effective `moduleIntergration`, and a copy of the stored root holding the effective
/// `templateDefaultVariables`, which the run and its CBS processor read.
pub fn owner_input(database: &Value, budget: &Budget) -> (Value, TriggerInput) {
    let preset = match (field(database, "botPresets", budget), field(database, "botPresetsId", budget)) {
        (Some(Value::Array(presets)), Some(Value::Number(index))) => presets.as_slice().get(*index as usize),
        _ => None,
    };
    let settings = Settings::derive(Root::new(database).unwrap(), preset.map(|preset| Preset::new(preset).unwrap()), budget).unwrap();
    let template = settings.get("templateDefaultVariables", budget).unwrap().map_or(Value::Undefined, |value| copy(value, budget));
    let mut root = copy(database, budget);
    root.define(Text::from_str("templateDefaultVariables", budget).unwrap(), template, budget).unwrap();
    let record = character(database, budget);
    let chat = conversation(record, budget);
    let sources = ModuleSources::for_conversation(database, record, chat, budget).unwrap();
    let modules = SelectedModules::capture(&sources, settings.get("moduleIntergration", budget).unwrap(), budget).unwrap();
    let input = TriggerInput::from_records_with_modules(&root, record, chat, &modules, budget).unwrap();
    (root, input)
}

/// A database from the oracle's base records and a case written as the generator writes one.
pub fn case_database(case: &str, budget: &Budget) -> Value {
    let case = parse_json(case, budget).unwrap();
    parse_json(&database_text(&base(budget), &case, budget), budget).unwrap()
}

pub fn character<'a>(database: &'a Value, budget: &Budget) -> &'a Value {
    field(database, "characters", budget).and_then(Value::array).and_then(|characters| characters.first()).unwrap()
}

pub fn conversation<'a>(character: &'a Value, budget: &Budget) -> &'a Value {
    field(character, "chats", budget).and_then(Value::array).and_then(|chats| chats.first()).unwrap()
}

/// `Math.random`, one tape for CBS and the trigger effects.
#[derive(Clone)]
pub struct SharedTape(pub Arc<Mutex<Tape>>);

impl SharedTape {
    pub fn new(values: Vec<f64>) -> Self { Self(Arc::new(Mutex::new(Tape::new(values)))) }
    pub fn consumed(&self) -> usize { self.0.lock().unwrap().consumed() }
}

impl Rng for SharedTape {
    fn random(&mut self) -> Result<f64, Error> { self.0.lock().unwrap().random() }
}

/// The oracle's `HypaProcesser`: `similaritySearch` returns the added texts in reverse. The search asks the host once,
/// with the documents and then the query as the texts to embed, and ignores the vectors. It stands in for the trigger's
/// side only (split, documents, query, join, outputVar); ranking and the embedding and cache flow are not compared.
#[derive(Default)]
pub struct ReversedSimilarity { found: Option<Vec<Text>> }

impl SimilaritySearch for ReversedSimilarity {
    fn search(&mut self, documents: Vec<Text>, query: Text, budget: &Budget) -> Result<Step<Vec<Text>>, StageError> {
        let mut texts = Vec::new();
        for document in &documents { texts.push(document.copy(budget)?); }
        texts.push(query);
        self.found = Some(documents.into_iter().rev().collect());
        Ok(Step::NeedsHost(HostRequest::Embed(EmbedRequest { model: Text::from_str("oracle-similarity", budget)?, texts })))
    }
    fn resume(&mut self, reply: HostReply, _budget: &Budget) -> Result<Step<Vec<Text>>, StageError> {
        match reply {
            HostReply::Embeddings(_) => Ok(Step::Done(self.found.take().expect("a search"))),
            HostReply::Unsupported(name) => Err(StageError::Unsupported(name)),
            _ => Err(StageError::Invalid("similarity-reply".into())),
        }
    }
}

/// Version 4 UUIDs in draw order.
pub struct CountingIds(pub usize);

impl MessageIds for CountingIds {
    fn next_id(&mut self, budget: &Budget) -> Result<Text, StageError> {
        self.0 += 1;
        Ok(Text::from_str(&format!("00000000-0000-4000-8000-{:012x}", self.0), budget)?)
    }
}

pub fn copy(value: &Value, budget: &Budget) -> Value {
    let mut text = String::new();
    write_json(value, &mut text, budget);
    parse_json(&text, budget).unwrap()
}

/// The `risuChatParser` reads of the selected character and chat in `database`.
pub fn processor(database: &Value, tape: SharedTape, budget: &Budget) -> Processor<SharedTape> {
    let character = character(database, budget);
    let chat = conversation(character, budget);
    let text = |value: &Value, key: &str| text_field(value, key, budget).map(|text| units(&text));
    let mut variables = std::collections::HashMap::new();
    for (key, value) in field(chat, "scriptstate", budget).and_then(Value::object).unwrap_or(&[]) {
        let key: Vec<u16> = key.units().collect();
        if key.first() != Some(&u16::from(b'$')) || matches!(value, Value::Null | Value::Undefined) { continue; }
        variables.insert(key[1..].to_vec(), units(&string(&value.to_text(budget).unwrap())));
    }
    let first_message_index = match field(chat, "fmIndex", budget) { Some(Value::Number(index)) => *index as i64, _ => -1 };
    let messages = field(chat, "message", budget).and_then(Value::array).unwrap_or(&[]).iter().map(|row| copy(row, budget)).collect();
    let greetings = field(character, "alternateGreetings", budget).and_then(Value::array).unwrap_or(&[]).iter()
        .filter_map(|greeting| if let Value::Text(text) = greeting { Some(units(&string(text))) } else { None }).collect();
    let snapshot = Database {
        character: Some(Character {
            id: text(character, "chaId").unwrap_or_default(),
            name: text(character, "name").unwrap_or_default(),
            first_message: text(character, "firstMessage"),
            alternate_greetings: greetings,
            default_variables: text(character, "defaultVariables").unwrap_or_default(),
            chat: Chat {
                id: text(chat, "id"),
                first_message_index,
                note: field(chat, "note", budget).map(|note| copy(note, budget)),
                variables,
                messages,
                ..Chat::default()
            },
            ..Character::default()
        }),
        template_default_variables: text(database, "templateDefaultVariables").unwrap_or_default(),
        ..Database::default()
    };
    let user = text(database, "username").unwrap_or_default();
    Processor::new(Snapshot { user, source: Source::Database(Box::new(snapshot)), ..Snapshot::default() }, tape)
}

pub type Run = TriggerRun<Processor<SharedTape>, Counter, JsPatternEngine, SharedTape, CountingIds>;

/// A run whose owner answers every host request the stage makes: plugin tokenizer counts, model requests and searches.
pub fn runner(database: &Value, input: TriggerInput, tape: &SharedTape, budget: &Budget) -> Run {
    TriggerRun::new(input, processor(database, tape.clone(), budget), Counter::new(TokenizerId::Plugin, ChatAccounting::default()),
        JsPatternEngine::default(), tape.clone(), CountingIds(0)).with_similarity(Box::new(ReversedSimilarity::default())).with_llm()
}

pub fn request(mode: TriggerMode, manual_name: Option<&str>, trigger_id: Option<&str>, budget: &Budget) -> TriggerRequest {
    let text = |value: Option<&str>| value.map(|value| Text::from_str(value, budget).unwrap());
    TriggerRequest { mode, display: None, manual_name: text(manual_name), display_mode: matches!(mode, TriggerMode::Display | TriggerMode::Request),
        temp_vars: None, trigger_id: text(trigger_id), cbs_variables: TriggerCbsVariables::TriggerScope }
}

pub struct Driven {
    pub result: Result<TriggerOutput, StageError>,
    /// Every effect the owner received, in delivery order: `take_effects` before each request and after an error,
    /// then `Done`'s suffix.
    pub effects: Vec<Effect>,
    /// The texts of the plugin tokenizer requests.
    pub tokenizer: Vec<String>,
    /// The kinds of the effects `take_effects` returned before each request.
    pub drained: Vec<Vec<&'static str>>,
    /// The other requests, in order: model requests and the similarity searches' embedding requests.
    pub hosts: Vec<HostRequest>,
    /// The effects `take_effects` returned after an error.
    pub after_error: usize,
    pub console: Vec<String>,
}

/// Runs `request` to the end, answering each plugin tokenizer request with `Math.ceil(length / 4)` tokens and each
/// embedding request with no vectors; any other request panics.
pub fn drive(run: &mut Run, request: TriggerRequest, budget: &Budget) -> Driven {
    drive_with(run, request, &mut |other| panic!("unexpected host request {other:?}"), budget)
}

/// [`drive`], answering the other requests with `answer`.
pub fn drive_with(run: &mut Run, request: TriggerRequest, answer: &mut dyn FnMut(&HostRequest) -> HostReply, budget: &Budget) -> Driven {
    let (mut effects, mut tokenizer, mut drained, mut after_error, mut hosts) = (Vec::new(), Vec::new(), Vec::new(), 0, Vec::new());
    let mut step = run.run(request, budget);
    let result = loop {
        match step {
            Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text })) => {
                let taken = run.take_effects();
                drained.push(taken.iter().map(kind).collect::<Vec<_>>());
                effects.extend(taken);
                tokenizer.push(string(&text));
                step = run.resume(HostReply::Tokens(vec![0.0; text.len_units().div_ceil(4)]), budget);
            }
            Ok(Step::NeedsHost(other)) => {
                let taken = run.take_effects();
                drained.push(taken.iter().map(kind).collect::<Vec<_>>());
                effects.extend(taken);
                let reply = match &other { HostRequest::Embed(_) => HostReply::Embeddings(Vec::new()), _ => answer(&other) };
                hosts.push(other);
                step = run.resume(reply, budget);
            }
            Ok(Step::Done(mut output)) => {
                assert!(run.take_effects().is_empty(), "Done carries the remaining effects");
                effects.append(&mut output.effects);
                break Ok(output);
            }
            Err(error) => {
                let taken = run.take_effects();
                after_error = taken.len();
                effects.extend(taken);
                break Err(error);
            }
        }
    };
    // `console.log` prints each value's string.
    let console = run.console_log().iter().map(|value| string(&value.to_text(budget).unwrap())).collect();
    Driven { result, effects, tokenizer, drained, hosts, after_error, console }
}

pub fn kind(effect: &Effect) -> &'static str {
    match effect {
        Effect::ChatVariable { .. } => "variable",
        Effect::ChatField { .. } => "field",
        Effect::Message(_) => "message",
        _ => "other",
    }
}

pub fn empty_object(budget: &Budget) -> Value { Value::Object(OwnedVec::new(budget).unwrap()) }
