// A synchronous ir::stage::TextProcessor over the stream parser. The caller captures the reads
// risuChatParser makes outside its input in a Snapshot: either the variable reads alone, or the database
// the host registrations read. Registrations the snapshot cannot serve are declined.
//
// Each process() is one risuChatParser(text, arg) call on an arg object the Snapshot describes. It carries
// the arg fields the admitted registrations and the parser read: consistantChar, tokenizeAccurate,
// callStack and functions, with chatID, role, runVar, rmVar, visualize and cbsConditions from the TextContext,
// and projectedChatID, historyOffset, historyAccess, currentTime and triggerId from the CallArgs.
// `Date.now()` reads the processor's clock.
pub use super::parser::CaseTables;
use super::parser::{self, ArgState, Options};
use super::{families, host, Budget, Context, Units};
use risunest_ir::clock::{Clock, System};
use risunest_ir::rng::Rng;
use risunest_ir::stage::{Effect, ScopedTextProcessor, StageError, TextContext, TextOutput, TextProcessor, TextScope};
use risunest_ir::{Budget as StageBudget, Text, Value};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The values `risuChatParser` reads outside its input, captured before evaluation.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// What `{{user}}` returns: `getUserName()`, or `username` when the caller sets `consistantChar`.
    pub user: Units,
    pub source: Source,
    /// The default locale tailors case mapping (`tr`, `az`, `lt`, ...); `lower`, `upper` and names with `I` decline.
    pub tailored_case: bool,
    /// The Unicode tables of the JS engine's case mapping; the default declines `lower`, `upper` and `capitalize`
    /// as `case-table` for text they would map beyond ASCII.
    pub case_tables: CaseTables,
    /// `arg.consistantChar`: `{{user}}` reads `username` and `{{char}}` reads `botname`.
    pub consistant_char: bool,
    /// `arg.tokenizeAccurate`: `{{random}}` with choices takes the first one, still drawing `Math.random`, and
    /// `{{char}}` reads `bot` when no character is selected.
    pub tokenize_accurate: bool,
    /// `arg.callStack` and `arg.functions` (name to #func body) before each call. risuChatParser counts
    /// every call on one arg object and returns `ERROR: Call stack limit reached` past 20, and once a
    /// `call::` runs it leaves its function map, with the #func definitions made so far, in `arg.functions`.
    pub arg: ArgState,
    /// Registry ids of the registrations whose host state the caller does not capture: a call to one declines as
    /// `declined:<id>` before its callback runs.
    pub declined: Vec<&'static str>,
}

/// Where the variables come from.
#[derive(Debug)]
pub enum Source {
    /// The variable reads alone; every registration that reads the database declines.
    Reads(Reads),
    /// The database the host registrations read.
    Database(Box<Database>),
}
impl Default for Source {
    fn default() -> Self {
        Source::Reads(Reads::default())
    }
}

/// Captured `getChatVar` and `getGlobalChatVar` reads.
#[derive(Debug, Default)]
pub struct Reads {
    /// `getChatVar(key)` for every key that does not read `null`: the stored value, else the first default.
    pub chat_variables: HashMap<Units, Units>,
    /// The default `getChatVar` reads once a stored value is cleared: the character's `defaultVariables`, then the
    /// template defaults, first entry per key.
    pub default_variables: HashMap<Units, Units>,
    /// `getGlobalChatVar(key)` for every key that does not read `null`, including the `toggle_` keys.
    pub global_variables: HashMap<Units, Units>,
}

/// `DBState.db` as the host registrations read it for `selectedCharID`.
#[derive(Debug, Default)]
pub struct Database {
    /// `db.characters[selectedCharID]`, or None when that index holds no character.
    pub character: Option<Character>,
    /// `getPersonaPrompt()`: the prompt of the persona the chat binds, else `db.personaPrompt`.
    pub persona_prompt: Units,
    /// `db.mainPrompt`, `db.jailbreak` and `db.globalNote`; None is undefined.
    pub main_prompt: Option<Units>,
    pub jailbreak: Option<Units>,
    pub global_note: Option<Units>,
    /// The `defaultText` of each `authornote` entry of `db.promptTemplate`, in template order.
    pub author_notes: Vec<Units>,
    /// `db.templateDefaultVariables`: `key=value` lines read after the character's own.
    pub template_default_variables: Units,
    /// `db.globalChatVariables`.
    pub global_variables: HashMap<Units, Units>,
    /// `db.aiModel` and `db.subModel`; None is undefined.
    pub model: Option<Units>,
    pub sub_model: Option<Units>,
    pub jailbreak_toggle: bool,
    pub max_context: f64,
    /// `CurrentTriggerIdStore`; None is null.
    pub trigger_id: Option<Units>,
    /// `window.innerWidth` and `window.innerHeight`.
    pub viewport: (u32, u32),
    /// `getModules()`, or `arg.modules` when the caller sets it: the conversation's modules in selection order.
    pub modules: Vec<Module>,
    /// `getModuleLorebooks()`, or `arg.moduleLorebooks`: the lore entries of those modules in module order.
    pub module_lorebooks: Vec<Value>,
    /// `getModelInfo(db.aiModel)`, the model record as an object; None declines the registrations that read it.
    pub model_info: Option<Value>,
    /// `db.language`; None is undefined.
    pub language: Option<Units>,
    pub platform: Platform,
    /// Date's local time and the default locale's formats; None declines the registrations that read them.
    pub local_time: Option<LocalTime>,
}

/// An entry of `getModules()`.
#[derive(Debug)]
pub struct Module {
    /// `namespace`; `Value::Undefined` when unset.
    pub namespace: Value,
    /// `assets`, each entry `[name, path, ext]`; None is undefined.
    pub assets: Option<Vec<Value>>,
}

/// What `{{metadata}}` reports about the running app. The parser registers `isMobile` as false.
#[derive(Debug, Default)]
pub struct Platform {
    /// `isTauri` of src/ts/platform.ts.
    pub tauri: bool,
    /// `appVer`.
    pub app_version: Units,
    /// `navigator.language`.
    pub browser_language: Units,
}

/// Date's local time zone and the default locale's `toLocaleTimeString()` and `toLocaleDateString()`.
#[derive(Debug, Default, Clone)]
pub struct LocalTime {
    pub zone: TimeZone,
    /// None when the format reads more than the local fields; `{{messagetime}}` then declines.
    pub time_format: Option<LocalePattern>,
    /// None likewise for `{{messagedate}}`.
    pub date_format: Option<LocalePattern>,
}

/// The offsets Date's local getters add to an instant: each `(start, offset)` holds from its start until the next
/// one, in milliseconds, up to `end`. An instant outside `[transitions[0].0, end)` declines.
#[derive(Debug, Default, Clone)]
pub struct TimeZone {
    pub transitions: Vec<(f64, f64)>,
    pub end: f64,
}

/// A locale string filled from the local fields of a Date.
#[derive(Debug, Default, Clone)]
pub struct LocalePattern {
    pub parts: Vec<LocalePart>,
    /// The hour text for each local hour 0 to 23, and the day period text likewise.
    pub hours: Vec<Units>,
    pub day_periods: Vec<Units>,
}

#[derive(Debug, Clone)]
pub enum LocalePart {
    Literal(Units),
    /// The four-digit year.
    Year,
    /// The month 1 to 12, with a leading zero below 10 when `two_digit`.
    Month { two_digit: bool },
    Day { two_digit: bool },
    /// `hours[hour]`.
    Hour,
    Minute { two_digit: bool },
    Second { two_digit: bool },
    /// `day_periods[hour]`.
    DayPeriod,
}

/// The risuChatParser arg fields of one call beyond those of the TextContext.
#[derive(Debug, Default)]
pub struct CallArgs {
    /// `projectedChatID`: read in place of the chat index as the absolute message index; None is undefined.
    pub projected_chat_id: Option<i64>,
    /// `historyOffset`: subtracted with the window start from the index `{{previouschatlog}}` names.
    pub history_offset: Option<i64>,
    /// `historyAccess`: the prepared history the call reads in place of the loaded conversation.
    pub history_access: Option<HistoryAccess>,
    /// `currentTime`: read in place of `Date.now()`; once set, `{{trigger_id}}` reads `trigger_id`.
    pub current_time: Option<f64>,
    /// `triggerId`; None is undefined or null.
    pub trigger_id: Option<Units>,
}

/// A `createCbsHistoryAccess` input: rows outside the prepared ones throw `CbsHistoryAccessError` out of the
/// parse, which declines as `history-unprepared`.
#[derive(Debug, Default)]
pub struct HistoryAccess {
    pub length: usize,
    /// `rows`: absolute index and message.
    pub rows: Vec<(usize, Value)>,
    /// `complete`: every message, when prepared.
    pub complete: Option<Vec<Value>>,
    /// `searches`: the messages `previous(role, before, count)` returns.
    pub searches: Vec<HistorySearch>,
}

#[derive(Debug)]
pub struct HistorySearch {
    pub role: Units,
    pub before: i64,
    pub count: i64,
    pub rows: Vec<Value>,
}

/// A character with its open conversation.
#[derive(Debug, Default)]
pub struct Character {
    /// `chaId`.
    pub id: Units,
    pub name: Units,
    pub nickname: Units,
    /// `personality`, `desc`, `scenario` and `exampleMessage`; None is undefined.
    pub personality: Option<Units>,
    pub description: Option<Units>,
    pub scenario: Option<Units>,
    pub example_message: Option<Units>,
    /// `firstMessage`; None is undefined.
    pub first_message: Option<Units>,
    pub alternate_greetings: Vec<Units>,
    /// `defaultVariables`: `key=value` lines.
    pub default_variables: Units,
    /// `globalLore ?? []`, of `arg.chara` when the caller sets a character there.
    pub global_lore: Vec<Value>,
    /// `emotionImages` and `additionalAssets`, each entry `[name, path, ...]`; None is undefined.
    pub emotion_images: Option<Vec<Value>>,
    pub additional_assets: Option<Vec<Value>>,
    /// `prebuiltAssetCommand`, as its truthiness.
    pub prebuilt_asset_command: bool,
    /// `prebuiltAssetExclude`; None is undefined or null.
    pub prebuilt_asset_exclude: Option<Vec<Value>>,
    /// The conversation the history registrations read: `chats[chatPage]`, or while a send builds from a history
    /// window without a history access, the window's chat that `resolveHistoryWindowChat` returns.
    pub chat: Chat,
}

/// A conversation with every message loaded.
#[derive(Debug, Default)]
pub struct Chat {
    /// `id`; None is undefined.
    pub id: Option<Units>,
    /// `fmIndex`: -1 for `firstMessage`, else the alternate greeting it selects.
    pub first_message_index: i64,
    /// `note` as stored; None is undefined.
    pub note: Option<Value>,
    /// `scriptstate['$' + key].toString()` for every key whose stored value is neither null nor undefined.
    pub variables: HashMap<Units, Units>,
    /// The keys whose stored value `toString()` throws on, which a function reading them declines.
    pub unreadable_variables: HashSet<Units>,
    /// `GLGlobalVariables`.
    pub local_variables: HashMap<Units, Units>,
    /// `message`, each row an object in its stored property order. A row without a string `role` and `data`
    /// declines the registrations that read rows.
    pub messages: Vec<Value>,
    /// `localLore ?? []`.
    pub local_lore: Vec<Value>,
    /// `getHistoryWindowStart(chat)`: the absolute index of `messages[0]`; None for a chat without a window.
    pub window_start: Option<usize>,
}

/// The UTF-16 units of `text`, through the UTF-8 fast path when the text has one.
pub fn text_units(text: &Text) -> Units {
    match text.as_unicode() {
        Some(value) => value.encode_utf16().collect(),
        None => text.units().collect(),
    }
}

/// `units` as a stage `Text`, stored as UTF-8 unless it holds an isolated surrogate.
pub fn units_text(units: &[u16], budget: &StageBudget) -> Result<Text, risunest_ir::Error> {
    match String::from_utf16(units) {
        Ok(value) => Text::from_str(&value, budget),
        Err(_) => Text::from_units(units, budget),
    }
}

/// Evaluates CBS natively against a [`Snapshot`], drawing `Math.random` from `rng`. Every registration the snapshot
/// cannot serve is `StageError::Unsupported` with the engine's reason, and the caller runs the product parser
/// instead; a declined call keeps none of its writes. `setChatVar` calls come back as effects in call order.
pub struct Processor<R> {
    context: Context,
    options: Options,
    next_arg: ArgState,
    last_arg: ArgState,
    rng: R,
}

impl<R: Rng + Send> Processor<R> {
    pub fn new(snapshot: Snapshot, rng: R) -> Self {
        Self::with_clock(snapshot, rng, Arc::new(System))
    }
    /// [`Processor::new`] with `Date.now()` read from `clock`.
    pub fn with_clock(snapshot: Snapshot, rng: R, clock: Arc<dyn Clock + Send + Sync>) -> Self {
        let Snapshot { user, source, tailored_case, case_tables, consistant_char, tokenize_accurate, arg, declined } = snapshot;
        let user = if consistant_char { super::units("username") } else { user };
        let mut context = match source {
            Source::Reads(Reads { chat_variables, default_variables, global_variables }) => Context {
                variables: chat_variables,
                defaults: default_variables,
                globals: global_variables,
                user,
                ..Context::default()
            },
            Source::Database(database) => host::context(*database, user, consistant_char, clock),
        };
        context.declined = declined;
        let options = Options { tailored_case, case_tables, tokenize_accurate, ..Options::default() };
        Self { context, options, last_arg: arg.clone(), next_arg: arg, rng }
    }
    pub fn rng(&self) -> &R {
        &self.rng
    }
    /// `Math.random` calls the processor has made.
    pub fn draws(&self) -> usize {
        self.context.draws
    }
    /// The arg state after the last successful call. A caller that reuses one arg object across calls passes
    /// it to [`Processor::set_arg`] before the next one.
    pub fn arg(&self) -> &ArgState {
        &self.last_arg
    }
    /// The arg state before every following call.
    pub fn set_arg(&mut self, arg: ArgState) {
        self.next_arg = arg;
    }
    /// The [`CallArgs`] of every following call. Only the registrations that read the database read them.
    pub fn set_call(&mut self, call: CallArgs) {
        if let Some(host) = self.context.host.as_deref_mut() {
            host::set_call(host, call);
        }
    }
}

// The engine's reason as a stage error: limits and cancellation keep their meaning, every other reason is a decline.
fn stage_error(reason: String) -> StageError {
    match reason.as_str() {
        "cancelled" => StageError::Cancelled,
        "budget" => StageError::Limit(Cow::Borrowed("work")),
        "output-limit" => StageError::Limit(Cow::Borrowed("text-units")),
        _ => match (reason.strip_prefix("limit:"), reason.strip_prefix("invalid:")) {
            (Some(name), _) => StageError::Limit(Cow::Owned(name.to_owned())),
            (_, Some(name)) => StageError::Invalid(Cow::Owned(name.to_owned())),
            _ => StageError::Unsupported(Cow::Owned(reason)),
        },
    }
}

struct ScopeBridge<'scope, 'budget> {
    scope: TextScope<'scope>,
    budget: &'budget StageBudget,
    failure: Option<StageError>,
}
impl ScopeBridge<'_, '_> {
    fn result<T>(&mut self, result: Result<T, StageError>) -> Result<T, String> {
        result.map_err(|error| {
            self.failure = Some(error);
            "scoped-variable".to_owned()
        })
    }
}
// `Number(value)`: an array or object converts through its string form, and `None` is a conversion that throws.
fn js_number(value: &Value, budget: &StageBudget) -> Result<Option<f64>, StageError> {
    Ok(Some(match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(u8::from(*value)),
        Value::Number(value) => *value,
        other => match other.to_text(budget) {
            Ok(text) => risunest_ir::num::to_number(&text, budget)?,
            Err(risunest_ir::Error::Type { .. }) => return Ok(None),
            Err(error) => return Err(error.into()),
        },
    }))
}
impl families::Scope for ScopeBridge<'_, '_> {
    fn get(&mut self, key: Option<&[u16]>) -> Result<families::ChatVar, String> {
        let result = (|| {
            let key = key.map(|key| units_text(key, self.budget)).transpose()?;
            Ok(match self.scope.variables.get_value(key.as_ref(), self.budget)? {
                Value::Text(text) => families::ChatVar::Text(text_units(&text)),
                other => {
                    let holds_var = matches!(other, Value::Object(_))
                        && other.get(b"var", self.budget)?.is_some_and(|v| !matches!(v, Value::Undefined | Value::Null));
                    families::ChatVar::Other { truthy: other.truthy(), holds_var }
                }
            })
        })();
        self.result(result)
    }
    fn number(&mut self, key: Option<&[u16]>) -> Result<Option<f64>, String> {
        let result = (|| {
            let key = key.map(|key| units_text(key, self.budget)).transpose()?;
            js_number(&self.scope.variables.get_value(key.as_ref(), self.budget)?, self.budget)
        })();
        self.result(result)
    }
    fn set(&mut self, key: Option<&[u16]>, value: Option<&[u16]>) -> Result<(), String> {
        let result = (|| {
            let key = key.map(|key| units_text(key, self.budget)).transpose()?;
            let value = value.map(|value| units_text(value, self.budget)).transpose()?;
            self.scope.variables.set(key.as_ref(), value.as_ref(), self.budget)
        })();
        self.result(result)
    }
    fn trigger_id(&self) -> Option<Units> { self.scope.trigger_id.map(text_units) }
}

impl<R: Rng + Send> Processor<R> {
    fn process_bound(&mut self, input: &Text, context: &TextContext, budget: &StageBudget,
        mut scope: Option<&mut dyn families::Scope>) -> Result<TextOutput, StageError> {
        budget.check()?;
        let source = text_units(input);
        let options = Options { displaying: context.visualize, ..self.options };
        self.context.index = context.chat_index;
        self.context.temp.clear();
        if let Some(host) = self.context.host.as_deref_mut() {
            host.flags = host::Flags {
                run_var: context.run_var,
                rm_var: context.rm_var,
                first_message: context.conditions.first_message == Some(true),
                role: context.role.as_ref().map(text_units),
                chat_role: context.conditions.chat_role.as_ref().map(text_units),
                ..std::mem::take(&mut host.flags)
            };
        }
        let mut local = Budget::shared(budget);
        let prepared = parser::prepare(&source, false, options);
        let mut arg = self.next_arg.clone();
        let scope: Option<&mut dyn families::Scope> = match &mut scope {
            Some(scope) => Some(&mut **scope),
            None => None,
        };
        let evaluated = parser::run_scoped(&prepared, &mut self.context, &mut local, Some(&mut self.rng), &mut arg, scope)
            .and_then(|output| local.settle().map(|()| output))
            .map_err(stage_error)
            .and_then(|output| {
                let text = units_text(&output, budget)?;
                let mut effects = Vec::new();
                for write in host::writes(&self.context) {
                    let value = match &write.value {
                        Some(value) => Value::Text(units_text(value, budget)?),
                        None => Value::Undefined,
                    };
                    effects.push(Effect::ChatVariable { name: units_text(&write.key, budget)?, value });
                }
                Ok(TextOutput { text, effects })
            });
        host::finish(&mut self.context, evaluated.is_ok());
        let output = evaluated?;
        self.last_arg = arg;
        Ok(output)
    }
}

impl<R: Rng + Send> ScopedTextProcessor for Processor<R> {
    fn process_scoped(&mut self, input: &Text, context: &TextContext, scope: TextScope<'_>, budget: &StageBudget)
        -> Result<Text, StageError> {
        let mut bridge = ScopeBridge { scope, budget, failure: None };
        let result = self.process_bound(input, context, budget, Some(&mut bridge));
        match bridge.failure {
            Some(error) => Err(error),
            None => result.map(|output| output.text),
        }
    }
}

impl<R: Rng + Send> TextProcessor for Processor<R> {
    fn process(&mut self, input: &Text, context: &TextContext, budget: &StageBudget) -> Result<TextOutput, StageError> {
        self.process_bound(input, context, budget, None)
    }

    // setChatVar stores the value; a later getChatVar reads its string, or the default once it is null or undefined.
    fn apply(&mut self, effect: &Effect, budget: &StageBudget) -> Result<(), StageError> {
        match effect {
            Effect::ChatVariable { name, value } => {
                let key = text_units(name);
                self.context.unreadable.remove(&key);
                match value {
                    Value::Undefined | Value::Null => {
                        self.context.variables.remove(&key);
                    }
                    other => {
                        let text = other.to_text(budget)?;
                        self.context.variables.insert(key, text_units(&text));
                    }
                }
                Ok(())
            }
            Effect::ChatField { field, value } if field.equals_ascii(b"note") => {
                let host = self.context.host.as_deref_mut().ok_or_else(|| StageError::Unsupported("snapshot".into()))?;
                host::apply_note(host, value, budget)
            }
            // These stored memory fields are not CBS input.
            Effect::ChatField { field, .. } if [b"lastMemory".as_slice(), b"supaMemoryData", b"hypaV3Data", b"hypaV2Data"]
                .iter().any(|name| field.equals_ascii(name)) => Ok(()),
            Effect::Message(edit) => {
                let host = self.context.host.as_deref_mut().ok_or_else(|| StageError::Unsupported("snapshot".into()))?;
                host::apply_message(host, edit, budget)
            }
            _ => Err(StageError::Unsupported(Cow::Borrowed("effect"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::chat::MessageRole;
    use risunest_ir::rng::Tape;
    use risunest_ir::stage::{MessageEdit, TextVariables};
    use risunest_ir::OwnedVec;
    use risunest_ir::Limits;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn limits() -> Limits {
        Limits { max_owned_bytes: 1 << 26, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16, max_depth: 64, max_work: 1 << 24 }
    }
    fn budget(limits: Limits) -> (StageBudget, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        (StageBudget::new(limits, cancel.clone()).unwrap(), cancel)
    }
    fn units(s: &str) -> Units {
        s.encode_utf16().collect()
    }
    fn lossy(text: &Text) -> String { String::from_utf16_lossy(&text_units(text)) }
    fn pairs(pairs: &[(&str, &str)]) -> HashMap<Units, Units> {
        pairs.iter().map(|(k, v)| (units(k), units(v))).collect()
    }
    fn snapshot() -> Snapshot {
        let reads = Reads {
            chat_variables: pairs(&[("score", "7"), ("name", "equal")]),
            default_variables: pairs(&[("score", "1")]),
            global_variables: pairs(&[("toggle_demo", "1")]),
        };
        Snapshot { user: units("Synthetic"), source: Source::Reads(reads), ..Snapshot::default() }
    }
    // The input comes from its own budget so that `budget` bounds only the evaluation and its output.
    fn process(p: &mut Processor<Tape>, text: &str, context: &TextContext, budget: &StageBudget) -> Result<String, StageError> {
        let input = Text::from_str(text, &self::budget(limits()).0).unwrap();
        let output = p.process(&input, context, budget)?;
        assert!(output.effects.is_empty());
        Ok(String::from_utf16(&text_units(&output.text)).unwrap())
    }

    #[test]
    fn reads_the_snapshot_and_the_injected_random_source() {
        let (b, _) = budget(limits());
        let mut p = Processor::new(snapshot(), Tape::new(vec![0.75, 0.1]));
        let context = TextContext { chat_index: 4, ..TextContext::default() };
        let text = "{{user}}/{{chatindex}}/{{getvar::score}}/{{getvar::missing}}/{{#when::toggle::demo}}on{{/when}}/{{random::a::b}}/{{random::a::b}}";
        assert_eq!(process(&mut p, text, &context, &b).unwrap(), "Synthetic/4/7/null/on/b/a");
        assert_eq!((p.draws(), p.rng().consumed()), (2, 2));
        assert_eq!(process(&mut p, "{{random}}", &context, &b), Err(StageError::Limit("rng-tape".into())));
    }

    #[test]
    fn applied_writes_feed_later_reads() {
        let (b, _) = budget(limits());
        let mut p = Processor::new(snapshot(), Tape::new(Vec::new()));
        let context = TextContext::default();
        let set = |name: &str, value: Value| Effect::ChatVariable { name: Text::from_str(name, &b).unwrap(), value };
        p.apply(&set("score", Value::Text(Text::from_str("9", &b).unwrap())), &b).unwrap();
        p.apply(&set("fresh", Value::Number(1.5)), &b).unwrap();
        assert_eq!(process(&mut p, "{{getvar::score}}/{{getvar::fresh}}", &context, &b).unwrap(), "9/1.5");
        p.apply(&set("score", Value::Undefined), &b).unwrap();
        p.apply(&set("fresh", Value::Null), &b).unwrap();
        assert_eq!(process(&mut p, "{{getvar::score}}/{{getvar::fresh}}", &context, &b).unwrap(), "1/null");
    }

    #[test]
    fn host_only_registrations_and_inexact_forms_decline() {
        let (b, _) = budget(limits());
        let mut p = Processor::new(snapshot(), Tape::new(Vec::new()));
        let context = TextContext { run_var: true, ..TextContext::default() };
        for (text, reason) in [
            ("{{char}}", "unsupported-operator"),
            ("{{setvar::x::1}}", "unsupported-operator"),
            ("{{pow::2::3}}", "unsupported-operator"),
            ("{{? 3^40}}", "pow-inexact"),
            ("{{getvar::constructor}}", "variable-prototype-key"),
            ("{{calc::$score+1}}", "calc-variable"),
        ] {
            assert_eq!(process(&mut p, text, &context, &b), Err(StageError::Unsupported(reason.into())), "{text}");
        }
        let mut tailored = Processor::new(Snapshot { tailored_case: true, ..snapshot() }, Tape::new(Vec::new()));
        assert_eq!(process(&mut tailored, "{{lower::I}}", &context, &b), Err(StageError::Unsupported("case-locale".into())));
        assert_eq!(process(&mut tailored, "{{length::abc}}", &context, &b).unwrap(), "3");
        // Under foreign case tables, the default, only ASCII maps, and capitalize maps only its first unit.
        let mut foreign = Processor::new(snapshot(), Tape::new(Vec::new()));
        for (text, expected) in [("{{upper::abc}}", Ok("ABC")), ("{{lower::ABC}}", Ok("abc")), ("{{capitalize::hello}}", Ok("Hello")),
            ("{{capitalize::h\u{10d0}}}", Ok("H\u{10d0}")), ("{{upper::\u{10d0}}}", Err("case-table")), ("{{lower::\u{c9}}}", Err("case-table")),
            ("{{capitalize::\u{10d0}a}}", Err("case-table"))] {
            assert_eq!(process(&mut foreign, text, &context, &b), expected.map(String::from).map_err(|reason| StageError::Unsupported(reason.into())), "{text}");
        }
        // Under Unicode 16 tables text maps beyond ASCII unless it holds a code point Rust's tables map or class
        // differently: U+A7D3 gains a capital in Unicode 17, and U+1ACF is a case-ignorable mark there.
        let mut unicode16 = Processor::new(Snapshot { case_tables: CaseTables::Unicode16, ..snapshot() }, Tape::new(Vec::new()));
        for (text, expected) in [("{{upper::\u{10d0}}}", Ok("\u{1c90}")), ("{{lower::\u{c9}\u{3a3}}}", Ok("\u{e9}\u{3c2}")),
            ("{{capitalize::\u{10d0}a}}", Ok("\u{1c90}a")), ("{{capitalize::\u{a7d3}}}", Err("case-table")),
            ("{{capitalize::a\u{a7d3}}}", Ok("A\u{a7d3}")), ("{{upper::a\u{a7d3}}}", Err("case-table")),
            ("{{lower::\u{391}\u{1acf}\u{3a3}}}", Err("case-table"))] {
            assert_eq!(process(&mut unicode16, text, &context, &b), expected.map(String::from).map_err(|reason| StageError::Unsupported(reason.into())), "{text}");
        }
        let mut native = Processor::new(Snapshot { case_tables: CaseTables::Native, ..snapshot() }, Tape::new(Vec::new()));
        for (text, expected) in [("{{upper::a\u{a7d3}}}", "A\u{a7d2}"), ("{{lower::\u{391}\u{1acf}\u{3a3}}}", "\u{3b1}\u{1acf}\u{3c2}")] {
            assert_eq!(process(&mut native, text, &context, &b).unwrap(), expected, "{text}");
        }
    }

    fn database() -> Snapshot {
        let chat = Chat { variables: pairs(&[("mood", "calm")]), ..Chat::default() };
        let character = Character { id: units("syn"), name: units("Aster"), default_variables: units("level=1"), chat, ..Character::default() };
        let database = Database { character: Some(character), ..Database::default() };
        Snapshot { user: units("Mina"), source: Source::Database(Box::new(database)), ..Snapshot::default() }
    }

    #[test]
    fn database_writes_return_as_effects_and_a_declined_call_keeps_none() {
        let (b, _) = budget(limits());
        let input = |text: &str| Text::from_str(text, &b).unwrap();
        let run = TextContext { run_var: true, ..TextContext::default() };
        let mut p = Processor::new(database(), Tape::new(Vec::new()));
        let output = p.process(&input("{{setvar::mood::sad}}{{addvar::level::2}}{{getvar::mood}}{{getvar::level}}"), &run, &b).unwrap();
        assert_eq!(output.text.as_unicode(), Some("sad3"));
        let effects: Vec<(String, String)> = output
            .effects
            .iter()
            .map(|effect| match effect {
                Effect::ChatVariable { name, value: Value::Text(value) } => (name.as_unicode().unwrap().to_owned(), value.as_unicode().unwrap().to_owned()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(effects, [("mood".to_owned(), "sad".to_owned()), ("level".to_owned(), "3".to_owned())]);
        let declined = p.process(&input("{{setvar::mood::lost}}{{getglobalvar::toString}}"), &run, &b);
        assert_eq!(declined.unwrap_err(), StageError::Unsupported("variable-prototype-key".into()));
        let output = p.process(&input("{{getvar::mood}}{{getvar::level}}"), &TextContext::default(), &b).unwrap();
        assert_eq!((output.text.as_unicode(), output.effects.len()), (Some("sad3"), 0));
    }

    #[test]
    fn a_variable_getchatvar_cannot_read_declines_its_readers_until_a_kept_write_replaces_it() {
        let (b, _) = budget(limits());
        let input = |text: &str| Text::from_str(text, &b).unwrap();
        let run = TextContext { run_var: true, ..TextContext::default() };
        let unreadable = || {
            let mut snapshot = database();
            let Source::Database(database) = &mut snapshot.source else { unreachable!() };
            database.character.as_mut().unwrap().chat.unreadable_variables = [units("odd")].into_iter().collect();
            snapshot
        };
        let declined = || StageError::Unsupported("unreadable-variable".into());
        let mut p = Processor::new(unreadable(), Tape::new(Vec::new()));
        for text in ["{{getvar::odd}}", "{{addvar::odd::1}}", "{{setdefaultvar::odd::d}}", "{{#when::var::odd}}T{{/when}}"] {
            assert_eq!(p.process(&input(text), &run, &b).unwrap_err(), declined(), "{text}");
        }
        assert_eq!(p.process(&input("{{getvar::mood}}{{getvar::level}}"), &run, &b).unwrap().text.as_unicode(), Some("calm1"));
        // A declined call restores the value it replaced, which stays unreadable.
        let failed = p.process(&input("{{setvar::odd::v}}{{getglobalvar::toString}}"), &run, &b);
        assert_eq!(failed.unwrap_err(), StageError::Unsupported("variable-prototype-key".into()));
        assert_eq!(p.process(&input("{{getvar::odd}}"), &run, &b).unwrap_err(), declined());
        assert_eq!(p.process(&input("{{setvar::odd::v}}{{getvar::odd}}"), &run, &b).unwrap().text.as_unicode(), Some("v"));
        assert_eq!(p.process(&input("{{getvar::odd}}"), &run, &b).unwrap().text.as_unicode(), Some("v"));
        let mut applied = Processor::new(unreadable(), Tape::new(Vec::new()));
        applied.apply(&Effect::ChatVariable { name: input("odd"), value: Value::Text(input("w")) }, &b).unwrap();
        assert_eq!(applied.process(&input("{{getvar::odd}}"), &TextContext::default(), &b).unwrap().text.as_unicode(), Some("w"));
    }

    #[test]
    fn a_declined_registration_declines_by_id_under_every_key_and_keeps_no_writes() {
        let (b, _) = budget(limits());
        let input = |text: &str| Text::from_str(text, &b).unwrap();
        let run = TextContext { run_var: true, ..TextContext::default() };
        let mut source = database();
        let Source::Database(db) = &mut source.source else { unreachable!() };
        db.character.as_mut().unwrap().description = Some(units("{{screenwidth}}"));
        let mut p = Processor::new(Snapshot { declined: vec!["screenwidth", "previoususerchat"], ..source }, Tape::new(Vec::new()));
        for (text, id) in [("{{setvar::mood::lost}}{{screenwidth}}", "screenwidth"), ("{{lastusermessage}}", "previoususerchat"),
            ("{{description}}", "screenwidth")] {
            assert_eq!(p.process(&input(text), &run, &b).unwrap_err(), StageError::Unsupported(format!("declined:{id}").into()), "{text}");
        }
        let output = p.process(&input("{{getvar::mood}}/{{screenheight}}"), &TextContext::default(), &b).unwrap();
        assert_eq!((output.text.as_unicode(), output.effects.len()), (Some("calm/0"), 0));
    }

    struct Variables<'a> {
        base: &'a HashMap<Units, Units>,
        // Unwritten keys whose value is not a string, as JSON.
        raw: &'a [(&'a str, &'a str)],
        writes: OwnedVec<(Option<Text>, Option<Text>)>,
        failure: Option<StageError>,
    }
    impl<'a> Variables<'a> {
        fn new(base: &'a HashMap<Units, Units>, budget: &StageBudget) -> Self {
            Self { base, raw: &[], writes: OwnedVec::new(budget).unwrap(), failure: None }
        }
    }
    impl TextVariables for Variables<'_> {
        fn get_value(&mut self, key: Option<&Text>, budget: &StageBudget) -> Result<Value, StageError> {
            let name = key.map(text_units).unwrap_or_else(|| units("undefined"));
            let written = self.writes.as_slice().iter().any(|(key, _)| key.as_ref().map(text_units).as_ref() == Some(&name));
            match self.raw.iter().find(|(key, _)| units(key) == name) {
                Some((_, json)) if !written => Ok(risunest_ir::parse_json(json, budget)?),
                _ => self.get(key, budget).map(Value::Text),
            }
        }
        fn get(&mut self, key: Option<&Text>, budget: &StageBudget) -> Result<Text, StageError> {
            if let Some(error) = self.failure.take() { return Err(error); }
            let key = key.map(text_units).unwrap_or_else(|| units("undefined"));
            for (name, value) in self.writes.as_slice().iter().rev() {
                budget.work(1)?;
                if name.as_ref().map(text_units).unwrap_or_else(|| units("undefined")) == key {
                    return Ok(match value { Some(value) => value.copy(budget)?, None => Text::from_str("null", budget)? });
                }
            }
            Ok(units_text(self.base.get(&key).map(Vec::as_slice).unwrap_or(&units("null")), budget)?)
        }
        fn set(&mut self, key: Option<&Text>, value: Option<&Text>, budget: &StageBudget) -> Result<(), StageError> {
            if let Some(error) = self.failure.take() { return Err(error); }
            let key = key.map(|key| key.copy(budget)).transpose()?;
            let value = value.map(|value| value.copy(budget)).transpose()?;
            self.writes.push((key, value))?;
            Ok(())
        }
    }

    #[test]
    fn scoped_variables_reach_writers_conditions_functions_and_host_reparse() {
        let (b, _) = budget(limits());
        let mut source = database();
        let Source::Database(db) = &mut source.source else { unreachable!() };
        db.character.as_mut().unwrap().description = Some(units("D{{setvar::inside::yes}}{{getvar::mood}}/{{triggerid}}"));
        db.trigger_id = Some(units("outer"));
        let mut p = Processor::new(source, Tape::new(Vec::new()));
        let base = pairs(&[("mood", "bound"), ("level", "40")]);
        let mut vars = Variables::new(&base, &b);
        let id = Text::from_str("inner", &b).unwrap();
        let input = Text::from_str(concat!("{{setvar::mood::local}}{{addvar::level::2}}{{setdefaultvar::new::x}}",
            "{{#when::mood::vis::local}}Y{{:else}}N{{/when}}|{{getvar::level}}|{{description}}|",
            "{{#func f}}{{getvar::new}}/{{triggerid}}{{/func}}{{call::f}}"), &b).unwrap();
        let run = TextContext { run_var: true, ..TextContext::default() };
        let output = (&mut p as &mut dyn ScopedTextProcessor).process_scoped(&input, &run,
            TextScope { variables: &mut vars, trigger_id: Some(&id) }, &b).unwrap();
        assert_eq!(output.as_unicode(), Some("Y|42|Dlocal/inner|x/inner"));
        assert_eq!(vars.writes.as_slice().len(), 4);
        assert_eq!(base.get(&units("mood")), Some(&units("bound")));
        assert_eq!(process(&mut p, "{{getvar::mood}}|{{getvar::inside}}|{{triggerid}}", &TextContext::default(), &b).unwrap(), "calm|null|outer");
        let output = p.process_scoped(&Text::from_str("{{triggerid}}", &b).unwrap(), &run,
            TextScope { variables: &mut vars, trigger_id: None }, &b).unwrap();
        assert_eq!(output.as_unicode(), Some("null"));
        let empty = pairs(&[("mood", "")]);
        let mut vars = Variables::new(&empty, &b);
        let output = p.process_scoped(&Text::from_str("{{#when::var::mood}}stored{{:else}}empty{{/when}}", &b).unwrap(), &run,
            TextScope { variables: &mut vars, trigger_id: None }, &b).unwrap();
        assert_eq!(output.as_unicode(), Some("empty"));
    }

    // Each reader of `getChatVar` converts a value the scope keeps as written its own way.
    #[test]
    fn scoped_non_string_values_follow_each_reader() {
        let (b, _) = budget(limits());
        let base = pairs(&[]);
        let raw = [("one", "1"), ("zero", "0"), ("yes", "true"), ("no", "false"), ("pair", "[3,4]"), ("single", "[5]"), ("obj", r#"{"k":"v"}"#),
            ("forced", r#"{"text":"t","var":{"x":"1"}}"#), ("unforced", r#"{"text":"t","var":null}"#), ("odd", r#"{"toString":"x"}"#)];
        let mut p = Processor::new(database(), Tape::new(Vec::new()));
        let t = |s| Text::from_str(s, &b).unwrap();
        p.apply(&Effect::ChatField { field: t("note"), value: Value::Text(t("{{getvar::one}}")) }, &b).unwrap();
        let run = TextContext { run_var: true, ..TextContext::default() };
        let mut eval = |source: &str| {
            let mut vars = Variables { raw: &raw, ..Variables::new(&base, &b) };
            let output = p.process_scoped(&Text::from_str(source, &b).unwrap(), &run, TextScope { variables: &mut vars, trigger_id: None }, &b);
            let writes = vars.writes.as_slice().iter()
                .map(|(key, value)| format!("{}={}", key.as_ref().map_or(String::new(), lossy), value.as_ref().map_or(String::new(), lossy)))
                .collect::<Vec<_>>();
            (output.map(|text| lossy(&text)), writes.join(" "))
        };
        // `Number(value)`: an array through its string form, a boolean as 0 or 1.
        let (output, writes) = eval("{{addvar::one::1}}{{addvar::yes::1}}{{addvar::pair::1}}{{addvar::single::1}}{{addvar::obj::1}}");
        assert_eq!((output.unwrap().as_str(), writes.as_str()), ("", "one=2 yes=2 pair=NaN single=6 obj=NaN"));
        // Only `addvar` converts with `Number(value)`, which throws for an own `toString` inside the matcher's `try`.
        let (output, writes) = eval("{{addvar::odd::1}}{{setdefaultvar::odd::d}}{{setvar::after::1}}");
        assert_eq!((output.unwrap().as_str(), writes.as_str()), ("{{addvar::odd::1}}", "after=1"));
        // `!currentValue`.
        let (output, writes) = eval("{{setdefaultvar::zero::d}}{{setdefaultvar::no::d}}{{setdefaultvar::one::d}}{{setdefaultvar::pair::d}}");
        assert_eq!((output.unwrap().as_str(), writes.as_str()), ("", "zero=d no=d"));
        let (output, _) = eval(concat!("{{#when::var::one}}T{{:else}}F{{/when}}{{#when::one::vis::1}}T{{:else}}F{{/when}}",
            "{{#when::one::visnot::1}}T{{:else}}F{{/when}}[{{getvar::zero}}][{{getvar::no}}]"));
        assert_eq!(output.unwrap(), "FFT[{{getvar::zero}}][{{getvar::no}}]");
        assert_eq!(eval("{{getvar::one}}").0, Err(StageError::Invalid("matcher-result".into())));
        // The result is read as `{text, var}`: a `var` that holds a value declines, and a null one throws as well.
        assert_eq!(eval("{{getvar::forced}}").0, Err(StageError::Unsupported("matcher-result-var".into())));
        assert_eq!(eval("{{getvar::unforced}}").0, Err(StageError::Invalid("matcher-result".into())));
        // A callback that reparses runs inside the matcher's `try`, which keeps its tag for the same throw.
        let (output, writes) = eval("{{authornote}}{{setvar::after::1}}");
        assert_eq!((output.unwrap().as_str(), writes.as_str()), ("{{authornote}}", "after=1"));
    }

    #[test]
    fn scoped_arguments_and_write_flags_keep_undefined_and_utf16() {
        let (b, _) = budget(limits());
        let base = [(units("s"), vec![0xd800])].into_iter().collect();
        let mut vars = Variables::new(&base, &b);
        let mut p = Processor::new(database(), Tape::new(Vec::new()));
        let input = Text::from_str("{{setvar}}{{setvar::}}{{setvar::x}}{{getvar::s}}", &b).unwrap();
        let run = TextContext { run_var: true, ..TextContext::default() };
        let output = p.process_scoped(&input, &run, TextScope { variables: &mut vars, trigger_id: None }, &b).unwrap();
        assert_eq!(text_units(&output), [0xd800]);
        assert!(vars.writes.as_slice()[0].0.is_none());
        assert_eq!(vars.writes.as_slice()[1].0.as_ref().unwrap().as_unicode(), Some(""));
        assert!(vars.writes.as_slice().iter().all(|(_, value)| value.is_none()));
        for (run_var, rm_var, expected) in [(false, false, "{{setvar::x::value}}"), (true, true, "")] {
            let context = TextContext { run_var, rm_var, ..TextContext::default() };
            let input = Text::from_str("{{setvar::x::value}}", &b).unwrap();
            let output = p.process_scoped(&input, &context, TextScope { variables: &mut vars, trigger_id: None }, &b).unwrap();
            assert_eq!(output.as_unicode(), Some(expected));
        }
        assert_eq!(vars.writes.as_slice().len(), 3);
    }

    #[test]
    fn scoped_errors_keep_the_original_category_and_no_processor_writes() {
        let (b, _) = budget(limits());
        let base = pairs(&[]);
        let mut p = Processor::new(database(), Tape::new(Vec::new()));
        let run = TextContext { run_var: true, ..TextContext::default() };
        for source in ["{{getvar::x}}", "{{#when::var::x}}yes{{/when}}", "{{setvar::x::v}}"] {
            for error in [StageError::Host(vec![0xd800]), StageError::Cancelled, StageError::Limit("scope".into()),
                StageError::Invalid("scope".into()), StageError::Unsupported("scope".into())] {
                let mut vars = Variables::new(&base, &b);
                vars.failure = Some(error.clone());
                let output = p.process_scoped(&Text::from_str(source, &b).unwrap(), &run,
                    TextScope { variables: &mut vars, trigger_id: None }, &b);
                assert_eq!(output.unwrap_err(), error);
                assert!(vars.writes.as_slice().is_empty());
            }
        }
        let mut vars = Variables::new(&base, &b);
        let result = p.process_scoped(&Text::from_str("{{setvar::mood::private}}{{getglobalvar::toString}}", &b).unwrap(), &run,
            TextScope { variables: &mut vars, trigger_id: None }, &b);
        assert_eq!(result.unwrap_err(), StageError::Unsupported("variable-prototype-key".into()));
        assert_eq!(vars.writes.as_slice().len(), 1);
        drop(vars); // The caller discards this failed invocation's private overlay.
        assert_eq!(process(&mut p, "{{getvar::mood}}", &TextContext::default(), &b).unwrap(), "calm");
    }

    fn history_snapshot(b: &StageBudget) -> Snapshot {
        let mut source = database();
        let Source::Database(db) = &mut source.source else { unreachable!() };
        db.author_notes = vec![units("fallback")];
        db.character.as_mut().unwrap().first_message = Some(units("greeting"));
        db.character.as_mut().unwrap().chat.first_message_index = -1;
        db.character.as_mut().unwrap().chat.messages = [
            r#"{"role":"user","data":"first","chatId":"old","extra":{"x":[1,true]}}"#,
            r#"{"role":"char","data":"second","time":1000}"#,
        ].into_iter().map(|s| risunest_ir::parse_json(s, b).unwrap()).collect();
        source
    }

    #[test]
    fn applied_history_and_note_changes_feed_later_cbs_reads() {
        let (b, _) = budget(limits());
        let mut p = Processor::new(history_snapshot(&b), Tape::new(Vec::new()));
        let t = |s| Text::from_str(s, &b).unwrap();
        p.apply(&Effect::Message(MessageEdit::SetData { index: -2.8, data: Value::Text(t("edited {{getvar::mood}}")) }), &b).unwrap();
        let history = process(&mut p, "{{history}}", &TextContext::default(), &b).unwrap();
        let history = risunest_ir::parse_json(&history, &b).unwrap();
        let rows = history.array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].to_text(&b).unwrap().as_unicode(),
            Some(r#"{"role":"user","data":"edited calm","chatId":"old","extra":{"x":[1,true]}}"#));
        p.apply(&Effect::Message(MessageEdit::Append { role: MessageRole::User, data: Value::Text(t("third")), chat_id: t("new") }), &b).unwrap();
        assert_eq!(process(&mut p, "{{lastmessageid}}|{{lastmessage}}", &TextContext::default(), &b).unwrap(), "2|third");
        p.apply(&Effect::Message(MessageEdit::Cut { start: 1.5, end: Some(-1.0) }), &b).unwrap();
        assert_eq!(process(&mut p, "{{lastmessageid}}|{{previouschatlog::0}}", &TextContext::default(), &b).unwrap(), "0|second");
        p.apply(&Effect::ChatField { field: t("note"), value: Value::Text(t("N{{getvar::mood}}")) }, &b).unwrap();
        assert_eq!(process(&mut p, "{{authornote}}", &TextContext::default(), &b).unwrap(), "Ncalm");
        p.apply(&Effect::ChatField { field: t("note"), value: Value::Text(t("")) }, &b).unwrap();
        assert_eq!(process(&mut p, "{{authornote}}", &TextContext::default(), &b).unwrap(), "fallback");
        // A falsy non-string falls through as well, and a truthy one keeps the tag.
        p.apply(&Effect::ChatField { field: t("note"), value: Value::Number(0.0) }, &b).unwrap();
        assert_eq!(process(&mut p, "{{authornote}}", &TextContext::default(), &b).unwrap(), "fallback");
        for note in [Value::Number(5.0), risunest_ir::parse_json(r#"{"k":"v"}"#, &b).unwrap()] {
            p.apply(&Effect::ChatField { field: t("note"), value: note }, &b).unwrap();
            assert_eq!(process(&mut p, "{{authornote}}|{{getvar::mood}}", &TextContext::default(), &b).unwrap(), "{{authornote}}|calm");
        }
        p.set_call(CallArgs { projected_chat_id: Some(1), history_offset: Some(4), ..CallArgs::default() });
        assert_eq!(process(&mut p, "{{previouschatlog::4}}|{{role}}", &TextContext { chat_index: 0, ..TextContext::default() }, &b).unwrap(),
            "second|{{role}}");
    }

    // Message data need not be a string: each reader converts a loaded or edited value as the product does.
    #[test]
    fn message_data_other_than_a_string_follows_each_reader() {
        let (b, _) = budget(limits());
        let json = |s: &str| risunest_ir::parse_json(s, &b).unwrap();
        let mut source = history_snapshot(&b);
        let Source::Database(db) = &mut source.source else { unreachable!() };
        db.character.as_mut().unwrap().chat.messages.push(json(r#"{"role":"char","data":[1,null,[2.5,true]]}"#));
        let mut p = Processor::new(source, Tape::new(Vec::new()));
        let eval = |p: &mut Processor<Tape>, s: &str| process(p, s, &TextContext::default(), &b);
        let roles = |last: &str| format!(r#"["user: first","char: second",{last}]"#);
        let readers = ["{{lastmessage}}", "{{previouschatlog::2}}", "{{previouscharchat}}"];
        // `history` with arguments writes `'' + data`; the readers that reparse `data` keep their tag.
        assert_eq!(eval(&mut p, "{{history::role}}").unwrap(), roles(r#""char: 1,,2.5,true""#));
        assert_eq!(eval(&mut p, "{{history::x}}").unwrap(), r#"["first","second","1,,2.5,true"]"#);
        assert_eq!(eval(&mut p, "{{history}}|{{charhistory}}").unwrap(), "{{history}}|{{charhistory}}");
        assert_eq!(eval(&mut p, "{{userhistory}}").unwrap(),
            r#"["{\"role\":\"user\",\"data\":\"first\",\"chatId\":\"old\",\"extra\":{\"x\":[1,true]}}"]"#);
        let set = |p: &mut Processor<Tape>, data: &str| {
            p.apply(&Effect::Message(MessageEdit::SetData { index: 2.0, data: json(data) }), &b).unwrap();
        };
        // The callbacks that return `data` keep the tag for a falsy value, except that `?? 'Out of range'` reads null.
        for (data, log) in [("0", "{{previouschatlog::2}}"), ("false", "{{previouschatlog::2}}"), ("null", "Out of range")] {
            set(&mut p, data);
            assert_eq!(eval(&mut p, &readers.join("|")).unwrap(), [readers[0], log, readers[2]].join("|"));
            assert_eq!(eval(&mut p, "{{history::role}}").unwrap(), roles(&format!(r#""char: {data}""#)));
        }
        // A truthy value other than a string throws out of the parse.
        for (data, text) in [("-1.5e-7", "-1.5e-7"), ("1e21", "1e+21"), ("true", "true"), ("[]", ""), (r#"{"text":"t","var":null}"#, "")] {
            set(&mut p, data);
            for reader in readers {
                assert_eq!(eval(&mut p, reader), Err(StageError::Invalid("matcher-result".into())), "{data} {reader}");
            }
            if !data.starts_with('{') {
                assert_eq!(eval(&mut p, "{{history::role}}").unwrap(), roles(&format!(r#""char: {text}""#)));
            }
        }
        // An object whose `var` holds a value declines, and so does ToString of an object.
        set(&mut p, r#"{"text":"t","var":{"x":"1"}}"#);
        for reader in readers {
            assert_eq!(eval(&mut p, reader), Err(StageError::Unsupported("matcher-result-var".into())));
        }
        for data in [r#"{"text":"t","var":{"x":"1"}}"#, r#"{"k":"v"}"#, r#"[1,[{"k":"v"}]]"#] {
            set(&mut p, data);
            for reader in ["{{history::role}}", "{{history::x}}"] {
                assert_eq!(eval(&mut p, reader), Err(StageError::Unsupported("message-data-object".into())));
            }
        }
        let t = |s| Text::from_str(s, &b).unwrap();
        let append = |data| Effect::Message(MessageEdit::Append { role: MessageRole::User, data, chat_id: t("new") });
        p.apply(&append(Value::Number(0.0)), &b).unwrap();
        let context = TextContext { chat_index: 4, ..TextContext::default() };
        assert_eq!(process(&mut p, "{{previoususerchat}}|{{lastmessage}}|{{previouschatlog::3}}", &context, &b).unwrap(),
            "{{previoususerchat}}|{{lastmessage}}|{{previouschatlog::3}}");
        p.apply(&Effect::Message(MessageEdit::Cut { start: 0.0, end: Some(3.0) }), &b).unwrap();
        p.apply(&append(Value::Bool(true)), &b).unwrap();
        assert_eq!(process(&mut p, "{{previoususerchat}}", &context, &b), Err(StageError::Invalid("matcher-result".into())));
        set(&mut p, "5");
        assert_eq!(eval(&mut p, "{{history::role}}").unwrap(), r#"["user: first","char: second","char: 5","user: true"]"#);
        // `data = undefined` keeps a key that JSON rows cannot hold.
        for edit in [append(Value::Undefined), Effect::Message(MessageEdit::SetData { index: 0.0, data: Value::Undefined })] {
            assert_eq!(p.apply(&edit, &b), Err(StageError::Unsupported("message-undefined".into())));
        }
        assert_eq!(eval(&mut p, "{{lastmessageid}}|{{previouschatlog::0}}").unwrap(), "3|first");
    }

    #[test]
    fn prepared_history_edits_drop_stale_rows_and_searches() {
        let (b, _) = budget(limits());
        let mut source = history_snapshot(&b);
        let Source::Database(db) = &mut source.source else { unreachable!() };
        db.character.as_mut().unwrap().chat.window_start = Some(8);
        let mut p = Processor::new(source, Tape::new(Vec::new()));
        let row = |s: &str| risunest_ir::parse_json(&format!(r#"{{"role":"user","data":"{s}"}}"#), &b).unwrap();
        p.set_call(CallArgs { history_access: Some(HistoryAccess { length: 1, rows: vec![(0, row("prepared"))],
            complete: Some(vec![row("complete")]), searches: vec![HistorySearch { role: units("user"), before: 1, count: 1,
                rows: vec![row("stale")] }] }), ..CallArgs::default() });
        p.apply(&Effect::Message(MessageEdit::SetData { index: f64::NAN, data: Value::Text(Text::from_str("new", &b).unwrap()) }), &b).unwrap();
        let context = TextContext { chat_index: 1, ..TextContext::default() };
        assert_eq!(process(&mut p, "{{previoususerchat}}|{{previouschatlog::0}}", &context, &b).unwrap(), "new|new");
        p.set_call(CallArgs::default());
        assert_eq!(process(&mut p, "{{lastmessage}}", &context, &b).unwrap(), "new");
    }

    #[test]
    fn failed_or_unsupported_view_updates_leave_the_last_view_intact() {
        let (b, _) = budget(limits());
        let mut p = Processor::new(history_snapshot(&b), Tape::new(Vec::new()));
        let t = |s| Text::from_str(s, &b).unwrap();
        let (small, _) = budget(Limits { max_owned_bytes: StageBudget::context_bytes(), ..limits() });
        let edit = Effect::Message(MessageEdit::SetData { index: 0.0, data: Value::Text(t("replacement")) });
        assert_eq!(p.apply(&edit, &small), Err(StageError::Limit("owned-bytes".into())));
        assert_eq!(process(&mut p, "{{previouschatlog::0}}", &TextContext::default(), &b).unwrap(), "first");
        let replaced = |json: &str| Effect::Message(MessageEdit::Replace { messages: risunest_ir::parse_json(json, &b).unwrap() });
        assert_eq!(p.apply(&replaced("{}"), &b), Err(StageError::Invalid("message-replace".into())));
        assert_eq!(process(&mut p, "{{previouschatlog::0}}", &TextContext::default(), &b).unwrap(), "first");
        p.set_call(CallArgs { history_access: Some(HistoryAccess { length: 2, rows: Vec::new(), complete: None, searches: Vec::new() }),
            ..CallArgs::default() });
        assert_eq!(p.apply(&edit, &b), Err(StageError::Unsupported("history-unprepared".into())));

        let mut source = history_snapshot(&b);
        let Source::Database(db) = &mut source.source else { unreachable!() };
        db.character.as_mut().unwrap().chat.window_start = Some(8);
        let mut windowed = Processor::new(source, Tape::new(Vec::new()));
        assert_eq!(windowed.apply(&edit, &b), Err(StageError::Unsupported("history-window".into())));
        assert_eq!(process(&mut windowed, "{{previouschatlog::8}}", &TextContext::default(), &b).unwrap(), "first");
        windowed.set_call(CallArgs { history_access: Some(HistoryAccess { length: 10, rows: Vec::new(), complete: None,
            searches: Vec::new() }), ..CallArgs::default() });
        assert_eq!(windowed.apply(&edit, &b), Err(StageError::Unsupported("history-unprepared".into())));

        let mut p = Processor::new(history_snapshot(&b), Tape::new(Vec::new()));
        p.set_call(CallArgs { trigger_id: Some(vec![65; 8192]), current_time: Some(0.0), ..CallArgs::default() });
        let cut = Effect::Message(MessageEdit::Cut { start: 0.0, end: Some(0.0) });
        // Cutting every row still copies the captured invocation ID; a refusal must keep both old views.
        for (limits, error) in [
            (Limits { max_owned_bytes: StageBudget::context_bytes() + 1024, ..limits() }, "owned-bytes"),
            (Limits { max_work: 16, ..limits() }, "work"),
        ] {
            let (small, _) = budget(limits);
            assert_eq!(p.apply(&cut, &small), Err(StageError::Limit(error.into())));
            assert_eq!(small.owned_bytes(), StageBudget::context_bytes());
            assert_eq!(process(&mut p, "{{previouschatlog::0}}|{{lastmessageid}}", &TextContext::default(), &b).unwrap(), "first|1");
        }
        p.apply(&cut, &b).unwrap();
        assert_eq!(process(&mut p, "{{lastmessageid}}", &TextContext::default(), &b).unwrap(), "-1");

        // An assigned message that is not a regular one leaves the count readable and declines the rows, as a loaded one.
        let mut p = Processor::new(history_snapshot(&b), Tape::new(Vec::new()));
        p.apply(&replaced(r#"[{"role":5,"data":"x"},{"role":"user","data":"y"}]"#), &b).unwrap();
        assert_eq!(process(&mut p, "{{lastmessageid}}", &TextContext::default(), &b).unwrap(), "1");
        assert_eq!(process(&mut p, "{{previouschatlog::1}}", &TextContext::default(), &b), Err(StageError::Unsupported("history-row".into())));
        assert_eq!(p.apply(&cut, &b), Err(StageError::Unsupported("history-row".into())));
    }

    #[test]
    fn stage_budget_bounds_work_text_and_cancellation() {
        let text = "{{length::abc}}".repeat(400);
        let (b, _) = budget(limits());
        let mut p = Processor::new(snapshot(), Tape::new(Vec::new()));
        assert_eq!(process(&mut p, &text, &TextContext::default(), &b).unwrap(), "3".repeat(400));
        let (small, _) = budget(Limits { max_work: 600, ..limits() });
        assert_eq!(process(&mut p, &text, &TextContext::default(), &small), Err(StageError::Limit("work".into())));
        // A skipped body has no output to convert, so only the evaluation charges the budget.
        let skipped = format!("{{{{#if 0}}}}{text}{{{{/if}}}}");
        assert_eq!(process(&mut p, &skipped, &TextContext::default(), &b).unwrap(), "");
        let (small, _) = budget(Limits { max_work: 600, ..limits() });
        assert_eq!(process(&mut p, &skipped, &TextContext::default(), &small), Err(StageError::Limit("work".into())));
        let (narrow, _) = budget(Limits { max_text_units: 35, ..limits() });
        assert_eq!(process(&mut p, "{{replace::aaaa::a::aaaaaaaa}}", &TextContext::default(), &narrow).unwrap(), "a".repeat(32));
        assert_eq!(process(&mut p, "{{replace::aaaa::a::aaaaaaaaa}}", &TextContext::default(), &narrow), Err(StageError::Limit("text-units".into())));
        let (cancelled, cancel) = budget(limits());
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(process(&mut p, "plain", &TextContext::default(), &cancelled), Err(StageError::Cancelled));
    }

    #[test]
    fn arg_fields_follow_the_product_parser() {
        let (b, _) = budget(limits());
        let context = TextContext::default();
        let mut p = Processor::new(Snapshot { consistant_char: true, tokenize_accurate: true, ..snapshot() }, Tape::new(vec![0.9, 0.9, 0.9]));
        assert_eq!(process(&mut p, "{{user}}/{{random::a::b}}/{{random::x,y}}/{{random::[1,2]}}", &context, &b).unwrap(), "username/a/x/1");
        assert_eq!(p.rng().consumed(), 3);

        let arg = |call_stack| ArgState { call_stack, functions: None };
        let mut p = Processor::new(Snapshot { arg: arg(19), ..snapshot() }, Tape::new(Vec::new()));
        assert_eq!(process(&mut p, "{{user}}", &context, &b).unwrap(), "Synthetic");
        assert_eq!(p.arg(), &arg(20));
        assert_eq!(process(&mut p, "plain", &context, &b).unwrap(), "plain");
        p.set_arg(p.arg().clone());
        assert_eq!(process(&mut p, "{{user}}", &context, &b).unwrap(), "ERROR: Call stack limit reached");
        assert_eq!(p.arg(), &arg(21));

        // A fresh arg keeps #func definitions only once a call:: has run.
        let mut p = Processor::new(snapshot(), Tape::new(Vec::new()));
        assert_eq!(process(&mut p, "{{#func g}}G{{/func}}", &context, &b).unwrap(), "");
        assert_eq!(p.arg(), &arg(1));
        assert_eq!(process(&mut p, "{{#func f}}[{{arg::1}}]{{/func}}{{call::f::x}}", &context, &b).unwrap(), "[x]");
        let reused = p.arg().clone();
        assert_eq!(reused, ArgState { call_stack: 2, functions: Some([(units("f"), units("[{{arg::1}}]"))].into_iter().collect()) });
        assert_eq!(process(&mut p, "{{call::f::y}}", &context, &b).unwrap(), "{{call::f::y}}");
        p.set_arg(reused);
        assert_eq!(process(&mut p, "{{call::f::y}}", &context, &b).unwrap(), "[y]");
        assert_eq!(p.arg().call_stack, 4);
    }

    #[test]
    fn isolated_surrogates_round_trip_through_text() {
        let (b, _) = budget(limits());
        let reads = Reads { chat_variables: [(units("s"), vec![0xd800])].into_iter().collect(), ..Reads::default() };
        let mut p = Processor::new(Snapshot { source: Source::Reads(reads), ..snapshot() }, Tape::new(Vec::new()));
        let output = p.process(&Text::from_str("{{getvar::s}}x", &b).unwrap(), &TextContext::default(), &b).unwrap();
        assert!(output.text.as_unicode().is_none());
        assert_eq!(text_units(&output.text), vec![0xd800, 120]);
        let output = p.process(&Text::from_str("{{reverse::ab}}", &b).unwrap(), &TextContext::default(), &b).unwrap();
        assert_eq!(output.text.as_unicode(), Some("ba"));
    }
}
