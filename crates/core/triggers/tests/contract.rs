//! The stage contract around declines, failures and host requests, over the oracle's base records.

mod support;

use risunest_cbs_experimental::cbs::processor::CaseTables;
use risunest_ir::budget::Budget;
use risunest_ir::chat::MessageRole;
use risunest_ir::json::parse_json;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, OutputShape, ProviderOutput, ResponseKind};
use risunest_ir::rng::Tape;
use risunest_ir::stage::{
    Effect, HostReply, HostRequest, MessageEdit, StageError, Step, TokenCounter, TokenInput, TriggerMode, TriggerRunner,
};
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_records::modules::{ModuleSources, SelectedModules};
use risunest_regex::engine::JsPatternEngine;
use risunest_tokenizer::counter::{ChatAccounting, Counter};
use risunest_tokenizer::id::TokenizerId;
use risunest_triggers::input::{MessageIds, RandomIds, TriggerInput};
use risunest_triggers::runner::{admit, reaches_counts, Dependencies, TriggerRun};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use support::*;

/// Runs `effects` as one trigger of `kind` under `mode`, the character's `lowLevelAccess` and `scriptstate` as given.
fn run_effects(effects: &str, scriptstate: &str, low_level: bool, tailored: bool, mode: TriggerMode, manual: Option<&str>) -> Driven {
    let budget = budget();
    let kind = if manual.is_some() { "manual" } else { "start" };
    let comment = manual.unwrap_or("t");
    let database = case_database(&format!(r#"{{"character":{{"lowLevelAccess":{low_level},"triggerscript":[
        {{"comment":"{comment}","type":"{kind}","conditions":[],"effect":[{effects}]}}]}},
        "chat":{{"scriptstate":{scriptstate}}}}}"#), &budget);
    let character = character(&database, &budget);
    let mut input = TriggerInput::from_records(&database, character, conversation(character, &budget), &budget).unwrap();
    input.tailored_case = tailored;
    let tape = SharedTape::new(Vec::new());
    let mut run = runner(&database, input, &tape, &budget);
    drive(&mut run, request(mode, manual, None, &budget), &budget)
}

fn start(effects: &str, scriptstate: &str) -> Driven { run_effects(effects, scriptstate, false, false, TriggerMode::Start, None) }

fn kinds(driven: &Driven) -> Vec<&'static str> { driven.effects.iter().map(kind).collect() }

const SET_A: &str = r#"{"type":"v2SetVar","operator":"=","var":"a","valueType":"value","value":"1","indent":0}"#;
const MAKE_PARSED: &str = r#"{"type":"v2MakeArrayVar","var":"{{getvar::n}}","indent":0}"#;
const IMPERSONATE: &str = r#"{"type":"v2Impersonate","role":"user","valueType":"value","value":"hi","indent":0}"#;
const SET_NOTE: &str = r#"{"type":"v2SetAuthorNote","valueType":"value","value":"new note","indent":0}"#;

#[test]
fn a_decline_the_run_itself_reaches_before_a_host_request_drops_its_effects() {
    // `v2MakeArrayVar` returns from `runTrigger` when its parsed name is bracketed.
    let state = r#"{"$n":"[x]"}"#;
    let declined = start(MAKE_PARSED, state);
    assert_eq!(declined.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("trigger-early-return")));
    assert!(declined.effects.is_empty() && declined.tokenizer.is_empty());

    let log = r#"{"type":"v2ConsoleLog","sourceType":"value","source":"x","indent":0}"#;
    let after_effects = start(&format!("{SET_A},{IMPERSONATE},{SET_NOTE},{log},{MAKE_PARSED}"), state);
    assert_eq!(after_effects.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("trigger-early-return")));
    assert!(after_effects.effects.is_empty() && after_effects.tokenizer.is_empty() && after_effects.console.is_empty());
}

#[test]
fn a_message_list_that_is_not_an_array_declines_in_admission() {
    // No oracle case records this: an active session refuses such a conversation before any trigger runs.
    let budget = budget();
    let database = case_database(&format!(r#"{{"character":{{"triggerscript":[{{"comment":"t","type":"start","conditions":[],"effect":[{SET_A}]}}]}},
        "chat":{{"message":{{}}}}}}"#), &budget);
    let character = character(&database, &budget);
    let input = TriggerInput::from_records(&database, character, conversation(character, &budget), &budget).unwrap();
    let declined = StageError::Unsupported(Cow::Borrowed("trigger-conversation-shape"));
    assert_eq!(admit(&input, &request(TriggerMode::Start, None, None, &budget), Dependencies::default(), &budget).unwrap_err(), declined);
    let tape = SharedTape::new(Vec::new());
    let mut run = runner(&database, input, &tape, &budget);
    let driven = drive(&mut run, request(TriggerMode::Start, None, None, &budget), &budget);
    assert_eq!(driven.result.unwrap_err(), declined);
    assert!(driven.effects.is_empty() && driven.tokenizer.is_empty());
}

#[test]
fn a_lua_trigger_declines_before_any_host_request_or_effect() {
    // The first trigger would write a variable and request a token count; the Lua trigger after it runs in every mode.
    let budget = budget();
    let database = case_database(&format!(r#"{{"character":{{"triggerscript":[
        {{"comment":"first","type":"start","conditions":[],"effect":[{SET_A},
            {{"type":"v2SystemPrompt","location":"start","valueType":"value","value":"x","indent":0}}]}},
        {{"comment":"lua","type":"output","conditions":[],"effect":[{{"type":"triggerlua","code":"setChatVar(triggerId, 'b', '1')"}}]}}]}}}}"#), &budget);
    let character = character(&database, &budget);
    let input = TriggerInput::from_records(&database, character, conversation(character, &budget), &budget).unwrap();
    let tape = SharedTape::new(Vec::new());
    let mut run = runner(&database, input, &tape, &budget);
    let declined = drive(&mut run, request(TriggerMode::Start, None, None, &budget), &budget);
    assert_eq!(declined.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("trigger-effect-triggerlua")));
    assert!(declined.tokenizer.is_empty() && declined.effects.is_empty());
}

#[test]
fn a_local_scope_hides_no_object_prototype_key() {
    // `scope[key] !== undefined` finds `Object.prototype.constructor`, which TS would then pass to `setLocalVar`.
    let declined = start(&format!(r#"{{"type":"v2DeclareLocalVar","var":"x","valueType":"value","value":"1","indent":0}},
        {{"type":"v2SetVar","operator":"=","var":"constructor","valueType":"value","value":"2","indent":0}}"#), "{}");
    assert_eq!(declined.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("trigger-local-key")));
    assert!(declined.effects.is_empty() && declined.tokenizer.is_empty());
}

#[test]
fn message_edits_wait_for_done_while_token_counts_are_requested() {
    let prompt = r#"{"type":"v2SystemPrompt","location":"start","valueType":"value","value":"x","indent":0}"#;
    let driven = start(&format!("{SET_A},{IMPERSONATE},{SET_NOTE},{prompt}"), "{}");
    let output = driven.result.as_ref().unwrap();
    assert_eq!(driven.tokenizer, ["x\n\n"]);
    assert_eq!(output.tokens, 1);
    assert_eq!(driven.drained, [vec!["variable", "field"]]);
    assert_eq!(kinds(&driven), ["variable", "field", "message"]);
}

/// A plugin tokenizer whose answer declines.
struct DecliningTokenizer;

impl TokenCounter for DecliningTokenizer {
    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        let TokenInput::Text(text) = input else { panic!("a system prompt count") };
        Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: text.copy(budget)? }))
    }
    fn resume(&mut self, _reply: HostReply, _budget: &Budget) -> Result<Step<u32>, StageError> {
        Err(StageError::Unsupported(Cow::Borrowed("tokenizer-reply")))
    }
}

#[test]
fn a_decline_after_only_token_counts_leaves_the_run_to_ts() {
    // A plugin tokenizer count does not commit (`HostKind::commits`), so a decline after one is still a decline, with
    // nothing left to take, as the prompt machine treats it.
    let budget = budget();
    let prompt = r#"{"type":"v2SystemPrompt","location":"start","valueType":"value","value":"x","indent":0}"#;
    let database = case_database(&format!(r#"{{"character":{{"triggerscript":[{{"comment":"t","type":"start","conditions":[],
        "effect":[{SET_A},{IMPERSONATE},{prompt}]}}]}}}}"#), &budget);
    let character = character(&database, &budget);
    let input = TriggerInput::from_records(&database, character, conversation(character, &budget), &budget).unwrap();
    let tape = SharedTape::new(Vec::new());
    let processor = processor(&database, tape.clone(), &budget);
    let mut run = TriggerRun::new(input, processor, DecliningTokenizer, JsPatternEngine::default(), tape, CountingIds(0));
    let step = run.run(request(TriggerMode::Start, None, None, &budget), &budget).unwrap();
    assert!(matches!(step, Step::NeedsHost(HostRequest::PluginTokenizer { .. })));
    assert_eq!(run.take_effects().iter().map(kind).collect::<Vec<_>>(), ["variable"]);
    let declined = run.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap_err();
    assert_eq!(declined, StageError::Unsupported(Cow::Borrowed("tokenizer-reply")));
    assert!(run.take_effects().is_empty());
}

/// A tokenizer whose count is a network request, as Google Cloud's `countTokens` is: it commits.
struct HttpTokenizer;

impl TokenCounter for HttpTokenizer {
    fn count(&mut self, _input: TokenInput<'_>, _budget: &Budget) -> Result<Step<u32>, StageError> {
        Ok(Step::NeedsHost(HostRequest::Http(HttpRequestSpec { method: HttpMethod::Post, url: String::new(), headers: vec![],
            body: vec![], response: ResponseKind::Buffered })))
    }
    fn resume(&mut self, _reply: HostReply, _budget: &Budget) -> Result<Step<u32>, StageError> { Ok(Step::Done(3)) }
}

/// A run over `effects` as one start trigger, with `counter` and without a similarity search.
fn bare_run<C: TokenCounter>(effects: &str, counter: C, budget: &Budget) -> TriggerRun<risunest_cbs_experimental::cbs::processor::Processor<SharedTape>, C, JsPatternEngine, SharedTape, CountingIds> {
    let database = case_database(&format!(r#"{{"character":{{"lowLevelAccess":true,"triggerscript":[{{"comment":"t","type":"start",
        "conditions":[],"effect":[{effects}]}}]}}}}"#), budget);
    let record = character(&database, budget);
    let input = TriggerInput::from_records(&database, record, conversation(record, budget), budget).unwrap();
    let tape = SharedTape::new(Vec::new());
    TriggerRun::new(input, processor(&database, tape.clone(), budget), counter, JsPatternEngine::default(), tape, CountingIds(0))
}

#[test]
fn a_decline_after_a_tokenize_count_that_commits_fails_and_keeps_the_writes() {
    // The count went out as a request TS would make again, so the decline after it cannot leave the run to TS: it fails,
    // and the writes the run made stay while its messages are dropped.
    let budget = budget();
    let tokenize = r#"{"type":"v2Tokenize","valueType":"value","value":"abcd","outputVar":"out","indent":0}"#;
    let calc = r#"{"type":"v2Calculate","expressionType":"value","expression":"@g+1","outputVar":"res","indent":0}"#;
    let mut run = bare_run(&format!("{SET_A},{tokenize},{IMPERSONATE},{SET_NOTE},{calc}"), HttpTokenizer, &budget);
    let step = run.run(request(TriggerMode::Start, None, None, &budget), &budget).unwrap();
    assert!(matches!(step, Step::NeedsHost(HostRequest::Http(_))));
    assert_eq!(run.take_effects().iter().map(kind).collect::<Vec<_>>(), ["variable"]);
    let failed = run.resume(HostReply::Http { status: 200, body: Vec::new() }, &budget).unwrap_err();
    assert_eq!(failed, StageError::Invalid(Cow::Borrowed("calc-variable")));
    let kept = run.take_effects();
    assert_eq!(kept.iter().map(kind).collect::<Vec<_>>(), ["variable", "field"]);
    assert!(matches!(&kept[0], Effect::ChatVariable { name, value: Value::Text(count) } if string(name) == "out" && string(count) == "3"));
}

#[test]
fn what_the_dependencies_cannot_run_declines_in_admission() {
    // A tokenizer this build does not run declines every count, so a run that counts declines before anything happens,
    // as admission tells an owner before its own first request.
    let budget = budget();
    let local = || Counter::new(TokenizerId::Local, ChatAccounting::default());
    let tokenize = r#"{"type":"v2Tokenize","valueType":"value","value":"abcd","outputVar":"out","indent":0}"#;
    let prompt = r#"{"type":"v2SystemPrompt","location":"start","valueType":"value","value":"x","indent":0}"#;
    let similar = r#"{"type":"v2CheckSimilarity","sourceType":"value","source":"a","valueType":"value","value":"a§b","outputVar":"sim","indent":0}"#;
    let declined = |name: &'static str| StageError::Unsupported(Cow::Borrowed(name));
    for (effects, name) in [(tokenize, "tokenizer-local"), (prompt, "tokenizer-local")] {
        let mut run = bare_run(&format!("{SET_A},{effects}"), local(), &budget);
        let start = request(TriggerMode::Start, None, None, &budget);
        assert_eq!(run.admit(&start, &budget).unwrap_err(), declined(name));
        assert_eq!(run.run(start, &budget).unwrap_err(), declined(name));
        assert!(run.take_effects().is_empty());
    }
    // A run that never counts does not need the tokenizer.
    let mut run = bare_run(SET_A, local(), &budget);
    assert!(run.admit(&request(TriggerMode::Start, None, None, &budget), &budget).is_ok());
    assert!(matches!(run.run(request(TriggerMode::Start, None, None, &budget), &budget), Ok(Step::Done(_))));
    // Without a search wired, `v2CheckSimilarity` and v1 `checkSimilarity` decline before anything happens.
    let similar_v1 = r#"{"type":"checkSimilarity","source":"a","value":"a§b","inputVar":"sim"}"#;
    for (similar, name) in [(similar, "trigger-effect-v2CheckSimilarity"), (similar_v1, "trigger-effect-checkSimilarity")] {
        let mut run = bare_run(&format!("{SET_A},{similar}"), Counter::new(TokenizerId::Plugin, ChatAccounting::default()), &budget);
        let start = request(TriggerMode::Start, None, None, &budget);
        assert_eq!(run.admit(&start, &budget).unwrap_err(), declined(name));
        assert_eq!(run.run(start, &budget).unwrap_err(), declined(name));
        assert!(run.take_effects().is_empty());
    }
}

#[test]
fn a_tokenize_count_the_tokenizer_answers_itself_needs_no_host_request() {
    // Without the plugin's tokenizer function every text encodes as `[0]` (`src/ts/tokenizer.ts`), one token the
    // counter gives without the host, so the run stores it and goes on.
    let budget = budget();
    let effects = r#"{"type":"v2Tokenize","valueType":"value","value":"abcdefgh","outputVar":"out","indent":0},
        {"type":"v2SetVar","operator":"=","var":"after","valueType":"value","value":"{{getvar::out}}","indent":0}"#;
    let database = case_database(&format!(r#"{{"character":{{"triggerscript":[{{"comment":"t","type":"start","conditions":[],
        "effect":[{effects}]}}]}}}}"#), &budget);
    let record = character(&database, &budget);
    let input = TriggerInput::from_records(&database, record, conversation(record, &budget), &budget).unwrap();
    let tape = SharedTape::new(Vec::new());
    let counter = Counter::new(TokenizerId::PluginMissing, ChatAccounting::default());
    let mut run = TriggerRun::new(input, processor(&database, tape.clone(), &budget), counter, JsPatternEngine::default(), tape.clone(),
        CountingIds(0));
    let driven = drive(&mut run, request(TriggerMode::Start, None, None, &budget), &budget);
    assert!(driven.result.is_ok(), "{:?}", driven.result.as_ref().err());
    assert!(driven.tokenizer.is_empty());
    let writes: Vec<(String, String)> = driven.effects.iter().filter_map(|effect| match effect {
        Effect::ChatVariable { name, value: Value::Text(value) } => Some((string(name), string(value))),
        _ => None,
    }).collect();
    assert_eq!(writes, [("out", "1"), ("after", "1")].map(|(name, value)| (name.to_string(), value.to_string())));
}

#[test]
fn a_failed_run_keeps_its_variable_and_note_writes_without_its_message_edits() {
    let invalid = r#"{"type":"v2ExtractRegex","valueType":"value","value":"x","regexType":"value","regex":"(","flagsType":"value","flags":"",
        "resultType":"value","result":"$1","outputVar":"out","indent":0}"#;
    let failed = start(&format!("{SET_A},{IMPERSONATE},{SET_NOTE},{invalid}"), "{}");
    assert!(matches!(failed.result, Err(StageError::Invalid(_))));
    assert_eq!(kinds(&failed), ["variable", "field"]);
}

#[test]
fn changes_applied_before_the_run_are_the_conversation_it_reads() {
    // TS's trigger reads the conversation as earlier stages left it (`getVar` through the session,
    // `src/ts/process/triggers.ts:1339-1345`; the messages and note of the chat it is given). The oracle's `earlier`
    // cases compare applied variable writes with TS; these are the message and note changes.
    let budget = budget();
    let reads = r#"{"type":"v2GetLastMessage","outputVar":"last","indent":0},{"type":"v2GetMessageCount","outputVar":"count","indent":0},
        {"type":"v2GetMessageAtIndex","index":"0","indexType":"value","outputVar":"first","indent":0},
        {"type":"v2GetAuthorNote","outputVar":"note","indent":0},{"type":"v2SetVar","operator":"=","var":"seen","valueType":"var","value":"gate","indent":0},
        {"type":"v2SetVar","operator":"=","var":"cbs","valueType":"value","value":"{{lastmessage}}","indent":0}"#;
    let database = case_database(&format!(r#"{{"character":{{"triggerscript":[{{"comment":"t","type":"start","conditions":[],"effect":[{reads}]}}]}}}}"#), &budget);
    let character = character(&database, &budget);
    let input = TriggerInput::from_records(&database, character, conversation(character, &budget), &budget).unwrap();
    let tape = SharedTape::new(Vec::new());
    let mut run = runner(&database, input, &tape, &budget);
    let text = |value: &str| Text::from_str(value, &budget).unwrap();
    for effect in [
        Effect::Message(MessageEdit::Cut { start: 1.0, end: None }),
        Effect::Message(MessageEdit::SetData { index: -3.0, data: Value::Text(text("edited")) }),
        Effect::Message(MessageEdit::Append { role: MessageRole::User, data: Value::Text(text("appended")), chat_id: text("earlier-id") }),
        Effect::ChatField { field: text("note"), value: Value::Text(text("earlier note")) },
        Effect::ChatVariable { name: text("gate"), value: Value::Text(text("open")) },
    ] { run.apply(&effect, &budget).unwrap(); }
    let driven = drive(&mut run, request(TriggerMode::Start, None, None, &budget), &budget);
    assert!(driven.result.is_ok(), "{:?}", driven.result.as_ref().err());
    let writes: Vec<(String, String)> = driven.effects.iter().filter_map(|effect| match effect {
        Effect::ChatVariable { name, value: Value::Text(value) } => Some((string(name), string(value))),
        _ => None,
    }).collect();
    // CBS reads them through the processor.
    let expected = [("last", "appended"), ("count", "4"), ("first", "edited"), ("note", "earlier note"), ("seen", "open"), ("cbs", "appended")];
    assert_eq!(writes, expected.map(|(name, value)| (name.to_string(), value.to_string())));
    // Only before the run.
    let late = Effect::ChatVariable { name: text("late"), value: Value::Text(text("1")) };
    assert_eq!(run.apply(&late, &budget).unwrap_err(), StageError::Invalid(Cow::Borrowed("trigger-apply")));

    // The Lua edits (`setFullChatMain`, `removeChat`, `insertChat`, `setChatRole`) are the oracle's `earlierEdits` cases.
    // No stage stores these chat fields, so they decline instead of standing for the conversation's.
    let mut input = TriggerInput::from_records(&database, character, conversation(character, &budget), &budget).unwrap();
    for field in ["scriptstate", "message", "fmIndex"] {
        let replaced = Effect::ChatField { field: text(field), value: Value::Null };
        assert_eq!(input.apply(&replaced, &budget).unwrap_err(), StageError::Unsupported(Cow::Borrowed("trigger-applied-effect")));
    }
}

#[test]
fn approximate_text_equality_declines_under_a_tailored_locale() {
    let compare = |source: &str, target: &str, tailored: bool| run_effects(&format!(
        r#"{{"type":"v2IfAdvanced","condition":"≒","sourceType":"value","source":"{source}","targetType":"value","target":"{target}","indent":0}},
        {SET_A},{{"type":"v2EndIndent","indent":1,"endOfLoop":false}}"#), "{}", false, tailored, TriggerMode::Start, None);
    // `toLocaleLowerCase()` under `tr` maps `I` to dotless `ı`.
    let declined = compare("I a", "ia", true);
    assert_eq!(declined.result.unwrap_err(), StageError::Unsupported(Cow::Borrowed("case-locale")));
    assert!(declined.effects.is_empty());
    assert_eq!(kinds(&compare("I a", "ia", false)), ["variable"]);
    // Numbers compare without case mapping.
    assert_eq!(kinds(&compare("1", "1.00001", true)), ["variable"]);
}

#[test]
fn unbounded_low_level_recursion_stops_at_the_depth_limit() {
    // With `lowLevelAccess` TS has no recursion bound; the stage stops at `Limits::max_depth`.
    let again = r#"{"type":"v2SetVar","operator":"+=","var":"count","valueType":"value","value":"1","indent":0},
        {"type":"v2RunTrigger","target":"again","indent":0}"#;
    let failed = run_effects(again, "{}", true, false, TriggerMode::Manual, Some("again"));
    assert_eq!(failed.result.unwrap_err(), StageError::Limit(Cow::Borrowed("depth")));
    assert_eq!(failed.effects.len(), 65, "one write per invocation up to the limit");
}

#[test]
fn appended_message_ids_are_version_4_uuids_from_three_draws() {
    let budget = budget();
    let mut ids = RandomIds(Tape::new(vec![0.0, 0.5, 0.999_999_999, 0.25, 0.75, 0.125]));
    let first = string(&ids.next_id(&budget).unwrap());
    let second = string(&ids.next_id(&budget).unwrap());
    for id in [&first, &second] {
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|part| part.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12], "{id}");
        assert!(id.chars().all(|character| character == '-' || character.is_ascii_hexdigit() && !character.is_ascii_uppercase()), "{id}");
        assert!(parts[2].starts_with('4') && "89ab".contains(&parts[3][..1]), "{id}");
    }
    assert_ne!(first, second);
    assert_eq!(ids.0.consumed(), 6);
}

#[test]
fn captured_modules_select_the_module_triggers_instead_of_the_root_mirror() {
    // The stored root mirror names `mirror` and the effective preset `effective`; the owner captures the modules once with
    // the effective setting for every preparation stage, and the run takes that capture.
    let budget = budget();
    const CONCAT: &str = r#"{"type":"v2ConcatString","source1":"seq","source1Type":"var","source2":"ID","source2Type":"value","outputVar":"seq","indent":0}"#;
    let module = |id: &str| r#"{"id":"ID","name":"ID","trigger":[{"comment":"","type":"start","conditions":[],"effect":[EFFECT]}]}"#
        .replace("EFFECT", CONCAT).replace("ID", id);
    let case = r#"{"root":{"modules":[MODULES],"moduleIntergration":"mirror"},
        "character":{"triggerscript":[{"comment":"","type":"start","conditions":[],"effect":[SET]}]}}"#
        .replace("MODULES", &format!("{},{}", module("mirror"), module("effective")))
        .replace("SET", r#"{"type":"v2SetVar","operator":"=","var":"seq","valueType":"value","value":"c","indent":0}"#);
    let database = case_database(&case, &budget);
    let record = character(&database, &budget);
    let chat = conversation(record, &budget);
    let writes = |input: TriggerInput| -> Vec<String> {
        let tape = SharedTape::new(Vec::new());
        let mut run = runner(&database, input, &tape, &budget);
        let driven = drive(&mut run, request(TriggerMode::Start, None, None, &budget), &budget);
        assert!(driven.result.is_ok(), "{:?}", driven.result.as_ref().err());
        driven.effects.iter().filter_map(|effect| match effect {
            Effect::ChatVariable { value: Value::Text(value), .. } => Some(string(value)),
            _ => None,
        }).collect()
    };
    let sources = ModuleSources::for_conversation(&database, record, chat, &budget).unwrap();
    let effective = Value::Text(Text::from_str("effective", &budget).unwrap());
    let captured = SelectedModules::capture(&sources, Some(&effective), &budget).unwrap();
    assert_eq!(writes(TriggerInput::from_records_with_modules(&database, record, chat, &captured, &budget).unwrap()), ["c", "ceffective"]);
    // An absent effective setting integrates nothing, never the mirror.
    let none = SelectedModules::capture(&sources, None, &budget).unwrap();
    assert_eq!(writes(TriggerInput::from_records_with_modules(&database, record, chat, &none, &budget).unwrap()), ["c"]);
    // `from_records` reads the stored root as it stands.
    assert_eq!(writes(TriggerInput::from_records(&database, record, chat, &budget).unwrap()), ["c", "cmirror"]);
}

#[test]
fn token_counts_are_reachable_through_the_program_admission_compiles() {
    // An owner that counts approximately in admission counts exactly when the run can count, so a branch on a count
    // takes the same path in admission and in the owned run.
    let budget = budget();
    let tokenize = r#"{"type":"v2Tokenize","valueType":"value","value":"abcd","outputVar":"out","indent":0}"#;
    let trigger = |kind: &str, comment: &str, effects: &str| {
        format!(r#"{{"comment":"{comment}","type":"{kind}","conditions":[],"effect":[{effects}]}}"#)
    };
    let reaches = |triggers: String, mode: TriggerMode| -> Result<bool, StageError> {
        let database = case_database(&format!(r#"{{"character":{{"triggerscript":[{triggers}]}}}}"#), &budget);
        let record = character(&database, &budget);
        let input = TriggerInput::from_records(&database, record, conversation(record, &budget), &budget).unwrap();
        let event = request(mode, None, None, &budget);
        let counts = reaches_counts(&input, &event, &budget);
        // A tokenizer this build does not run declines exactly the runs that count.
        let local = Dependencies { tokenizer: Some("tokenizer-local"), ..Dependencies::default() };
        if let Ok(counts) = counts { assert_eq!(admit(&input, &event, local, &budget).is_err(), counts, "{triggers}"); }
        counts
    };
    assert_eq!(reaches(String::new(), TriggerMode::Start), Ok(false));
    assert_eq!(reaches(trigger("start", "t", SET_A), TriggerMode::Start), Ok(false));
    assert_eq!(reaches(trigger("start", "t", &format!("{SET_A},{tokenize}")), TriggerMode::Start), Ok(true));
    // v1 and v2 system prompts are counted at the end of the invocation.
    assert_eq!(reaches(trigger("start", "t", r#"{"type":"systemprompt","location":"start","value":"x"}"#), TriggerMode::Start), Ok(true));
    let prompt = r#"{"type":"v2SystemPrompt","location":"promptend","valueType":"value","value":"x","indent":0}"#;
    assert_eq!(reaches(trigger("start", "t", prompt), TriggerMode::Start), Ok(true));
    // Through a called trigger, and not through one nothing calls or another event's.
    let sub = trigger("manual", "sub", tokenize);
    let call = r#"{"type":"v2RunTrigger","target":"sub","indent":0}"#;
    assert_eq!(reaches(format!("{},{sub}", trigger("start", "t", call)), TriggerMode::Start), Ok(true));
    assert_eq!(reaches(format!("{},{sub}", trigger("start", "t", SET_A)), TriggerMode::Start), Ok(false));
    assert_eq!(reaches(trigger("output", "o", tokenize), TriggerMode::Start), Ok(false));
    assert_eq!(reaches(trigger("output", "o", tokenize), TriggerMode::Output), Ok(true));
    // What the stored triggers alone decline is that decline.
    assert_eq!(reaches(trigger("start", "t", r#"{"type":"v2UpdateGUI","indent":0}"#), TriggerMode::Start),
        Err(StageError::Unsupported(Cow::Borrowed("trigger-effect-v2UpdateGUI"))));
}

/// A run of a route contract case. With `off` the case's switches are off, as the route owner sets them; otherwise the
/// owner answers model requests, acts on both outcomes and shares Rust's case tables.
fn route_run(database: &Value, switches: &Value, off: bool, budget: &Budget) -> Run {
    let disabled = |key: &str| off && matches!(field(switches, key, budget), Some(Value::Bool(false)));
    let record = character(database, budget);
    let mut input = TriggerInput::from_records(database, record, conversation(record, budget), budget).unwrap();
    input.case_tables = match (off, text_field(switches, "caseTables", budget).as_deref()) {
        (true, Some("foreign")) => CaseTables::Foreign,
        (true, Some("unicode16")) => CaseTables::Unicode16,
        (_, None | Some("foreign" | "unicode16")) => CaseTables::Native,
        (_, Some(other)) => panic!("case tables {other}"),
    };
    let tape = SharedTape::new(Vec::new());
    let mut run = TriggerRun::new(input, processor(database, tape.clone(), budget), Counter::new(TokenizerId::Plugin, ChatAccounting::default()),
        JsPatternEngine::default(), tape, CountingIds(0)).with_similarity(Box::new(ReversedSimilarity::default()));
    if !disabled("llm") { run = run.with_llm(); }
    if disabled("stopSending") { run = run.without_stop_sending(); }
    if disabled("sendAiPrompt") { run = run.without_send_ai_prompt(); }
    run
}

#[test]
fn route_declines_come_before_any_host_request_or_effect() {
    // The route contract (`tests/fixtures/rust-core/triggers/route-declines.*.json`): TS runs every case, and the native
    // Send route, which answers no model request, acts on neither outcome yet and knows the engine's case tables only as
    // probed, declines it before a token count, a model request or an effect reaches the owner. Each case turns one
    // switch off, and with the switch on it runs to the end.
    let budget = budget();
    let file = parse_json(std::str::from_utf8(ROUTE_CASES).unwrap(), &budget).unwrap();
    let expected = parse_json(std::str::from_utf8(ROUTE_EXPECTED).unwrap(), &budget).unwrap();
    let digest: String = Sha256::digest(ROUTE_CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(text_field(&expected, "fixtureSha256", &budget), Some(digest), "the expected file was recorded from other cases");
    assert!(!ROUTE_CASES.contains(&b'\r'));
    let base = field(&file, "base", &budget).unwrap();
    let cases = field(&file, "cases", &budget).and_then(Value::array).unwrap();
    let results = field(&expected, "cases", &budget).and_then(Value::array).unwrap();
    assert_eq!(cases.len(), results.len());
    let mut reply = |_: &HostRequest| HostReply::Llm(Ok(ProviderOutput { texts: vec![Text::from_str("reply", &budget).unwrap()],
        shape: OutputShape::Text, thoughts: Vec::new(), tool_calls: Vec::new(), usage: None, finish: None }));
    let mut declined = std::collections::BTreeSet::new();
    for (case, recorded) in cases.iter().zip(results) {
        let id = text_field(case, "id", &budget).unwrap();
        assert_eq!(text_field(recorded, "id", &budget).as_deref(), Some(id.as_str()));
        let result = field(recorded, "result", &budget).unwrap();
        assert_eq!(text_field(result, "contract", &budget).as_deref(), Some("route"), "{id}");
        let decline = text_field(result, "decline", &budget);
        let phase = text_field(result, "phase", &budget);
        let mode = match text_field(case, "mode", &budget).as_deref() {
            Some("start") => TriggerMode::Start,
            Some("output") => TriggerMode::Output,
            other => panic!("{id}: mode {other:?}"),
        };
        let database = parse_json(&database_text(base, case, &budget), &budget).unwrap();
        let switches = field(case, "route", &budget).unwrap();
        for off in [true, false] {
            let mut run = route_run(&database, switches, off, &budget);
            let admission = run.admit(&request(mode, None, None, &budget), &budget);
            let driven = drive_with(&mut run, request(mode, None, None, &budget), &mut reply, &budget);
            let Some(name) = decline.as_deref().filter(|_| off) else {
                assert_eq!(admission, Ok(()), "{id}");
                assert!(driven.result.is_ok(), "{id} (switches off: {off}): {:?}", driven.result.as_ref().err());
                continue;
            };
            let unsupported = StageError::Unsupported(Cow::Owned(name.to_string()));
            match phase.as_deref() {
                Some("admission") => assert_eq!(admission.as_ref().err(), Some(&unsupported), "{id}"),
                Some("running") => assert_eq!(admission, Ok(()), "{id}"),
                other => panic!("{id}: phase {other:?}"),
            }
            assert_eq!(driven.result.as_ref().err(), Some(&unsupported), "{id}");
            assert!(driven.drained.is_empty() && driven.tokenizer.is_empty() && driven.hosts.is_empty(), "{id}: a host request before the decline");
            assert!(driven.effects.is_empty() && driven.after_error == 0 && driven.console.is_empty(), "{id}: effects before the decline");
            declined.insert(name.to_string());
        }
    }
    assert_eq!(declined.into_iter().collect::<Vec<_>>(),
        ["trigger-case-table", "trigger-effect-runLLM", "trigger-effect-v2RunLLM", "trigger-send-ai-prompt", "trigger-stop-sending"]);
}
