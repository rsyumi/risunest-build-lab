//! The route's CBS stage: the native CBS processor over the stored records `risuChatParser` reads during a send.

use risunest_cbs_experimental::cbs::processor::{text_units, CaseTables, Character as Card, Chat, Database, LocalTime, Processor, Snapshot, Source,
    TimeZone};
use risunest_cbs_experimental::cbs::{units, Units};
use risunest_ir::clock::{Clock, Fixed, System};
use risunest_ir::rng::{OsEntropy, Rng, Tape};
use risunest_ir::stage::{Effect, ScopedTextProcessor, StageError, TextContext, TextOutput, TextProcessor, TextScope};
use risunest_ir::{json::parse_json, Budget, Error, Text, Value};
use risunest_prompt::machine::{EngineCase, PromptInput};
use risunest_records::root::{copy, is_text, Root};
use risunest_records::{character::Character, conversation::Conversation, persona, preset::Preset, settings::Settings};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

/// The registrations whose host state the route does not capture: the trigger ID store, the window size and the app
/// metadata. Each declines by its registry ID.
const DECLINED: [&str; 4] = ["trigger_id", "screenwidth", "screenheight", "metadata"];

/// The same inside a start trigger, where `{{trigger_id}}` reads the run's own ID, which a start trigger leaves null.
const SCOPED_DECLINED: [&str; 3] = ["screenwidth", "screenheight", "metadata"];

/// The host-state registrations no Send oracle case covers yet: whole history rows, message times, assets, modules,
/// lore, the auxiliary model and `declare`. Each declines as `cbs`. `history`, `userhistory` and `charhistory` print
/// whole rows, IDs included, and between the reserving admission and the stored IDs a row without one carries an
/// `admission-N` placeholder (`prepare::prompt_input`), so admitting them needs the stored IDs first.
const UNPROVEN: [&str; 17] = ["lorebook", "userhistory", "charhistory", "messagetime", "messagedate", "messageunixtimearray",
    "messageidleduration", "idleduration", "axmodel", "previouschatlog", "emotionlist", "assetlist", "chardisplayasset",
    "history", "moduleenabled", "moduleassetlist", "declare"];

/// The registrations that read the greeting `fmIndex` selects.
const GREETING_READERS: [&str; 3] = ["previouscharchat", "previoususerchat", "history"];

/// The most `Math.random()` draws one send's admission records; a send whose CBS takes more refuses.
pub(super) const MAX_DRAWS: usize = 4096;

#[path = "output_admission.rs"]
mod output_admission;

pub(super) fn admit_output_replacement(input: &PromptInput, host: Option<&HostCapture>, replacement: &[u16], budget: &Budget)
    -> Result<(), StageError> {
    let (snapshot, _) = snapshot(input, budget, host)?;
    output_admission::admit(&snapshot, replacement, budget)
}


/// `getModelInfo('ollama-hosted')` of `src/ts/model/modellist.ts` as `JSON.stringify` writes it, which the CBS model
/// readers see for the only model the route admits.
pub(crate) const OLLAMA_HOSTED: &str = r#"{"id":"ollama-hosted","name":"Local","fullName":"Ollama Local","provider":16,"format":15,"flags":[6,8],"parameters":["temperature","top_p","frequency_penalty","presence_penalty"],"tokenizer":0,"recommended":true,"shortName":"Local","internalID":"ollama-hosted"}"#;

/// The most local offset changes a capture holds; a zone with more declines what reads it.
const MAX_TRANSITIONS: usize = 1024;

/// What the WebView reported about its JavaScript engine when the send was reserved (`captureNativeHost` of
/// `src/ts/process/nativeHostCapture.ts`), which every run of the send reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HostCapture {
    /// The default locale maps case other than the root mapping.
    tailored_case: bool,
    case_unicode: CaseUnicode,
    time_zone: Zone,
    regex_unicode: RegexUnicode,
    /// A recovery start keeps regex off, so a script run takes only the character's scripts.
    regex_excluded: bool,
}

/// The Unicode version of `toLowerCase` and `toUpperCase`, by `caseUnicode` of the host capture. `From17`, Unicode 17
/// or newer, is what the probe reported before it told Unicode 17 from later versions; a journaled reservation can still
/// hold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CaseUnicode { Before16, Unicode16, From17, Unicode17, From18 }

/// Rust's case tables are the Unicode 17 ones that `CaseTables::Native` maps with and `CaseTables::Unicode16` starts
/// from; a toolchain with other tables reproduces no engine.
const RUST_CASE_UNICODE_17: bool = matches!(char::UNICODE_VERSION, (17, 0, 0));

/// The stages' case tables for an engine: Rust's own, Rust's less the Unicode 16 differences, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reproduced { Native, Unicode16, Foreign }

/// The Unicode version of the RegExp property and case folding tables, by `regexUnicode` of the host capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RegexUnicode { Before17, Unicode17, From18 }

/// The offsets `Date`'s local getters add: each `[start, offset]` holds from its start until the next one, in
/// milliseconds, up to `end`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Zone { transitions: Vec<(i64, i64)>, end: i64 }

impl HostCapture {
    #[cfg(test)]
    pub(crate) fn new(tailored_case: bool, case_unicode: CaseUnicode, transitions: Vec<(i64, i64)>, end: i64) -> Self {
        Self { tailored_case, case_unicode, time_zone: Zone { transitions, end }, regex_unicode: RegexUnicode::Before17, regex_excluded: false }
    }

    #[cfg(test)]
    pub(crate) fn with_regex(self, regex_unicode: RegexUnicode, regex_excluded: bool) -> Self { Self { regex_unicode, regex_excluded, ..self } }

    /// The regex fork's tables are Unicode 17 (`risunest_regex::unicode::TABLE_VERSION`).
    pub(crate) fn foreign_regex_unicode(&self) -> bool { self.regex_unicode != RegexUnicode::Unicode17 }

    pub(crate) fn regex_excluded(&self) -> bool { self.regex_excluded }

    pub(super) fn tailored_case(&self) -> bool { self.tailored_case }

    // Only the Unicode 16 and 17 tables are reproduced: any other engine can map a letter Rust's tables leave alone.
    fn reproduced(&self) -> Reproduced {
        match self.case_unicode {
            _ if !RUST_CASE_UNICODE_17 => Reproduced::Foreign,
            CaseUnicode::Unicode16 => Reproduced::Unicode16,
            CaseUnicode::Unicode17 => Reproduced::Native,
            CaseUnicode::Before16 | CaseUnicode::From17 | CaseUnicode::From18 => Reproduced::Foreign,
        }
    }

    pub(super) fn lore_case_tables(&self) -> risunest_lorebook::matching::CaseTables {
        use risunest_lorebook::matching::CaseTables;
        match self.reproduced() { Reproduced::Native => CaseTables::Native, Reproduced::Unicode16 => CaseTables::Unicode16, Reproduced::Foreign => CaseTables::Foreign }
    }

    fn case_tables(&self) -> CaseTables {
        match self.reproduced() { Reproduced::Native => CaseTables::Native, Reproduced::Unicode16 => CaseTables::Unicode16, Reproduced::Foreign => CaseTables::Foreign }
    }

    /// The tables a trigger run maps case with, as the CBS stage does; without a capture, none.
    pub(super) fn trigger_case_tables(host: Option<&Self>) -> CaseTables { host.map_or(CaseTables::Foreign, Self::case_tables) }

    // At least one start, in increasing order before `end`, each a valid time value, and offsets within a day, as the
    // local time conversion assumes; any other capture declines what reads it.
    fn time_zone(&self) -> Option<TimeZone> {
        let Zone { transitions, end } = &self.time_zone;
        let time = |t: i64| t.unsigned_abs() <= 8_640_000_000_000_000;
        let valid = transitions.len() <= MAX_TRANSITIONS && time(*end)
            && transitions.iter().all(|&(start, offset)| time(start) && offset.unsigned_abs() < 86_400_000)
            && transitions.windows(2).all(|pair| pair[0].0 < pair[1].0) && transitions.last().is_some_and(|last| last.0 < *end);
        valid.then(|| TimeZone { transitions: transitions.iter().map(|&(start, offset)| (start as f64, offset as f64)).collect(),
            end: *end as f64 })
    }
}

// A stored value the snapshot cannot hold as TS reads it.
fn unrepresentable() -> StageError { StageError::Unsupported(Cow::Borrowed("cbs.snapshot")) }

// A string, or undefined when absent.
fn optional(value: Option<&Value>) -> Result<Option<Units>, StageError> {
    match value {
        None => Ok(None),
        Some(Value::Text(text)) => Ok(Some(text_units(text))),
        Some(_) => Err(unrepresentable()),
    }
}

// A string read through `??` or `||` with an empty default.
fn or_empty(value: Option<&Value>) -> Result<Units, StageError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Text(text)) => Ok(text_units(text)),
        Some(_) => Err(unrepresentable()),
    }
}

// A string, or undefined when nullish.
fn nullable(value: Option<&Value>) -> Result<Option<Units>, StageError> {
    match value {
        Some(Value::Null) => Ok(None),
        value => optional(value),
    }
}

// An object of strings, empty when nullish.
fn strings(value: Option<&Value>) -> Result<HashMap<Units, Units>, StageError> {
    match value {
        None | Some(Value::Null) => Ok(HashMap::new()),
        Some(Value::Object(entries)) => entries.as_slice().iter().map(|(key, value)| match value {
            Value::Text(value) => Ok((text_units(key), text_units(value))),
            _ => Err(unrepresentable()),
        }).collect(),
        Some(_) => Err(unrepresentable()),
    }
}

// A stored array, undefined when nullish.
fn values(value: Option<&Value>, budget: &Budget) -> Result<Option<Vec<Value>>, StageError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(entries)) => entries.as_slice().iter().map(|entry| copy(entry, budget)).collect::<Result<_, _>>().map(Some),
        Some(_) => Err(unrepresentable()),
    }
}

// A string, `None` for any other value.
fn text(value: Option<&Value>) -> Option<Units> {
    match value { Some(Value::Text(text)) => Some(text_units(text)), _ => None }
}

/// What `risuChatParser` reads outside its input when the send parses with `{chara: currentChar}`: the effective
/// settings after the load, `getUserName()` and `getPersonaPrompt()` (`src/ts/util.ts:185-210`), and the character after
/// `characterFormatUpdate` (`src/ts/characters.ts`) with its open conversation. The machine refuses stored lore and
/// active modules, so both lists are empty. The WebView's capture gives the default locale's case rules, the engine's
/// Unicode case tables and Date's local zone; without one, locale case mapping, case mapping beyond ASCII and local
/// time decline.
/// Beside it come the registrations whose stored input TS reads in a form the snapshot cannot hold, which decline as
/// `cbs.snapshot` when a send calls them.
fn snapshot(input: &PromptInput, budget: &Budget, host: Option<&HostCapture>) -> Result<(Snapshot, Vec<&'static str>), StageError> {
    snapshot_declining(input, budget, host, &DECLINED)
}

fn snapshot_declining(input: &PromptInput, budget: &Budget, host: Option<&HostCapture>, host_state: &[&'static str])
    -> Result<(Snapshot, Vec<&'static str>), StageError> {
    let root = Root::new(&input.root)?;
    let preset = input.preset.as_ref().map(Preset::new).transpose()?;
    let settings = Settings::derive(root, preset, budget)?;
    let character = Character::new(&input.character)?;
    let conversation = Conversation::new(&input.conversation)?;
    let field = |name: &str| character.get(name, budget);
    let setting = |name: &str| settings.get(name, budget);
    let mut unreadable = Vec::new();
    // `getUserName()` and `getPersonaPrompt()` return a bound persona's fields as stored, and otherwise
    // `username ?? 'User'` and `personaPrompt ?? ''`; a value the snapshot cannot hold leaves the function unreadable.
    let (name, prompt, defaults) = match persona::bound(root, conversation, budget)? {
        Some(bound) => (bound.get("name", budget)?, bound.get("personaPrompt", budget)?, None),
        None => (setting("username")?, setting("personaPrompt")?, Some(("User", ""))),
    };
    let mut read = |value: Option<&Value>, nullish: Option<&str>, id: &'static str| match (value, nullish) {
        (Some(Value::Text(text)), _) => text_units(text),
        (None | Some(Value::Null), Some(default)) => units(default),
        _ => { unreadable.push(id); Vec::new() }
    };
    let user = read(name, defaults.map(|(user, _)| user), "user");
    let persona_prompt = read(prompt, defaults.map(|(_, prompt)| prompt), "persona");
    // `{{authornote}}` falls back to the `defaultText` of the first `authornote` card with a truthy one when the chat
    // note is not truthy, iterating the template as TS does: a string has no cards, and another truthy value or a
    // nullish card throws.
    let mut author_notes = Vec::new();
    let readable = match setting("promptTemplate")? {
        Some(Value::Array(cards)) => {
            let mut readable = true;
            for card in cards.as_slice() {
                if matches!(card, Value::Null | Value::Undefined) { readable = false; break; }
                if !matches!(card, Value::Object(_)) || !is_text(card.get(b"type", budget)?, "authornote") { continue; }
                match card.get(b"defaultText", budget)? {
                    Some(Value::Text(text)) if text.len_units() != 0 => { author_notes.push(text_units(text)); break; }
                    Some(value) if value.truthy() => { readable = false; break; }
                    _ => {}
                }
            }
            readable
        }
        Some(Value::Text(_)) => true,
        Some(value) => !value.truthy(),
        None => true,
    };
    if !readable && !conversation.get("note", budget)?.is_some_and(Value::truthy) { unreadable.push("authornote"); }
    let max_context = match setting("maxContext")? { Some(Value::Number(value)) => *value, _ => return Err(unrepresentable()) };
    // `getChatVar` reads `scriptstate?.['$' + key]` and converts a value other than null with `toString()`, which throws
    // for an own `toString`; a function that reads such a key declines until a write replaces the value.
    let mut variables = HashMap::new();
    let mut unreadable_variables = HashSet::new();
    if let Some(Value::Object(entries)) = conversation.get("scriptstate", budget)? {
        for (key, value) in entries.as_slice() {
            let key = text_units(key);
            if key.first() != Some(&0x24) || matches!(value, Value::Null | Value::Undefined) { continue; }
            match value.to_text(budget) {
                Ok(value) => { variables.insert(key[1..].to_vec(), text_units(&value)); }
                Err(Error::Type { .. }) => { unreadable_variables.insert(key[1..].to_vec()); }
                Err(error) => return Err(error.into()),
            }
        }
    }
    // `{{firstmsgindex}}` prints `fmIndex` and the greeting readers index with it.
    let first_message_index = match conversation.get("fmIndex", budget)? {
        Some(Value::Number(index)) if index.fract() == 0.0 && index.abs() < 9007199254740992.0 => *index as i64,
        _ => {
            unreadable.push("firstmsgindex");
            unreadable.extend(GREETING_READERS);
            -1
        }
    };
    let messages = match conversation.get("message", budget)? {
        Some(Value::Array(rows)) => rows.as_slice().iter().map(|row| copy(row, budget)).collect::<Result<_, _>>()?,
        _ => return Err(unrepresentable()),
    };
    // `alternateGreetings[fmIndex]`, which only an `fmIndex` other than -1 reads.
    let selected = usize::try_from(first_message_index).ok();
    let greetings = match field("alternateGreetings")? {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(greetings)) => greetings.as_slice(),
        Some(_) => {
            if selected.is_some() { unreadable.extend(GREETING_READERS); }
            &[][..]
        }
    };
    let mut alternate_greetings = Vec::with_capacity(greetings.len());
    for (index, greeting) in greetings.iter().enumerate() {
        alternate_greetings.push(text(Some(greeting)).unwrap_or_else(|| {
            if selected == Some(index) { unreadable.extend(GREETING_READERS); }
            Vec::new()
        }));
    }
    // `pick` and `rollp` hash `chaId + chat.id`, and the send gives a chat without a truthy `id` a random one first.
    let chat_id = match conversation.get("id", budget)? {
        Some(Value::Text(id)) if id.len_units() != 0 => Some(text_units(id)),
        _ => { unreadable.extend(["pick", "rollp"]); None }
    };
    // `{{description}}` parses `desc` as stored.
    let description = match field("desc")? {
        None => None,
        Some(Value::Text(desc)) => Some(text_units(desc)),
        Some(_) => { unreadable.push("description"); None }
    };
    let card = Card {
        id: optional(field("chaId")?)?.filter(|id| !id.is_empty()).ok_or_else(unrepresentable)?,
        name: optional(field("name")?)?.ok_or_else(unrepresentable)?,
        // `{{char}}` reads `nickname || name`.
        nickname: or_empty(field("nickname")?)?,
        personality: Some(or_empty(field("personality")?)?),
        description,
        scenario: Some(or_empty(field("scenario")?)?),
        example_message: Some(or_empty(field("exampleMessage")?)?),
        first_message: optional(field("firstMessage")?)?,
        alternate_greetings,
        default_variables: or_empty(field("defaultVariables")?)?,
        global_lore: Vec::new(),
        emotion_images: values(field("emotionImages")?, budget)?,
        additional_assets: values(field("additionalAssets")?, budget)?,
        prebuilt_asset_command: character.truthy("prebuiltAssetCommand", budget)?,
        prebuilt_asset_exclude: values(field("prebuiltAssetExclude")?, budget)?,
        chat: Chat {
            id: chat_id,
            first_message_index,
            note: conversation.get("note", budget)?.map(|note| copy(note, budget)).transpose()?,
            variables,
            unreadable_variables,
            local_variables: strings(conversation.get("GLGlobalVariables", budget)?)?,
            messages,
            local_lore: Vec::new(),
            window_start: None,
        },
    };
    let database = Database {
        character: Some(card),
        persona_prompt,
        main_prompt: optional(setting("mainPrompt")?)?,
        jailbreak: optional(setting("jailbreak")?)?,
        global_note: optional(setting("globalNote")?)?,
        author_notes,
        template_default_variables: or_empty(setting("templateDefaultVariables")?)?,
        global_variables: strings(setting("globalChatVariables")?)?,
        model: nullable(setting("aiModel")?)?,
        sub_model: nullable(setting("subModel")?)?,
        jailbreak_toggle: settings.truthy("jailbreakToggle", budget)?,
        max_context,
        // `getModelInfo(db.aiModel)`, whose flags `enableCustomFlags` replaces.
        model_info: (is_text(setting("aiModel")?, "ollama-hosted") && !settings.truthy("enableCustomFlags", budget)?)
            .then(|| parse_json(OLLAMA_HOSTED, budget)).transpose()?,
        local_time: host.and_then(HostCapture::time_zone).map(|zone| LocalTime { zone, ..LocalTime::default() }),
        // The language and the platform are read only by `{{metadata}}`, which declines.
        ..Database::default()
    };
    let declined = host_state.iter().chain(&UNPROVEN).chain(&unreadable).copied().collect();
    let (tailored_case, case_tables) = host.map_or((true, CaseTables::Foreign), |host| (host.tailored_case, host.case_tables()));
    Ok((Snapshot { user, source: Source::Database(Box::new(database)), tailored_case, case_tables, declined, ..Snapshot::default() },
        unreadable))
}

// The engine's declines as route refusals: a host state the route does not capture by name, local time without a
// capture or outside its zone as `cbs.local-time`, a stored value the called function cannot read as TS reads it as
// `cbs.snapshot`, an unproven registration and any other function or form as `cbs`, and more draws than an admission
// records as `cbs.random-draws`. The model record is always captured, since the route admits one model.
fn refusal(error: StageError, unreadable: &[&'static str]) -> StageError {
    let reason = match &error {
        StageError::Limit(name) if &**name == "cbs-draws" => return StageError::Unsupported(Cow::Borrowed("cbs.random-draws")),
        StageError::Unsupported(reason) => &**reason,
        _ => return error,
    };
    if reason.strip_prefix("declined:").is_some_and(|id| unreadable.iter().any(|name| *name == id)) { return unrepresentable(); }
    match reason {
        "declined:trigger_id" => StageError::Unsupported(Cow::Borrowed("cbs.trigger_id")),
        "declined:screenwidth" => StageError::Unsupported(Cow::Borrowed("cbs.screenwidth")),
        "declined:screenheight" => StageError::Unsupported(Cow::Borrowed("cbs.screenheight")),
        "declined:metadata" => StageError::Unsupported(Cow::Borrowed("cbs.metadata")),
        "local-time" | "time-zone-range" => StageError::Unsupported(Cow::Borrowed("cbs.local-time")),
        "case-locale" => StageError::Unsupported(Cow::Borrowed("cbs.case-locale")),
        "case-table" => StageError::Unsupported(Cow::Borrowed("cbs.case-table")),
        "unreadable-variable" => unrepresentable(),
        _ => StageError::Unsupported(Cow::Borrowed("cbs")),
    }
}

struct RouteText { text: Box<dyn TextProcessor + Send>, unreadable: Vec<&'static str> }
impl TextProcessor for RouteText {
    fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
        self.text.process(input, context, budget).map_err(|error| refusal(error, &self.unreadable))
    }
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        self.text.apply(effect, budget).map_err(|error| refusal(error, &self.unreadable))
    }
}

/// The `Date.now()` reading and the `Math.random()` draws, as IEEE 754 bit patterns, that a send's admission took, so
/// every later run of the send reads the same time and takes the same branches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entropy { pub(super) now: u64, pub(super) draws: Vec<u64> }

impl Entropy {
    /// Whether the run drew `Math.random()` at all.
    pub(crate) fn drew(&self) -> bool { !self.draws.is_empty() }
    /// A fixed `Date.now()` and a `Math.random()` tape, as the TS oracles stub them.
    #[cfg(test)]
    pub(crate) fn pinned(now: f64, draws: &[f64]) -> Self {
        Self { now: now.to_bits(), draws: draws.iter().map(|draw| draw.to_bits()).collect() }
    }
}

/// Where a CBS run reads `Date.now()` and draws `Math.random()`.
pub(crate) enum Draws<'a> {
    /// One clock reading and operating-system draws, which the recorder keeps.
    Record(&'a Recorder),
    /// The reading and the draws an admission recorded; a run that needs more draws fails.
    Replay(&'a Entropy),
    /// The system clock and operating-system draws.
    Fresh,
}

/// The clock reading and the draws of one recording run.
pub(crate) struct Recorder { pub(super) now: f64, pub(super) draws: Arc<Mutex<Vec<f64>>> }

impl Recorder {
    pub(crate) fn new() -> Self { Self { now: System.now_ms(), draws: Arc::default() } }
    pub(crate) fn entropy(&self) -> Entropy {
        let draws = self.draws.lock().unwrap_or_else(PoisonError::into_inner);
        Entropy { now: self.now.to_bits(), draws: draws.iter().map(|draw| draw.to_bits()).collect() }
    }
}

// Operating-system draws, each kept for the recorder.
pub(super) struct Recording(pub(super) Arc<Mutex<Vec<f64>>>);
impl Rng for Recording {
    fn random(&mut self) -> Result<f64, Error> {
        let mut draws = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if draws.len() == MAX_DRAWS { return Err(Error::Limit("cbs-draws")); }
        let draw = OsEntropy.random()?;
        draws.push(draw);
        Ok(draw)
    }
}

/// The engine's `toLocaleLowerCase` the prompt reads the example messages with; without a capture, the one that
/// declines the most.
pub(super) fn engine_case(host: Option<&HostCapture>) -> EngineCase {
    use risunest_prompt::machine::CaseTables;
    host.map_or(EngineCase { tables: CaseTables::Foreign, tailored: true }, |host| EngineCase {
        tables: match host.reproduced() { Reproduced::Native => CaseTables::Native, Reproduced::Unicode16 => CaseTables::Unicode16, Reproduced::Foreign => CaseTables::Foreign },
        tailored: host.tailored_case,
    })
}

/// The CBS stage over [`snapshot`] with the reservation's host capture, reading `Date.now()` and drawing `Math.random()`
/// as `draws` says.
pub(super) fn processor(input: &PromptInput, budget: &Budget, draws: &Draws, host: Option<&HostCapture>)
    -> Result<Box<dyn TextProcessor + Send>, StageError> {
    let (snapshot, unreadable) = snapshot(input, budget, host)?;
    let text: Box<dyn TextProcessor + Send> = match draws {
        Draws::Record(recorder) => {
            Box::new(Processor::with_clock(snapshot, Recording(recorder.draws.clone()), Arc::new(Fixed(recorder.now))))
        }
        Draws::Replay(entropy) => {
            let tape = Tape::new(entropy.draws.iter().map(|draw| f64::from_bits(*draw)).collect());
            Box::new(Processor::with_clock(snapshot, tape, Arc::new(Fixed(f64::from_bits(entropy.now)))))
        }
        Draws::Fresh => Box::new(Processor::new(snapshot, OsEntropy)),
    };
    Ok(Box::new(RouteText { text, unreadable }))
}

pub(super) fn processor_with_runtime(input: &PromptInput, budget: &Budget, rng: super::rng::SharedRng,
    clock: Arc<dyn Clock + Send + Sync>, host: Option<&HostCapture>) -> Result<Box<dyn TextProcessor + Send>, StageError> {
    let (snapshot, unreadable) = snapshot(input, budget, host)?;
    Ok(Box::new(RouteText { text: Box::new(Processor::with_clock(snapshot, rng, clock)), unreadable }))
}

/// The CBS a start trigger evaluates against its own variables (`risuChatParser` inside `runTrigger`): the prompt's
/// snapshot with its declines, drawing from the prompt's `Math.random()` and reading its clock.
pub(super) struct ScopedText { text: Processor<super::rng::SharedRng>, unreadable: Vec<&'static str> }

impl TextProcessor for ScopedText {
    fn process(&mut self, input: &Text, context: &TextContext, budget: &Budget) -> Result<TextOutput, StageError> {
        self.text.process(input, context, budget).map_err(|error| refusal(error, &self.unreadable))
    }
    fn apply(&mut self, effect: &Effect, budget: &Budget) -> Result<(), StageError> {
        self.text.apply(effect, budget).map_err(|error| refusal(error, &self.unreadable))
    }
}

impl ScopedTextProcessor for ScopedText {
    fn process_scoped(&mut self, input: &Text, context: &TextContext, scope: TextScope<'_>, budget: &Budget) -> Result<Text, StageError> {
        self.text.process_scoped(input, context, scope, budget).map_err(|error| match error {
            // A truthy value other than a string, such as a number or object a trigger wrote, reads as a `{text, var}`
            // result: TS throws a TypeError reading its missing `var`, or reads the `var` it holds. The route leaves such
            // a send to TS.
            StageError::Invalid(name) if name == "matcher-result" => StageError::Unsupported(Cow::Borrowed("triggers")),
            StageError::Unsupported(name) if name == "matcher-result-var" => StageError::Unsupported(Cow::Borrowed("triggers")),
            error => refusal(error, &self.unreadable),
        })
    }
}

pub(super) fn scoped_processor(input: &PromptInput, budget: &Budget, rng: super::rng::SharedRng, clock: Arc<dyn Clock + Send + Sync>,
    host: Option<&HostCapture>) -> Result<ScopedText, StageError> {
    let (snapshot, unreadable) = snapshot_declining(input, budget, host, &SCOPED_DECLINED)?;
    Ok(ScopedText { text: Processor::with_clock(snapshot, rng, clock), unreadable })
}
