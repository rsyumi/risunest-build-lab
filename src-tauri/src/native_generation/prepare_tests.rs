use crate::native_generation::{cbs::{CaseUnicode, Entropy, HostCapture}, inputs::{OllamaIntent, PreparationError, PreparationSnapshot}, operation::Target,
    prepare::prepare_ollama};
use crate::persistent_store::{PersistentStore, WorkingSetCommit, RootMutation, ConversationMutation};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn cases() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-text.json")).unwrap()
}

/// `stored` as TS stored the history (`expected`): a message the start trigger appended carries a fresh UUID of its own,
/// which no row of `original` holds.
pub(crate) fn assert_stored_rows(stored: &[Value], expected: &Value, original: &[Value], id: &str) {
    let known: std::collections::BTreeSet<_> = original.iter().filter_map(|row| row["chatId"].as_str()).collect();
    let expected = expected.as_array().unwrap_or_else(|| panic!("{id}: no stored rows"));
    assert_eq!(stored.len(), expected.len(), "{id}");
    let mut drawn = std::collections::BTreeSet::new();
    for (stored, expected) in stored.iter().zip(expected) {
        let (mut stored, mut expected) = (stored.clone(), expected.clone());
        if expected["chatId"].as_str().is_some_and(|chat| !known.contains(chat)) {
            let chat = stored["chatId"].as_str().unwrap_or_else(|| panic!("{id}: an appended message without an ID: {stored}"));
            assert!(uuid::Uuid::parse_str(chat).is_ok_and(|uuid| uuid.get_version_num() == 4 && uuid.hyphenated().to_string() == chat), "{id}: {chat}");
            assert!(!known.contains(chat) && drawn.insert(chat.to_owned()), "{id}: {chat}");
            (stored["chatId"], expected["chatId"]) = (Value::Null, Value::Null);
        }
        assert_eq!(stored, expected, "{id}");
    }
}

// The history once the send stores `plan`'s message edits over the rows it read.
fn messages_after(rows: &[Value], plan: &crate::native_generation::preparation_effects::PreparedVariablePlan) -> Vec<Value> {
    let mut rows = rows.to_vec();
    if let Some(edits) = &plan.messages {
        assert_eq!(edits.base_count, rows.len());
        rows.splice(edits.start..edits.start + edits.delete_count, edits.rows.iter().cloned());
    }
    rows
}

// `plan` with the IDs of the messages it appends (rows whose ID the history it read lacks) masked, and those IDs.
fn without_appended_ids(plan: &crate::native_generation::preparation_effects::PreparedVariablePlan, original: &[Value])
    -> (crate::native_generation::preparation_effects::PreparedVariablePlan, Vec<String>)
{
    let known: std::collections::BTreeSet<_> = original.iter().filter_map(|row| row["chatId"].as_str()).collect();
    let (mut plan, mut drawn) = (plan.clone(), Vec::new());
    for row in plan.messages.iter_mut().flat_map(|edits| edits.rows.iter_mut()) {
        if let Some(chat) = row["chatId"].as_str().filter(|chat| !known.contains(chat)).map(str::to_owned) {
            drawn.push(chat);
            row["chatId"] = Value::Null;
        }
    }
    (plan, drawn)
}

// The refusal a trigger row meets before reservation, as its `decline` names it. The `decline` of another row names a
// decline of the prompt machine's literal replay (`crates/core/prompt/src/machine.rs`), and the route prepares that row.
fn route_decline(case: &Value) -> Option<&str> {
    case["decline"].as_str().filter(|_| case["observeTriggers"] == true)
}

fn merge(base: &Value, patch: &Value) -> Value {
    let mut result = base.clone();
    if let Some(patch) = patch.as_object() { for (key, value) in patch { result[key] = value.clone(); } }
    result
}

pub(crate) fn database(case: &Value) -> Value {
    let fixture = cases();
    let mut root = merge(&fixture["root"], &case["root"]);
    let mut character = merge(&fixture["character"], &case["character"]);
    let mut conversation = merge(&fixture["conversation"], &case["conversation"]);
    conversation["message"] = case.get("messages").unwrap_or(&fixture["messages"]).clone();
    character["chats"] = json!([conversation]);
    root["characters"] = json!([character]);
    root["botPresets"] = json!([merge(&fixture["preset"], &case["preset"])]);
    root
}

pub(super) fn stored(database: &Value) -> (tempfile::TempDir, PersistentStore, OllamaIntent) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let mut root = database.clone();
    let characters = root.as_object_mut().unwrap().remove("characters").unwrap();
    let presets = root.as_object_mut().unwrap().remove("botPresets").unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &root).unwrap();
    store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
    store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
    store.replace_commit(&stage.staging_id, Some(0)).unwrap();
    let intent = OllamaIntent { effective_preset_id: "preset-a".into(), host: None, target: Target {
        character_id: "character".into(), conversation_id: "conversation".into(),
        expected_revision: store.revision().unwrap(), library_identity: store.library_identity().unwrap(),
        binding_authority: store.lww_binding_authority().unwrap(),
    }};
    (directory, store, intent)
}

fn now() -> u64 {
    u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

// Inputs the route refuses, with the refusal name.
fn route_refusals() -> Vec<(Value, &'static str)> {
    vec![
        (json!({"preset":{"aiModel":"ollama-cloud"}}), "model"),
        (json!({"root":{"presetChain":"other"}}), "presetChain"),
        (json!({"preset":{"regex":[{"type":"editoutput","in":"x","out":"y"}]}}), "regex.host"),
        // `editprocess` scripts read the host capture, and TS throws on a list or entry it cannot spread or filter.
        (json!({"preset":{"regex":[{"type":"editprocess","in":"x","out":"y"}]}}), "regex.host"),
        (json!({"character":{"customscript":[{"type":"editprocess","in":"x","out":"y"}]}}), "regex.host"),
        (json!({"character":{"customscript":"x"}}), "regex.scripts"),
        (json!({"character":{"customscript":[null]}}), "regex.scripts"),
        (json!({"character":{"customscript":[{"type":"editprocess","in":7,"out":"y"}]}}), "regex.scripts"),
        // The start trigger stage runs `start` triggers; a `request` or `output` trigger has no runner yet.
        (json!({"character":{"triggerscript":[{"comment":"","type":"request","conditions":[],"effect":[{"type":"v2StopTrigger"}]}]}}), "triggers"),
        (json!({"character":{"triggerscript":[{"comment":"","type":"output","conditions":[],"effect":[{"type":"v2StopTrigger"}]}]}}), "triggers"),
        // TS runs a trigger led by `triggercode` in every mode, with that effect doing nothing; the route leaves it to TS.
        (json!({"character":{"triggerscript":[{"comment":"","type":"manual","conditions":[],"effect":[{"type":"triggercode","code":"synthetic"}]}]}}), "triggers"),
        (json!({"conversation":{"localLore":{}}}), "lore-list"),
        (json!({"root":{"plugins":[{"name":"synthetic-enabled-plugin","enabled":true,"version":"3.0"}]}}), "plugins"),
        (json!({"root":{"enabledModules":["active"],"modules":[{"id":"active","regex":[{"type":"editprocess"}],"trigger":[{"type":"start","effect":[{"type":"v2StopTrigger"}]}]}]}}), "modules"),
        (json!({"root":{"hypaV3":true},"character":{"supaMemory":true}}), "memory"),
        (json!({"character":{"additionalText":"embedding text"}}), "additionalText"),
        (json!({"character":{"postHistoryInstructions":"legacy note"}}), "postHistoryInstructions"),
        (json!({"character":{"inlayViewScreen":true,"viewScreen":"emotion"}}), "inlayViewScreen"),
        (json!({"preset":{"enableCustomFlags":true}}), "customFlags"),
        (json!({"preset":{"modelTools":["tool"]}}), "tools"),
        (json!({"root":{"applyAdditionalParamsToAll":true,"additionalParams":[["temperature","1"]]}}), "additionalParams"),
        (json!({"preset":{"fallbackModels":{"model":["fallback"]}}}), "fallbackModels"),
        (json!({"root":{"ttsAutoSpeech":true}}), "ttsAutoSpeech"),
        (json!({"root":{"removeIncompleteResponse":true}}), "removeIncompleteResponse"),
        (json!({"root":{"autoContinueChat":true}}), "autoContinueChat"),
        (json!({"root":{"autoTranslate":true}}), "autoTranslate"),
        (json!({"root":{"inlayErrorResponse":true}}), "inlayErrorResponse"),
        (json!({"root":{"banCharacterset":["Latin"]}}), "banCharacterset"),
        (json!({"messages":[{"role":"user","data":"missing ID"}]}), "messageIdNormalization"),
        (json!({"messages":[{"role":"char","data":"Foreign speaker.","chatId":"speaker","saying":"other-character"}]}), "historySpeakerLookup"),
        (json!({"root":{"promptInfoInsideChat":true}}), "promptInfoInsideChat"),
        (json!({"root":{"statics":{"messages":"many"}}}), "statics"),
        // The turn reads these as whole numbers, and the response markers a truthy name through a template string.
        (json!({"preset":{"maxContext":4000.5}}), "maxContext"),
        (json!({"preset":{"maxContext":"4000"}}), "maxContext"),
        (json!({"preset":{"maxResponse":64.5}}), "maxResponse"),
        (json!({"root":{"personas":[persona("persona-a", json!(7), json!("Synthetic persona.")), persona("persona-b", json!("Bound user"), json!("Bound persona."))]}}), "names"),
        (json!({"root":{"personas":[persona("persona-a", json!("Synthetic user"), json!("Synthetic persona.")), persona("persona-b", json!(7), json!("Bound persona."))]},
            "conversation":{"bindedPersona":"persona-b"}}), "names"),
    ]
}

// A persona as the shared fixture stores it.
fn persona(id: &str, name: Value, prompt: Value) -> Value {
    json!({"id":id,"name":name,"personaPrompt":prompt,"note":"","icon":""})
}

// Refusals the machine makes in admission, with the refusal name.
fn machine_refusals() -> Vec<(Value, &'static str)> {
    vec![
        (json!({"root":{"presetChain":"other"}}), "presetChain"),
        (json!({"root":{"presetChain":7}}), "presetChain"),
        (json!({"root":{"promptInfoInsideChat":true}}), "promptInfoInsideChat"),
        (json!({"character":{"additionalText":"embedding text"}}), "additionalText"),
        (json!({"character":{"postHistoryInstructions":"legacy note"}}), "postHistoryInstructions"),
        (json!({"conversation":{"localLore":7}}), "lore-list"),
        (json!({"root":{"enabledModules":["plain"],"modules":[{"id":"plain","name":"Plain module","assets":[["synthetic","asset","bin"]]}]}}), "modules"),
        // The current chat parser pass stores the history it rewrites, even in disabled rows and rows before a reset.
        (json!({"messages":[{"role":"user","chatId":"disabled","disabled":true,"data":"{{user}}"}]}), "historyRewrite"),
        (json!({"messages":[{"role":"user","chatId":"before","data":"{{char}}"},{"role":"char","chatId":"reset","disabled":"allBefore","data":"Reset."},{"role":"user","chatId":"selected","data":"Selected question."}]}), "historyRewrite"),
        // It also stores the chat variables a message writes, whether or not the message changes.
        (json!({"messages":[{"role":"user","chatId":"write","data":"{{setvar::x::y}}"}]}), "historyRewrite"),
        (json!({"conversation":{"scriptstate":{"$same":"{{getvar::same}}{{addvar::n::1}}"}},
            "messages":[{"role":"user","chatId":"same","data":"{{getvar::same}}{{addvar::n::1}}"}]}), "historyRewrite"),
        (json!({"messages":[{"role":"user","data":"{{inlay::synthetic}}","chatId":"media"}]}), "inlay"),
        // CBS that reads host state the route does not capture, named by that state.
        (json!({"character":{"exampleMessage":"<START>\n{{char}}: {{screenwidth}}"}}), "cbs.screenwidth"),
        (json!({"preset":{"mainPrompt":"{{screenheight}}"}}), "cbs.screenheight"),
        (json!({"conversation":{"note":"{{metadata::version}}"}}), "cbs.metadata"),
        (json!({"character":{"desc":"{{trigger_id}}"}}), "cbs.trigger_id"),
        // A stored value TS throws on only where a function reads it.
        (json!({"preset":{"promptTemplate":[{"type":"authornote","defaultText":7},{"type":"chat","rangeStart":0,"rangeEnd":"end"}]},
            "conversation":{"note":""},"messages":[{"role":"user","chatId":"read","data":"{{authornote}}"}]}), "cbs.snapshot"),
        (json!({"conversation":{"scriptstate":{"$odd":{"toString":"x"}}},"preset":{"mainPrompt":"{{getvar::odd}}"}}), "cbs.snapshot"),
        (json!({"conversation":{"fmIndex":0.5},"preset":{"mainPrompt":"{{firstmsgindex}}"},
            "messages":[{"role":"char","chatId":"reset","disabled":"allBefore","data":"Reset."},{"role":"user","chatId":"selected","data":"Selected question."}]}), "cbs.snapshot"),
        (json!({"root":{"personas":[persona("persona-a", json!("Synthetic user"), json!("Synthetic persona.")), persona("persona-b", Value::Null, json!("Bound persona."))]},
            "conversation":{"bindedPersona":"persona-b"},"preset":{"mainPrompt":"{{user}}"}}), "cbs.snapshot"),
        (json!({"character":{"desc":null},"preset":{"mainPrompt":"{{description}}"}}), "cbs.snapshot"),
        // Stored shapes TS sends without an error, which the route does not reproduce: a greeting index into a stored
        // greeting list that is not an array, a role or message ID that is not a string, and a bias entry that is not a
        // pair of a string and a number.
        (json!({"character":{"alternateGreetings":"Greeting text"},"conversation":{"fmIndex":0}}), "fmIndex"),
        (json!({"character":{"alternateGreetings":{"0":"Object greeting."}},"conversation":{"fmIndex":0}}), "fmIndex"),
        (json!({"conversation":{"fmIndex":0.5}}), "fmIndex"),
        (json!({"messages":[{"role":7,"chatId":"role","data":"Question."}]}), "message.role"),
        (json!({"messages":[{"chatId":"role","data":"Question."}]}), "message.role"),
        (json!({"messages":[{"role":"user","chatId":7,"data":"Question."}]}), "message.chatId"),
        // `message.chatId || createId()` stores a fresh ID for a falsy one.
        (json!({"messages":[{"role":"user","chatId":0,"data":"Question."}]}), "messageIdNormalization"),
        (json!({"character":{"bias":"ab"}}), "bias"),
        (json!({"preset":{"bias":["ab"]}}), "bias"),
        (json!({"preset":{"bias":[["key","1"]]}}), "bias"),
        (json!({"preset":{"mainPrompt":"{{dice::4097d6}}"}}), "cbs.random-draws"),
        (json!({"preset":{"mainPrompt":"{{lower::Synthetic}}"}}), "cbs.case-locale"),
        (json!({"preset":{"mainPrompt":"{{capitalize::\u{10d0}}}"}}), "cbs.case-table"),
        (json!({"preset":{"globalNote":"{{time}}"}}), "cbs.local-time"),
        (json!({"character":{"firstMessage":"{{date::YYYY}}"}}), "cbs.local-time"),
        (json!({"preset":{"bias":[["{{screenwidth}}",1]]}}), "cbs.screenwidth"),
        (json!({"character":{"nickname":7}}), "cbs.snapshot"),
        (json!({"character":{"additionalAssets":5}}), "cbs.snapshot"),
        // `descriptionPrefix + desc` adds two values that are not strings, arrays or objects as numbers.
        (json!({"preset":{"promptPreprocess":true},"root":{"descriptionPrefix":1},"character":{"desc":2}}), "character.desc"),
        (json!({"preset":{"promptPreprocess":true},"root":{"descriptionPrefix":null},"character":{"desc":true}}), "character.desc"),
        // Host-state CBS without a Send oracle case.
        (json!({"preset":{"mainPrompt":"{{history}}"}}), "cbs"),
        (json!({"preset":{"promptTemplate":[{"type":"chat","rangeStart":"0","rangeEnd":"end"}]}}), "promptTemplate.range"),
    ]
}

// Stored values TS throws on, which fail the send in admission, with the failure name: a truthy author note, message
// name template or module list that is not a string, and a character bias that `concat` appends as one entry whose
// `v[0]` is not a string.
fn machine_failures() -> Vec<(Value, &'static str)> {
    let chat = json!({"type":"chat","rangeStart":0,"rangeEnd":"end"});
    vec![
        (json!({"conversation":{"note":7}}), "conversation.note"),
        (json!({"preset":{"promptTemplate":[{"type":"plain","type2":"main","text":"Main.","role":"system"}, chat],
            "promptSettings":{"sendName":true},"messageNameTemplate":7}}), "settings.messageNameTemplate"),
        (json!({"preset":{"moduleIntergration":7}}), "moduleIntergration"),
        (json!({"character":{"bias":7}}), "bias"),
        (json!({"character":{"bias":true}}), "bias"),
        (json!({"character":{"bias":null}}), "character.bias"),
    ]
}

// A stored conversation without `fmIndex`, which the shared fixture always has.
fn without_greeting_index(patch: &Value) -> Value {
    let mut database = database(patch);
    database["characters"][0]["chats"][0].as_object_mut().unwrap().remove("fmIndex").unwrap();
    database
}

// The snapshot with the `Date.now()` and `Math.random()` values the case pins in the TS oracles, as a reservation's
// admission records them, and the host capture the Send oracle recorded for a case that pins a time zone.
fn pinned(case: &Value, mut snapshot: PreparationSnapshot) -> PreparationSnapshot {
    use risunest_ir::clock::{Clock, System};
    let random: Vec<f64> = case["random"].as_array().map_or_else(Vec::new, |values| values.iter().map(|value| value.as_f64().unwrap()).collect());
    if case["now"].is_number() || !random.is_empty() {
        snapshot.entropy = Some(Entropy::pinned(case["now"].as_f64().unwrap_or_else(|| System.now_ms()), &random));
    }
    snapshot.host = recorded_host(case);
    snapshot
}

// `captureNativeHost()` as the Send oracle (`benchmarks/rust-core-oracles/prompt.oracle.test.ts`) recorded it in the
// time zone and at the clock the case pins.
pub(crate) fn recorded_host(case: &Value) -> Option<HostCapture> {
    let expected: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/prompt/ollama-text.expected.json")).unwrap();
    let recorded = expected["cases"].as_array().unwrap().iter().find(|entry| entry["id"] == case["id"]).unwrap();
    assert_eq!(case["timeZone"].is_string(), recorded["result"]["host"].is_object(), "{}", case["id"]);
    recorded["result"].get("host").map(|host| serde_json::from_value(host.clone()).unwrap())
}

fn prepare_case(case: &Value) -> Value {
    let (_directory, mut store, intent) = stored(&database(case));
    let snapshot = pinned(case, store.generation_preparation_snapshot(&intent).unwrap());
    serde_json::to_value(prepare_ollama(&snapshot).unwrap()).unwrap()
}

#[test]
fn native_preparation_reads_the_selected_records_without_writes_or_loss() {
    let data = database(&json!({}));
    let (_directory, mut store, intent) = stored(&data);
    let root = store.read_root(None).unwrap().value;
    let preset = store.read_preset("preset-a", None).unwrap().unwrap().value;
    let character = store.read_character("character", None).unwrap().unwrap().value;
    let conversation = store.read_conversation("character", "conversation", None).unwrap().unwrap().value;
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    assert_eq!(snapshot.history.revision, intent.target.expected_revision);
    assert_eq!(snapshot.root, root);
    assert_eq!(snapshot.preset, preset);
    assert_eq!(snapshot.history.character, character);
    assert_eq!(snapshot.history.messages, conversation["message"].as_array().unwrap().clone());
    let prepared = prepare_ollama(&snapshot).unwrap();
    assert_eq!(prepared.messages.first().unwrap().content, "Synthetic main.");
    assert_eq!(prepared.messages.last().unwrap().content, "Synthetic author note.");
    assert_eq!(prepared.last_memory.as_deref(), Some("NewChat"));
    assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
    assert_eq!(store.read_root(None).unwrap().value, root);
    assert_eq!(store.read_conversation("character", "conversation", None).unwrap().unwrap().value, conversation);
    let count: i64 = store.device_store().unwrap().connection().query_row(
        "SELECT count(*) FROM device_settings WHERE key='native-generation-journal-v1'", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn preset_defaults_and_protected_values_override_stale_mirrors() {
    let (_directory, mut store, intent) = stored(&database(&json!({})));
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    snapshot.root["aiModel"] = json!("wrong-root-model");
    snapshot.root["mainPrompt"] = json!("{{inactive-root-mirror}}");
    snapshot.root["maxContext"] = json!(1);
    snapshot.preset.as_object_mut().unwrap().remove("maxContext");
    let prepared = prepare_ollama(&snapshot).unwrap();
    assert_eq!(prepared.max_context, 4000);
    assert_eq!(prepared.messages[0].content, "Synthetic main.");
    snapshot.root["doNotChangeFallbackModels"] = json!(true);
    snapshot.root["protectedPresetValues"] = json!({"fallbackModels":{"model":["active-protected-fallback"]}});
    assert_eq!(prepare_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported("fallbackModels".into()));
}

#[test]
fn the_selected_persona_is_the_one_a_load_selects() {
    let (_directory, mut store, intent) = stored(&database(&json!({})));
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    snapshot.root["username"] = json!("Root user");
    for (selected, user) in [(None, "Synthetic user"), (Some(json!("persona-missing")), "Synthetic user"), (Some(json!(1)), "Bound user"),
        (Some(json!(-1)), "Root user"), (Some(json!(0.5)), "Root user"), (Some(json!(true)), "Root user")] {
        match &selected {
            Some(value) => snapshot.root["selectedPersona"] = value.clone(),
            None => { snapshot.root.as_object_mut().unwrap().remove("selectedPersona"); }
        }
        assert_eq!(prepare_ollama(&snapshot).unwrap().user_name, user, "{selected:?}");
    }
    // `checkPersonaBinded()` binds the persona whose ID is strictly equal to `bindedPersona`; stored IDs are strings, so
    // another value binds none.
    snapshot.root["selectedPersona"] = json!(0);
    for (binded, user) in [(json!("persona-b"), "Bound user"), (json!(5), "Synthetic user"), (json!(true), "Synthetic user"),
        (json!(["persona-b"]), "Synthetic user"), (json!({"id":"persona-b"}), "Synthetic user")] {
        snapshot.history.conversation["bindedPersona"] = binded.clone();
        assert_eq!(prepare_ollama(&snapshot).unwrap().user_name, user, "{binded}");
    }
}

#[test]
fn special_tokens_text_limits_and_insufficient_context_are_observable_errors() {
    for (data, expected) in [
        ("<|endoftext|>".to_owned(), "disallowed_special_token"),
        ("x".repeat(1_048_577), "aggregate_input_limit_exceeded"),
        // `prepare_ollama` skips the machine admission, so this is the path of text only the counting run builds.
        (format!("a{}x", " ".repeat(900_001)), "tokenizer_regex_limit"),
    ] {
        let case = json!({"messages":[{"role":"user","data":data,"chatId":"bounded"}]});
        let (_directory, mut store, intent) = stored(&database(&case));
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(prepare_ollama(&snapshot).unwrap_err(), PreparationError::Tokenizer(expected.into()));
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
    }
    let (_directory, mut store, intent) = stored(&database(&json!({"preset":{"maxContext":1}})));
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    assert_eq!(prepare_ollama(&snapshot).unwrap_err(), PreparationError::ContextLimit);
}

// Admission screens the text the counter will see, so a whitespace run the encoder declines or a text longer than the
// counter's 1 MiB input refuses the reservation before any effect, and text at either bound reserves.
#[test]
fn a_whitespace_run_the_encoder_declines_refuses_the_reservation_before_effects() {
    use crate::native_generation::service::{self, GenerationError};
    let space_run = |run: usize| format!("a{}x", " ".repeat(run));
    for (data, refused) in [(space_run(900_000), None), (space_run(900_001), Some("tokenizer-regex-limit")),
        ("x".repeat(1_048_576), None), ("x".repeat(1_048_577), Some("tokenizer-input-limit"))] {
        let case = json!({"messages":[{"role":"user","data":data}]});
        let (_directory, mut store, intent) = stored(&database(&case));
        let conversation = |store: &PersistentStore| store.read_conversation("character", "conversation", None).unwrap().unwrap().value;
        let before = conversation(&store);
        let reserved = service::reserve_ollama(&mut store, &intent);
        let Some(refused) = refused else { assert!(reserved.is_ok(), "{:?}", reserved.err()); continue };
        assert!(matches!(&reserved, Err(GenerationError::Preparation(PreparationError::Unsupported(name))) if name == refused),
            "{:?}", reserved.err());
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        assert!(service::active_generations(&store).unwrap().is_empty());
        // The message keeps its missing `chatId`: no history ID is written.
        assert_eq!(conversation(&store), before);
    }
}

#[test]
fn preparation_requires_captured_revision_identity_and_existing_preset() {
    let (_directory, mut store, intent) = stored(&database(&json!({})));
    let mut missing = intent.clone(); missing.effective_preset_id = "missing".into();
    assert!(store.generation_preparation_snapshot(&missing).is_err());
    let mut stale = intent.clone(); stale.target.library_identity = "other".into();
    assert!(store.generation_preparation_snapshot(&stale).is_err());
    stale = intent.clone(); stale.target.binding_authority = (intent.target.binding_authority.0 + 1).into();
    assert!(store.generation_preparation_snapshot(&stale).is_err());
    store.commit(&WorkingSetCommit { expected_revision: intent.target.expected_revision,
        root: Some(json!({"unrelated": true})), ..Default::default() }).unwrap();
    assert!(store.generation_preparation_snapshot(&intent).is_err());
}

#[test]
fn preparation_lease_keeps_root_preset_character_and_history_from_one_revision() {
    let (directory, mut store, intent) = stored(&database(&json!({})));
    let mut writer = PersistentStore::open(directory.path()).unwrap();
    let lease = store.acquire_revision(intent.target.expected_revision).unwrap().lease;
    let before = store.generation_preparation_at_lease(&intent, &lease).unwrap();
    let mut preset = before.preset.clone(); preset["mainPrompt"] = json!("Changed main.");
    let mut character = before.history.character.clone(); character["desc"] = json!("Changed description.");
    writer.commit(&WorkingSetCommit { expected_revision: intent.target.expected_revision,
        root_mutations: Some(vec![RootMutation::Set { key: "additionalPrompt".into(), value: json!("Changed extra.") }]),
        replace_presets: Some(vec![preset]), character: Some(character),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "character".into(), conversation_id: "conversation".into(), start: 0, delete_count: 1,
            messages: vec![json!({"role":"user","data":"Changed history.","chatId":"message-a"})],
            conversation: None, configured_index: None,
        }]), ..Default::default() }).unwrap();
    let pinned = store.generation_preparation_at_lease(&intent, &lease).unwrap();
    assert_eq!(pinned.root, before.root); assert_eq!(pinned.preset, before.preset);
    assert_eq!(pinned.history.character, before.history.character);
    assert_eq!(pinned.history.messages, before.history.messages);
    store.release_revision(&lease).unwrap();
    let mut current = intent; current.target.expected_revision = store.revision().unwrap();
    let current = store.generation_preparation_snapshot(&current).unwrap();
    assert_eq!(current.root["additionalPrompt"], "Changed extra.");
    assert_eq!(current.preset["mainPrompt"], "Changed main.");
    assert_eq!(current.history.character["desc"], "Changed description.");
    assert_eq!(current.history.messages[0]["data"], "Changed history.");
}

#[test]
fn prepared_cases_cover_reset_trimming_defaults_and_inactive_settings() {
    // `every_ollama_case_prepares_what_the_recorded_send_sent` pins the refusal of the rows the route refuses.
    for case in cases()["cases"].as_array().unwrap().iter().filter(|case| route_decline(case).is_none()) {
        let prepared = prepare_case(case);
        assert!(prepared["inputTokens"].as_u64().unwrap() <= prepared["maxContext"].as_u64().unwrap(), "{}", case["id"]);
        if case["id"] == "history-reset" {
            assert!(!prepared["messages"].to_string().contains("earlier"));
            assert!(!prepared["messages"].to_string().contains("greeting"));
        }
        if case["id"] == "history-reset" || case["id"] == "ignored-disabled-identities" {
            let messages = prepared["messages"].to_string();
            assert!(!messages.contains("Ignored"));
            assert!(messages.contains("Selected own answer."));
            assert!(messages.contains("Selected question."));
        }
        if case["id"] == "first-trim" { assert_ne!(prepared["lastMemory"], "NewChat"); }
        if case["id"] == "missing-preset-defaults" {
            assert_eq!(prepared["maxContext"], 4000); assert_eq!(prepared["outputTokens"], 500);
        }
        // The final trim keeps U+0085, and the CBS escapes are unescaped after the token recheck.
        if case["id"] == "alternate-unicode" {
            assert!(prepared["messages"].as_array().unwrap().iter().any(|message| message["role"] == "user"
                && message["content"] == "\u{d569}\u{c131} \u{1f98a} \u{85} {escaped}"), "{}", prepared["messages"]);
        }
    }
}

#[test]
fn effective_unsupported_options_are_rejected_before_any_reservation() {
    let patches = route_refusals();
    for (patch, expected) in patches {
        let (_directory, mut store, mut intent) = stored(&database(&patch));
        if expected == "regex.scripts" {
            let now = i64::try_from(now()).unwrap();
            intent.host = Some(HostCapture::new(false, CaseUnicode::Unicode16,
                vec![(now - 400 * 86_400_000, 0)], now + 86_400_000));
        }
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(prepare_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported(expected.into()), "{patch}");
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
    }
}

#[test]
fn next_statics_counts_one_send_exactly_and_keeps_unknown_fields() {
    use crate::native_generation::prepare::next_statics;
    for absent in [json!({}), json!({"statics":null})] { assert_eq!(next_statics(&absent), Some(json!({"messages":1,"imports":0}))); }
    assert_eq!(next_statics(&json!({"statics":{"messages":9007199254740990u64,"imports":3,"future":[1]}})),
        Some(json!({"messages":9007199254740991u64,"imports":3,"future":[1]})));
    for refused in [json!({"statics":{"messages":9007199254740991u64}}), json!({"statics":{"messages":-1}}), json!({"statics":{"messages":1.5}}),
        json!({"statics":{}}), json!({"statics":"many"})] {
        assert_eq!(next_statics(&refused), None, "{refused}");
    }
}

#[test]
#[ignore = "requires the real TypeScript Ollama preparation oracle output"]
fn matches_real_typescript_ollama_oracle() {
    let path = std::env::var_os("RISUNEST_OLLAMA_PREPARATION_ORACLE").expect("synthetic TS oracle path");
    let oracle: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(oracle["schema"], 1);
    assert_eq!(oracle["fixtureSha256"], hex::encode(Sha256::digest(include_bytes!("../../../tests/fixtures/native-generation/ollama-text.json"))));
    let fixture = cases();
    assert_eq!(oracle["cases"].as_array().unwrap().len(), fixture["cases"].as_array().unwrap().len());
    for case in fixture["cases"].as_array().unwrap() {
        let expected = oracle["cases"].as_array().unwrap().iter().find(|entry| entry["id"] == case["id"]).unwrap();
        let (_directory, mut store, intent) = stored(&expected["database"]);
        let snapshot = pinned(case, store.generation_preparation_snapshot(&intent).unwrap());
        let attempt = crate::native_generation::prepare::prepare_ollama_owned(&snapshot);
        if case["observeLore"] == true {
            let observation = &expected["loreObservation"];
            let assignments: Vec<_> = attempt.variables.assignments.iter().map(|(name, value)| json!({"name":name,"value":value})).collect();
            assert_eq!(json!(assignments), observation["assignments"], "{}", case["id"]);
            assert!(observation["requestScriptstate"].is_object() && observation["storedScriptstate"].is_object());
        } else { assert!(expected.get("loreObservation").is_none(), "{}", case["id"]); }
        if case["observeTriggers"] != true { assert!(expected.get("triggerObservation").is_none(), "{}", case["id"]); }
        else if route_decline(case).is_none() {
            // The variables and author note the owner stores with the request are the ones TS held when it sent.
            let observation = &expected["triggerObservation"];
            assert!(observation.get("jsonUnsafe").is_none(), "{}", case["id"]);
            let (scriptstate, note) = stored_after(&snapshot.history.conversation, &attempt.variables);
            assert_eq!(scriptstate, observation["requestScriptstate"], "{}", case["id"]);
            assert_eq!(note, observation["storedNote"], "{}", case["id"]);
            // The history the start trigger left, which TS sent from and stored.
            assert_eq!(case["triggerEditsMessages"] == true, attempt.variables.messages.is_some(), "{}", case["id"]);
            match observation.get("storedMessages") {
                Some(expected) => assert_stored_rows(&messages_after(&snapshot.history.messages, &attempt.variables), expected,
                    &snapshot.history.messages, case["id"].as_str().unwrap()),
                None => assert!(case["triggerEditsMessages"] != true, "{}", case["id"]),
            }
        }
        if let Some(name) = route_decline(case) {
            // The admission that reserves the send declines it; without it, an effect the route cannot store is invalid.
            let refusal = PreparationError::Unsupported(name.into());
            assert_eq!(crate::native_generation::prepare::admit_ollama(&snapshot).map(|_| ()), Err(refusal.clone()), "{}", case["id"]);
            match attempt.result {
                Err(error) if error == refusal => {}
                Err(PreparationError::Invalid(effect)) if name == "effect" && effect.starts_with("effect") => {}
                other => panic!("{}: {other:?}", case["id"]),
            }
            continue;
        }
        let turn = attempt.result.unwrap();
        // The owner sends the request body the TS send sent, byte for byte.
        let body = String::from_utf8(crate::native_generation::service::wire_body(&snapshot, &turn)).unwrap();
        assert_eq!(body, expected["body"].as_str().unwrap(), "{}", case["id"]);
        let mut prepared = serde_json::to_value(&turn).unwrap();
        // The recorded request body carries each message's role and content; the example name only marks the response.
        for message in prepared["messages"].as_array_mut().unwrap() { message.as_object_mut().unwrap().remove("name"); }
        assert_eq!(prepared, expected["prepared"], "{}", case["id"]);
    }
    use crate::native_generation::{owner::OwnerHost, service::{self, PreparedGeneration, ReadyPreparation}};
    let owner_host = OwnerHost::new("RisuNest preparation test").unwrap();
    assert_eq!(oracle["effects"].as_array().unwrap().len(), fixture["effects"].as_array().unwrap().len());
    for case in fixture["effects"].as_array().unwrap() {
        let expected = oracle["effects"].as_array().unwrap().iter().find(|entry| entry["id"] == case["id"]).unwrap();
        assert_eq!(expected["outcome"], case["outcome"]);
        let (_directory, mut store, mut intent) = stored(&expected["database"]);
        if case["observeLore"] == true {
            intent.host = Some(lore_host());
            let (_observation_directory, mut observation_store, mut observation_intent) = stored(&expected["database"]);
            observation_intent.host = intent.host.clone();
            let observation_ticket = service::reserve_ollama(&mut observation_store, &observation_intent).unwrap();
            let crate::native_generation::operation::Start::Started { owner, .. } =
                observation_store.generation_resume_preparation(&observation_ticket).unwrap() else { panic!("{}: observation did not start", case["id"]) };
            assert_eq!(observation_store.generation_ensure_history_ids(&owner).unwrap().phase,
                crate::native_generation::operation::Phase::Running);
            let snapshot = observation_store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
            let attempt = crate::native_generation::prepare::prepare_ollama_owned(&snapshot);
            assert_eq!(attempt.result.unwrap_err(), PreparationError::ContextLimit);
            let assignments: Vec<_> = attempt.variables.assignments.iter().map(|(name, value)| json!({"name":name,"value":value})).collect();
            assert_eq!(json!(assignments), expected["loreObservation"]["assignments"], "{}", case["id"]);
            assert!(expected["loreObservation"]["requestScriptstate"].is_null());
        }
        let triggered = case["observeTriggers"] == true && route_decline(case).is_none();
        if triggered {
            // What the start trigger leaves stored once the send ends: written before the request, or kept by a failure.
            let (_observation_directory, mut observation_store, observation_intent) = stored(&expected["database"]);
            let observation_ticket = service::reserve_ollama(&mut observation_store, &observation_intent).unwrap();
            let crate::native_generation::operation::Start::Started { owner, .. } =
                observation_store.generation_resume_preparation(&observation_ticket).unwrap() else { panic!("{}: observation did not start", case["id"]) };
            observation_store.generation_ensure_history_ids(&owner).unwrap();
            let snapshot = observation_store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
            let attempt = crate::native_generation::prepare::prepare_ollama_owned(&snapshot);
            let observation = &expected["triggerObservation"];
            assert!(observation.get("jsonUnsafe").is_none(), "{}", case["id"]);
            let (scriptstate, note) = stored_after(&snapshot.history.conversation, &attempt.variables);
            assert_eq!(scriptstate, observation["storedScriptstate"], "{}", case["id"]);
            assert_eq!(note, observation["storedNote"], "{}", case["id"]);
        }
        if let Some(name) = route_decline(case) {
            assert!(matches!(service::reserve_ollama(&mut store, &intent),
                Err(service::GenerationError::Preparation(PreparationError::Unsupported(refused))) if refused == name), "{}", case["id"]);
            continue;
        }
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
        if case["outcome"] == "abort" {
            let mut cancellation = store.open_native_job_store().unwrap();
            let cancel_ticket = ticket.clone();
            service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                assert_eq!(service::cancel_generation(&mut cancellation, &cancel_ticket).unwrap().phase,
                    crate::native_generation::operation::Phase::Cancelled);
            })));
        }
        let from = now();
        if triggered {
            // The start trigger's writes belong to the owner, which stores them with the request or the failure.
            let prepared = service::prepare_ready_ollama(&mut store, &ticket, &owner_host).unwrap();
            assert!(matches!((case["outcome"].as_str().unwrap(), &prepared), ("provider-failure", ReadyPreparation::Ready(_))
                | ("context-failure", ReadyPreparation::Failed { error: PreparationError::ContextLimit, .. })), "{}: {prepared:?}", case["id"]);
        } else { match (case["outcome"].as_str().unwrap(), service::prepare_reserved_ollama(&mut store, &ticket).unwrap()) {
            // Provider failure, preflight refusal and cancellation before dispatch occur after this preparation boundary.
            ("success" | "provider-failure" | "network-failure" | "abort-before-request" | "invalid-host", PreparedGeneration::Ready { .. }) => {},
            ("context-failure", PreparedGeneration::Failed { error: PreparationError::ContextLimit, .. }) => {},
            ("abort", PreparedGeneration::Existing(status)) => assert_eq!(status.phase, crate::native_generation::operation::Phase::Cancelled),
            (_, result) => panic!("{}: {result:?}", case["id"]),
        } }
        // The interaction effect commits before ID normalization in every outcome; the memory marker belongs to the owner.
        let effects = &expected["sendEffects"];
        assert_eq!(store.read_root(None).unwrap().value["statics"], effects["statics"]["after"], "{}", case["id"]);
        let interaction = store.read_character("character", None).unwrap().unwrap().value["lastInteraction"].as_u64();
        assert_eq!(Some(interaction.is_some_and(|time| time >= from && time <= now())), effects["lastInteraction"]["afterWithinSend"].as_bool(), "{}", case["id"]);
        let saved = store.read_conversation("character", "conversation", None).unwrap().unwrap().value;
        if case["observeLore"] == true { assert_eq!(saved["scriptstate"], expected["loreObservation"]["storedScriptstate"], "{}", case["id"]); }
        if triggered && case["outcome"] == "context-failure" {
            // A failed preparation stores the trigger's writes itself.
            assert_eq!(saved["scriptstate"], expected["triggerObservation"]["storedScriptstate"], "{}", case["id"]);
            assert_eq!(saved["note"], expected["triggerObservation"]["storedNote"], "{}", case["id"]);
        }
        let rows = saved["message"].as_array().unwrap();
        let expected_rows = expected["storedMessages"].as_array().unwrap();
        assert_eq!(rows.len(), expected_rows.len());
        assert_eq!(expected["assignedIndices"], case["expectedIdAssignments"]);
        for (index, (row, expected_row)) in rows.iter().zip(expected_rows).enumerate() {
            let mut expected_row = expected_row.clone();
            if expected["assignedIndices"].as_array().unwrap().contains(&json!(index)) {
                // Each runtime owns its UUID entropy; only these declared ID assignments differ.
                assert!(!row["chatId"].as_str().unwrap().is_empty());
                expected_row["chatId"] = row["chatId"].clone();
            }
            assert_eq!(row, &expected_row, "{} row {index}", case["id"]);
        }
    }
}

// Inputs the prompt machine prepares (oracle cases `examples-*`, `depth-prompt*`, `template-*`, `utility-bot*`), which
// the route refused before it delegated to the machine.
#[test]
fn machine_prepared_inputs_are_admitted_and_sent() {
    use crate::native_generation::prepare::admit_ollama;
    let template = json!([{"type":"plain","type2":"main","text":"Template main.","role":"system"},
        {"type":"description","innerFormat":"<desc>{{slot}}</desc>"}, {"type":"chat","rangeStart":0,"rangeEnd":"end"}]);
    let cases = [
        json!({"character":{"exampleMessage":"<START>\n{{char}}: Example reply.\n{{user}}: Example question."}}),
        json!({"character":{"depth_prompt":{"depth":0,"prompt":"Depth note."}}}),
        json!({"preset":{"promptTemplate":template}}),
        json!({"character":{"utilityBot":true}}),
    ];
    for (index, patch) in cases.iter().enumerate() {
        let (_directory, mut store, intent) = stored(&database(patch));
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(admit_ollama(&snapshot).map(|_| ()), Ok(()), "{patch}");
        let prepared = prepare_ollama(&snapshot).unwrap();
        let sent = |role: &str, content: &str, name: Option<&str>| prepared.messages.iter()
            .any(|message| message.role == role && message.content == content && message.name.as_deref() == name);
        match index {
            0 => assert!(sent("assistant", "Example reply.", Some("example_assistant")) && sent("user", "Example question.", Some("example_user")),
                "{:?}", prepared.messages),
            1 => assert!(sent("system", "Depth note.", None), "{:?}", prepared.messages),
            2 => {
                assert_eq!(prepared.messages[0].content, "Template main.");
                assert!(sent("system", "<desc>Synthetic description.</desc>", None), "{:?}", prepared.messages);
            }
            _ => assert!(!prepared.messages.iter().any(|message| message.content == "Synthetic author note."), "{:?}", prepared.messages),
        }
    }
}

// Refusals and failures the machine now makes in admission: each comes before any host request, and reservation records
// nothing.
#[test]
fn machine_refusals_come_before_host_requests_and_reservation() {
    use crate::native_generation::{prepare::admission_host_requests, service::{self, GenerationError}};
    let patches = machine_refusals().into_iter().map(|(patch, name)| (patch, PreparationError::Unsupported(name.into())))
        .chain(machine_failures().into_iter().map(|(patch, name)| (patch, PreparationError::Invalid(name.into()))));
    // A Lua trigger would make the machine ask the host for its editRequest listeners, but the start trigger's admission
    // declines it first (`triggers`, which the route also refuses before the machine). With a patch the machine refuses
    // whichever of the two comes first, and neither reaches the host.
    let lua = |patch: &Value| {
        let mut patch = patch.clone();
        patch["character"]["triggerscript"] = json!([{"comment":"","type":"manual","conditions":[],"effect":[{"type":"triggerlua","code":""}]}]);
        let (_directory, mut store, intent) = stored(&database(&patch));
        admission_host_requests(&store.generation_preparation_snapshot(&intent).unwrap())
    };
    let triggers = PreparationError::Unsupported("triggers".into());
    assert_eq!(lua(&json!({})), (Err(triggers.clone()), 0));
    for (patch, refusal) in patches {
        let (refused, requests) = lua(&patch);
        assert!(requests == 0 && (refused == Err(refusal.clone()) || refused == Err(triggers.clone())), "{patch}: {refused:?} {requests}");
        let (_directory, mut store, intent) = stored(&database(&patch));
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(admission_host_requests(&snapshot), (Err(refusal.clone()), 0), "{patch}");
        assert!(matches!(service::reserve_ollama(&mut store, &intent), Err(GenerationError::Preparation(error)) if error == refusal), "{patch}");
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        assert!(service::active_generations(&store).unwrap().is_empty(), "{patch}");
    }
}

// The host capture decides the CBS that reads it: a tailored locale declines locale case mapping and names with `I`,
// tables other than Unicode 16 or 17 or a code point Unicode 16 maps differently decline case mapping beyond ASCII, and
// local time declines outside the captured zone or with a capture that does not describe one. A fitting capture
// reserves.
#[test]
fn the_host_capture_decides_case_mapping_and_local_time() {
    use crate::native_generation::service::{self, GenerationError};
    let reserve = |prompt: &str, host: HostCapture| {
        let (_directory, mut store, mut intent) = stored(&database(&json!({"preset":{"mainPrompt":prompt}})));
        intent.host = Some(host);
        match service::reserve_ollama(&mut store, &intent) {
            Ok(_) => None,
            Err(GenerationError::Preparation(PreparationError::Unsupported(name))) => Some(name),
            Err(error) => panic!("{prompt}: {error:?}"),
        }
    };
    let now = i64::try_from(now()).unwrap();
    let (day, end) = (86_400_000, now + 86_400_000);
    // From more than a year back, which a format's day of the year reads.
    let start = now - 400 * day;
    let capture = |tailored, tables| HostCapture::new(tailored, tables, vec![(start, 3_600_000)], end);
    let zone = |transitions: Vec<(i64, i64)>| HostCapture::new(false, CaseUnicode::Unicode16, transitions, end);
    for (prompt, host, refusal) in [
        ("{{lower::Synthetic}}", capture(true, CaseUnicode::Unicode16), Some("cbs.case-locale")),
        ("{{ISODATE}}", capture(true, CaseUnicode::Unicode16), Some("cbs.case-locale")),
        ("{{lower::Synthetic}} {{UPPER::\u{e9}}} {{ISODATE}}", capture(false, CaseUnicode::Unicode16), None),
        ("{{upper::\u{e9}}}", capture(false, CaseUnicode::From17), Some("cbs.case-table")),
        ("{{capitalize::\u{e9}}}", capture(false, CaseUnicode::Before16), Some("cbs.case-table")),
        ("{{upper::a\u{a7d3}}}", capture(false, CaseUnicode::Unicode16), Some("cbs.case-table")),
        ("{{upper::a\u{a7d3}}} {{lower::\u{c9}}} {{capitalize::\u{e9}}}", capture(false, CaseUnicode::Unicode17), None),
        ("{{upper::\u{e9}}}", capture(false, CaseUnicode::From18), Some("cbs.case-table")),
        ("{{upper::abc}}", capture(false, CaseUnicode::From17), None),
        ("{{upper::abc}}", capture(false, CaseUnicode::From18), None),
        ("{{date}} {{time}} {{date::HH:mm DDDD}}", capture(false, CaseUnicode::Before16), None),
        ("{{date::YYYY::1000}}", capture(false, CaseUnicode::Unicode16), Some("cbs.local-time")),
        // An hour ahead, so the clock cannot pass it while the rows run.
        ("{{date}}", zone(vec![(now + 3_600_000, 0)]), Some("cbs.local-time")),
        ("{{date}}", zone(vec![(start, 0), (start - 1, 0)]), Some("cbs.local-time")),
        ("{{date}}", zone(vec![(start, day)]), Some("cbs.local-time")),
        ("{{date}}", zone(vec![(start, 0), (end, 0)]), Some("cbs.local-time")),
        ("{{date}}", zone(Vec::new()), Some("cbs.local-time")),
        ("{{date}}", zone((0..1025).map(|k| (start + k, 0)).collect()), Some("cbs.local-time")),
        ("{{date}}", HostCapture::new(false, CaseUnicode::Unicode16, vec![(start, 0)], 8_640_000_000_000_001), Some("cbs.local-time")),
        ("{{date}}", zone((0..1024).map(|k| (start + k, 0)).collect()), None),
        ("{{date}}", zone(vec![(start, 0), (now - 1_000, -day + 1)]), None),
    ] {
        assert_eq!(reserve(prompt, host).as_deref(), refusal, "{prompt}");
    }
}

// The model record the CBS stage reads is `getModelInfo('ollama-hosted')` as the TS oracle recorded it.
#[test]
fn the_static_model_record_is_the_recorded_typescript_record() {
    let expected: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/cbs/model-info.expected.json")).unwrap();
    let case = expected["cases"].as_array().unwrap().iter().find(|case| case["id"] == "ollama-hosted").unwrap();
    assert_eq!(case["result"]["json"], crate::native_generation::cbs::OLLAMA_HOSTED);
}

// Stored records reach the machine as `JSON.parse` reads their JSON: property order, numbers and text.
#[test]
fn stored_records_reach_the_machine_as_json_parse_reads_them() {
    use crate::native_generation::prepare::record_value;
    use risunest_ir::{json::stringify, parse_json, Budget, Limits};
    let budget = Budget::new(Limits { max_owned_bytes: 1 << 28, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22,
        max_depth: 128, max_work: usize::MAX / 4 }, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap();
    let text = |value: &risunest_ir::Value| String::from_utf16(&stringify(value, &budget).unwrap().unwrap().units().collect::<Vec<_>>()).unwrap();
    let mut records = vec![database(&json!({}))];
    records.push(serde_json::from_str(r#"{"b":1,"10":"ten","2":[1.5,-0.0,1e300,9007199254740993,0.1],"a":{"01":true,"4294967295":null,
        "4294967294":"last index","x":"�é😀é"},"0":{}}"#).unwrap());
    for record in records {
        let direct = record_value(&record, &budget).unwrap();
        let parsed = parse_json(&serde_json::to_string(&record).unwrap(), &budget).unwrap();
        assert_eq!(text(&direct), text(&parsed));
    }
    let indexed = record_value(&json!({"b":1,"10":2,"2":3,"a":4}), &budget).unwrap();
    assert_eq!(text(&indexed), r#"{"2":3,"10":2,"b":1,"a":4}"#);
}

// TS replaces isolated surrogates before every native save (`nativePersistenceValue.ts`), the native commit decoder
// rejects one that bypasses it, and the replacement character reaches the prompt unchanged.
#[test]
fn isolated_surrogates_never_reach_preparation() {
    let envelope = |text: &str| format!(r#"{{"commit":{{"expectedRevision":1,"root":{{"note":"{text}"}}}},"assetAliases":[]}}"#);
    assert!(crate::persistent_commit_raw::decode_envelope(envelope(r"�").as_bytes()).is_ok());
    for isolated in [r"H\ud800x", r"L\udc00y", r"\udc00\ud800"] {
        assert!(crate::persistent_commit_raw::decode_envelope(envelope(isolated).as_bytes()).is_err(), "{isolated}");
    }
    let data = "H\u{fffd}xL\u{fffd}yP\u{1f600}De\u{301}";
    let (_directory, mut store, intent) = stored(&database(&json!({"messages":[{"role":"user","data":data,"chatId":"repaired"}]})));
    let prepared = prepare_ollama(&store.generation_preparation_snapshot(&intent).unwrap()).unwrap();
    assert!(prepared.messages.iter().any(|message| message.role == "user" && message.content == data), "{:?}", prepared.messages);
}

// `editprocess` scripts run natively at the greeting and at each history message. What the route cannot reproduce
// declines before any host request and reserves nothing: a conversation read, a message write, an emotion, a search
// past the native limits, a pattern the native engine does not compile, and a pattern whose result follows the Unicode
// version when the WebView's RegExp tables are not Unicode 17. A start that excludes regex runs only the character's.
#[test]
fn editprocess_scripts_run_natively_or_decline_before_host_requests() {
    use crate::native_generation::{cbs::RegexUnicode, prepare::admission_host_requests, service::{self, GenerationError}};
    let now = i64::try_from(now()).unwrap();
    let host = |unicode, excluded| HostCapture::new(false, CaseUnicode::Unicode16, vec![(now - 400 * 86_400_000, 0)], now + 86_400_000)
        .with_regex(unicode, excluded);
    let script = |pattern: &str, out: &str, flag: &str| json!({"type":"editprocess","in":pattern,"out":out,"flag":flag,"ableFlag":!flag.is_empty()});
    let reserve = |patch: &Value, host: HostCapture| {
        let (directory, store, mut intent) = stored(&database(patch));
        intent.host = Some(host);
        (directory, store, intent)
    };
    let declines = [
        (json!({}), script("absent", "@@repeat_back end", ""), RegexUnicode::Unicode17, "regex.repeatBack"),
        (json!({}), script("question", "@@inject", ""), RegexUnicode::Unicode17, "regex.inject"),
        (json!({}), script("greeting", "@@emo happy", ""), RegexUnicode::Unicode17, "regex.emo"),
        (json!({"firstMessage":"a".repeat(40)}), script("(a+)+b", "x", ""), RegexUnicode::Unicode17, "regex.limit"),
        (json!({}), script("[a]", "x", "gv"), RegexUnicode::Unicode17, "regex.engine"),
        (json!({}), script("\\p{L}", "x", "gu"), RegexUnicode::Before17, "regex.unicode"),
        (json!({}), script("\u{e9}", "x", "gi"), RegexUnicode::From18, "regex.unicode"),
    ];
    for (character, script, unicode, refusal) in declines {
        let mut character = character;
        character["customscript"] = json!([script]);
        let patch = json!({"character":character});
        let refusal = PreparationError::Unsupported(refusal.into());
        let (_directory, mut store, intent) = reserve(&patch, host(unicode, false));
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(admission_host_requests(&snapshot), (Err(refusal.clone()), 0), "{patch}");
        assert!(matches!(service::reserve_ollama(&mut store, &intent), Err(GenerationError::Preparation(error)) if error == refusal), "{patch}");
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        assert!(service::active_generations(&store).unwrap().is_empty(), "{patch}");
    }
    let prepared = |patch: &Value, host: HostCapture| {
        let (_directory, mut store, intent) = reserve(patch, host);
        prepare_ollama(&store.generation_preparation_snapshot(&intent).unwrap()).unwrap().messages
    };
    let greeting = |messages: &[crate::native_generation::prepare::PreparedMessage], expected: &str| {
        assert!(messages.iter().any(|message| message.role == "assistant" && message.content == expected), "{expected}: {messages:?}");
    };
    // Unicode 17 tables run the patterns other tables decline.
    let unicode = json!({"character":{"customscript":[script("\\p{Lu}", "U", "gu"), script("\u{c9}", "e", "gi")],
        "firstMessage":"Synthetic \u{e9}greeting."}});
    greeting(&prepared(&unicode, host(RegexUnicode::Unicode17, false)), "Uynthetic egreeting.");
    // `presetRegex` runs before the character's scripts, and not at all in a start that excludes regex.
    let ordered = json!({"preset":{"regex":[script("greeting", "PRESET", "")]},"character":{"customscript":[script("Synthetic", "CHAR", "")]}});
    greeting(&prepared(&ordered, host(RegexUnicode::Before17, false)), "CHAR PRESET.");
    greeting(&prepared(&ordered, host(RegexUnicode::Before17, true)), "CHAR greeting.");
}

// Every refusal name the route can return maps to the lane that removes it.
#[test]
fn every_admission_refusal_is_in_the_ledger() {
    use std::collections::BTreeSet;
    // Names built at run time: boolean options, host refusals and L05's primary/secondary key selector.
    // L05 forwards PatternEngine's CompileError::Unsupported limits and the compatibility compiler's table refusals.
    const BUILT: [&str; 15] = ["ttsAutoSpeech", "removeIncompleteResponse", "autoContinueChat", "autoTranslate", "inlayErrorResponse",
        "lua", "plugins", "hostRequest", "lore-key", "lore-secondkey", "regex-pattern-units", "regex-alternation-depth",
        "regex-compiled-bytes", "unicode_sets", "regex_nesting_depth"];
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let ledger: Value = serde_json::from_slice(&std::fs::read(manifest.join("../tests/fixtures/rust-core/admission-ledger.json")).unwrap()).unwrap();
    let listed: BTreeSet<String> = ledger["refusals"].as_object().unwrap().keys().cloned().collect();
    for (name, entry) in ledger["refusals"].as_object().unwrap() {
        assert!(entry["lane"].as_str().is_some_and(|lane| !lane.is_empty()), "{name}");
    }
    let tests = regex::Regex::new(r"(?m)^#\[cfg\(test\)\]\r?\n(?:pub(?:\(crate\))? )?mod \w+ \{").unwrap();
    // Each form separately, since a field-named `literal` call can hold an `unsupported` call. The prompt helpers that
    // convert a stored value take the name they refuse with as an argument.
    let refusals = [r#"unsupported\("([^"]+)"\)"#, r#"Unsupported\("([^"]+)""#, r#"Unsupported\(Cow::Borrowed\("([^"]+)"\)\)"#,
        r#"\bliteral\([^;\n]*?, "([A-Za-z]+)"\)\?"#, r#"\b(?:js::string|js::number|text_or)\([^;\n]*?, "([A-Za-z.]+)", &?budget\)"#,
        r#"\bto_number\([^;\n]*?, "([A-Za-z-]+)", budget\)"#, r#"\.truthy_text\("([A-Za-z-]+)"\)"#]
        .map(|pattern| regex::Regex::new(pattern).unwrap());
    let mut sources = vec![manifest.join("src/persistent_store/generation.rs")];
    for directory in ["src/native_generation", "../crates/core/prompt/src", "../crates/core/records/src", "../crates/core/lorebook/src"] {
        for entry in std::fs::read_dir(manifest.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap();
            if name.ends_with(".rs") && !name.ends_with("_tests.rs") { sources.push(path); }
        }
    }
    let mut found = BTreeSet::new();
    for path in sources {
        let source = std::fs::read_to_string(&path).unwrap();
        let production = tests.find(&source).map_or(source.as_str(), |test| &source[..test.start()]);
        for pattern in &refusals {
            found.extend(pattern.captures_iter(production).map(|captures| captures[1].to_owned()));
        }
    }
    found.extend(BUILT.iter().map(|name| (*name).to_owned()));
    assert_eq!(found.difference(&listed).collect::<Vec<_>>(), Vec::<&String>::new(), "refusals missing from the ledger");
    assert_eq!(listed.difference(&found).collect::<Vec<_>>(), Vec::<&String>::new(), "ledger names no source refuses");
}

// `characterFormatUpdate` sets a missing greeting index to the character's `firstMsgIndex`, then -1.
#[test]
fn a_missing_greeting_index_reads_the_character_default() {
    let cases = [
        (without_greeting_index(&json!({})), "Synthetic greeting."),
        (without_greeting_index(&json!({"character":{"firstMsgIndex":0}})), "Alternate greeting."),
        (database(&json!({"conversation":{"fmIndex":null},"character":{"firstMsgIndex":0}})), "Alternate greeting."),
    ];
    for (database, greeting) in cases {
        let (_directory, mut store, intent) = stored(&database);
        let prepared = prepare_ollama(&store.generation_preparation_snapshot(&intent).unwrap()).unwrap();
        let greetings: Vec<&str> = prepared.messages.iter().map(|message| message.content.as_str())
            .filter(|content| ["Synthetic greeting.", "Alternate greeting."].contains(content)).collect();
        assert_eq!(greetings, [greeting], "{:?}", prepared.messages);
    }
}

// `characterFormatUpdate` gives a chat without a truthy `id` a random one before the send, and `pick` and `rollp` hash
// `chaId + chat.id`, so a snapshot without one refuses those two and admits the other functions. The store keeps a
// nonempty id, so the test edits the snapshot.
#[test]
fn a_chat_without_an_id_refuses_only_the_hash_picks() {
    use crate::native_generation::prepare::admit_ollama;
    for (id, hashable) in [(Some(json!("conversation")), true), (None, false), (Some(json!("")), false)] {
        for (main, hashed) in [("{{pick::a::b}}", true), ("{{rollp::2d6}}", true), ("{{char}}", false)] {
            let (_directory, mut store, intent) = stored(&database(&json!({"preset":{"mainPrompt":main}})));
            let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
            let conversation = snapshot.history.conversation.as_object_mut().unwrap();
            match &id { Some(id) => { conversation.insert("id".into(), id.clone()); } None => { conversation.remove("id"); } }
            let refusal = (hashed && !hashable).then(|| PreparationError::Unsupported("cbs.snapshot".into()));
            assert_eq!(admit_ollama(&snapshot).err(), refusal, "{main} {id:?}");
        }
    }
}

// A reserved send prepares, or fails only on its context limit or the tokenizer: admission refuses everything else.
#[test]
fn every_reserved_send_prepares() {
    use crate::native_generation::{owner::OwnerHost, service::{self, GenerationError, PreparedGeneration, ReadyPreparation}};
    let fixture = cases();
    let owner_host = OwnerHost::new("RisuNest preparation test").unwrap();
    let mut inputs = Vec::new();
    // A case that pins a time zone reserves with the capture its Send oracle recorded, and a trigger row the route
    // declines is refused with the name its `decline` holds. A row whose lore or start trigger writes variables prepares
    // through the owner, which stores them.
    for case in fixture["cases"].as_array().unwrap() {
        let decline = route_decline(case).map(str::to_owned);
        let owns_effects = case["observeLore"] == true || case["observeTriggers"] == true;
        inputs.push((case["id"].to_string(), database(case), recorded_host(case), owns_effects, decline));
    }
    for case in fixture["effects"].as_array().unwrap() {
        let mut case = case.clone();
        if case.get("messages").is_none() { case["messages"] = fixture["effectMessages"].clone(); }
        let owns_lore = case["observeLore"] == true;
        let decline = route_decline(&case).map(str::to_owned);
        let owns_effects = owns_lore || case["observeTriggers"] == true;
        inputs.push((case["id"].to_string(), database(&case), owns_lore.then(lore_host), owns_effects, decline));
    }
    for (patch, _) in machine_refusals().into_iter().chain(machine_failures()).chain(route_refusals()) {
        inputs.push((patch.to_string(), database(&patch), None, false, None));
    }
    for patch in [json!({}), json!({"character":{"firstMsgIndex":0}}), json!({"character":{"firstMessage":"{{char}}"}})] {
        inputs.push((format!("without fmIndex {patch}"), without_greeting_index(&patch), None, false, None));
    }
    let (mut reserved, mut refused) = (0, 0);
    for (label, database, host, owns_effects, decline) in inputs {
        let (_directory, mut store, mut intent) = stored(&database);
        intent.host = host;
        let ticket = match (service::reserve_ollama(&mut store, &intent), decline) {
            (Err(GenerationError::Preparation(PreparationError::Unsupported(name))), Some(decline)) if name == decline => { refused += 1; continue; }
            (Ok(ticket), None) => ticket,
            (Err(error), None) => { assert!(!owns_effects, "{label}: {error:?}"); continue; }
            (result, decline) => panic!("{label}: want the refusal {decline:?}, got {:?}", result.map(|_| ())),
        };
        reserved += 1;
        if owns_effects {
            let prepared = service::prepare_ready_ollama(&mut store, &ticket, &owner_host).unwrap();
            assert!(matches!(prepared, ReadyPreparation::Ready(_)
                | ReadyPreparation::Failed { error: PreparationError::ContextLimit | PreparationError::Tokenizer(_), .. }), "{label}: {prepared:?}");
        } else {
            let prepared = service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
            assert!(matches!(prepared, PreparedGeneration::Ready { .. }
                | PreparedGeneration::Failed { error: PreparationError::ContextLimit | PreparationError::Tokenizer(_), .. }), "{label}: {prepared:?}");
        }
    }
    // The 10 trigger rows the route declines are refused with their `decline`; at least as many inputs as the cases the
    // route admits, plus two, reserve.
    let admitted = fixture["cases"].as_array().unwrap().iter().filter(|case| route_decline(case).is_none()).count();
    assert_eq!(refused, 10);
    assert!(reserved >= admitted + 2, "{reserved}");
}

// A random branch reaches a declined function in a send's preparation exactly when it does in the admission that
// reserved the send: the reservation records the admission's draws, and later runs replay them and fail once they run
// out.
#[test]
fn a_random_branch_declines_in_admission_and_preparation_alike() {
    use crate::native_generation::{prepare::admit_ollama, service::{self, GenerationError, PreparedGeneration}};
    let gated = json!({"preset":{"mainPrompt":"{{#if {{equal::{{randint::1::2}}::1}}}}{{screenwidth}}{{/if}}Main."}});
    let (_directory, mut store, intent) = stored(&database(&gated));
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    for (draw, declined) in [(0.0, true), (0.9, false)] {
        snapshot.entropy = Some(Entropy::pinned(0.0, &[draw]));
        let refusal = || PreparationError::Unsupported("cbs.screenwidth".into());
        assert_eq!(admit_ollama(&snapshot).err(), declined.then(refusal), "{draw}");
        assert_eq!(prepare_ollama(&snapshot).err(), declined.then(refusal), "{draw}");
    }
    snapshot.entropy = Some(Entropy::pinned(0.0, &[]));
    assert_eq!(prepare_ollama(&snapshot).err(), Some(PreparationError::Invalid("limit:rng-tape".into())));
    let (mut prepared, mut refused) = (0, 0);
    for _ in 0..512 {
        if prepared >= 16 && refused >= 1 { break; }
        let (_directory, mut store, intent) = stored(&database(&gated));
        match service::reserve_ollama(&mut store, &intent) {
            Err(GenerationError::Preparation(error)) => {
                assert_eq!(error, PreparationError::Unsupported("cbs.screenwidth".into()));
                refused += 1;
            }
            Ok(ticket) => {
                let outcome = service::prepare_reserved_ollama(&mut store, &ticket).unwrap();
                assert!(matches!(outcome, PreparedGeneration::Ready { .. }), "{outcome:?}");
                prepared += 1;
            }
            Err(error) => panic!("{error:?}"),
        }
    }
    assert!(prepared >= 16 && refused >= 1, "{prepared} prepared, {refused} refused");
    // A memory card repeats with the trimmed history, so a send that also draws refuses at reservation.
    let memory = |inner: &str| json!({"preset":{"promptTemplate":[{"type":"memory","innerFormat":inner},{"type":"chat","rangeStart":0,"rangeEnd":"end"}]},
        "messages":[{"role":"user","chatId":"supaMemory","data":"Summary."},{"role":"user","chatId":"question","data":"Question."}]});
    let (_directory, mut store, intent) = stored(&database(&memory("{{random::a::b}} {{slot}}")));
    assert!(matches!(service::reserve_ollama(&mut store, &intent),
        Err(GenerationError::Preparation(PreparationError::Unsupported(name))) if name == "cbs.random-draws"));
    let (_directory, mut store, intent) = stored(&database(&memory("Memory: {{slot}}")));
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), PreparedGeneration::Ready { .. }));
}

// A stored value the CBS snapshot cannot hold refuses only a send that calls a function reading it, where TS throws.
// A stored shape TS reads as another value prepares what that value prepares.
#[test]
fn stored_values_no_called_function_reads_are_admitted() {
    use crate::native_generation::service::{self, PreparedGeneration};
    let chat = json!({"type":"chat","rangeStart":0,"rangeEnd":"end"});
    let without = |patch: &Value, record: &str, key: &str| {
        let mut database = database(patch);
        let record = if record == "character" { &mut database["characters"][0] } else { &mut database["personas"][1] };
        record.as_object_mut().unwrap().remove(key).unwrap();
        database
    };
    let reset = |first: Value| json!([first, {"role":"char","chatId":"reset","disabled":"allBefore","data":"Reset."},
        {"role":"user","chatId":"selected","data":"Selected question."}]);
    let named = |name: Value| json!({"preset":{"promptTemplate":[{"type":"plain","type2":"main","text":"Main.","role":"system"}, chat.clone()],
        "promptSettings":{"sendName":true},"messageNameTemplate":name}});
    let bound = |name: Value| json!({"root":{"personas":[persona("persona-a", json!("Synthetic user"), json!("Synthetic persona.")),
        persona("persona-b", name, json!("Bound persona."))]},"conversation":{"bindedPersona":"persona-b"}});
    let unbound = |name: Value| json!({"root":{"personas":[persona("persona-a", name, json!("Synthetic persona.")),
        persona("persona-b", json!("Bound user"), json!("Bound persona."))]}});
    let cases = [
        (database(&json!({"character":{"alternateGreetings":[null]}})), None),
        (database(&json!({"character":{"alternateGreetings":{"0":"Greeting."}}})), None),
        (database(&json!({"preset":{"promptTemplate":[5, "authornote", {"type":"authornote","defaultText":7}, chat.clone()]}})), None),
        // `{{authornote}}` reads the template only when the chat note is not truthy.
        (database(&json!({"preset":{"promptTemplate":[{"type":"authornote","defaultText":7},
            {"type":"plain","type2":"normal","text":"Note {{authornote}}.","role":"system"}, chat.clone()]},"conversation":{"note":"Chat note."}})), None),
        (database(&json!({"conversation":{"scriptstate":"text"}})), None),
        (database(&json!({"conversation":{"scriptstate":{"$obj":{"a":1},"$list":[1,null]}},"preset":{"mainPrompt":"{{getvar::obj}}"}})), None),
        (database(&json!({"conversation":{"scriptstate":{"$odd":{"toString":"x"}}}})), Some(database(&json!({})))),
        // `characterFormatUpdate` gives a character without `triggerscript` an empty list.
        (without(&json!({}), "character", "triggerscript"), Some(database(&json!({})))),
        // A falsy note, preset chain, module list or name template reads as an empty one.
        (database(&json!({"conversation":{"note":0}})), Some(database(&json!({"conversation":{"note":""}})))),
        (database(&json!({"conversation":{"note":false}})), Some(database(&json!({"conversation":{"note":""}})))),
        (database(&json!({"root":{"presetChain":0}})), Some(database(&json!({"root":{"presetChain":""}})))),
        (database(&json!({"preset":{"moduleIntergration":0}})), Some(database(&json!({"preset":{"moduleIntergration":""}})))),
        (database(&named(json!(0))), Some(database(&named(json!(""))))),
        // `'' + desc` concatenates whatever `desc` holds, and so does a string `descriptionPrefix`.
        (without(&json!({}), "character", "desc"), Some(database(&json!({"character":{"desc":"undefined"}})))),
        (database(&json!({"character":{"desc":null}})), Some(database(&json!({"character":{"desc":"null"}})))),
        (database(&json!({"preset":{"promptPreprocess":true},"character":{"desc":2}})),
            Some(database(&json!({"preset":{"promptPreprocess":true},"character":{"desc":"2"}})))),
        // A reset leaves the greeting unread, and the history reads neither the role nor the ID of a disabled row or one
        // before the reset.
        (database(&json!({"character":{"alternateGreetings":"Greeting text"},"conversation":{"fmIndex":0},"messages":reset(json!({"role":"user","chatId":"before","data":"Before."}))})),
            Some(database(&json!({"conversation":{"fmIndex":0},"messages":reset(json!({"role":"user","chatId":"before","data":"Before."}))})))),
        (database(&json!({"conversation":{"fmIndex":0.5},"messages":reset(json!({"role":"user","chatId":"before","data":"Before."}))})),
            Some(database(&json!({"messages":reset(json!({"role":"user","chatId":"before","data":"Before."}))})))),
        (database(&json!({"messages":reset(json!({"role":7,"chatId":7,"data":"Before."}))})),
            Some(database(&json!({"messages":reset(json!({"role":"char","chatId":"before","data":"Before."}))})))),
        (database(&json!({"messages":[{"role":7,"chatId":7,"disabled":true,"data":"Off."},{"role":"user","chatId":"question","data":"Question."}]})),
            Some(database(&json!({"messages":[{"role":"char","chatId":"off","disabled":true,"data":"Off."},{"role":"user","chatId":"question","data":"Question."}]})))),
        // A bound persona's name is the user name as stored, so a nullish one adds no response marker; an unbound one
        // reads as `User`.
        (database(&bound(Value::Null)), Some(database(&bound(json!(""))))),
        (without(&bound(json!("")), "persona", "name"), Some(database(&bound(json!(""))))),
        (database(&unbound(Value::Null)), Some(database(&unbound(json!("User"))))),
        // `JSON.parse` reads a stored `4000.0` as the whole number 4000.
        (database(&json!({"preset":{"maxContext":4000.0}})), Some(database(&json!({"preset":{"maxContext":4000}})))),
    ];
    let prepare = |database: &Value| {
        let (_directory, mut store, intent) = stored(database);
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap_or_else(|error| panic!("{database}: {error:?}"));
        match service::prepare_reserved_ollama(&mut store, &ticket).unwrap() {
            PreparedGeneration::Ready { prepared, .. } => serde_json::to_value(prepared).unwrap(),
            outcome => panic!("{database}: {outcome:?}"),
        }
    };
    for (database, same) in cases {
        let prepared = prepare(&database);
        if let Some(same) = same { assert_eq!(prepared, prepare(&same), "{database}"); }
    }
}

// The committed Send oracle (`benchmarks/rust-core-oracles/prompt.oracle.test.ts`) for each Ollama case: the stored records
// real `sendChat` read prepare the messages, token counts and memory marker it sent, with the CBS of the `cbs-*` cases
// evaluated under the clock and random draws the case pins, and the start triggers the route runs. A trigger row the route
// declines is refused by the admission that would reserve it, with the refusal its `decline` names.
#[test]
fn every_ollama_case_prepares_what_the_recorded_send_sent() {
    use crate::native_generation::prepare::admit_ollama;
    let expected: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/prompt/ollama-text.expected.json")).unwrap();
    let fixture = cases();
    let (recorded, params) = (expected["cases"].as_array().unwrap(), fixture["cases"].as_array().unwrap());
    assert_eq!(recorded.len(), params.len());
    let base = &recorded[0]["result"]["database"]["root"];
    let mut refused = 0;
    for (case, recorded) in params.iter().zip(recorded) {
        assert_eq!(case["id"], recorded["id"]);
        let (result, view) = (&recorded["result"], &recorded["result"]["database"]);
        assert_eq!(result["sent"], true, "{}", case["id"]);
        let mut root = view.get("root").cloned().unwrap_or_else(|| {
            let mut root = base.clone();
            for (key, value) in view["rootChanges"]["set"].as_object().unwrap() { root[key] = value.clone(); }
            for key in view["rootChanges"]["removed"].as_array().unwrap() { root.as_object_mut().unwrap().remove(key.as_str().unwrap()); }
            root
        });
        let mut character = view["character"].clone();
        character["chats"] = json!([view["conversation"]]);
        root["characters"] = json!([character]);
        root["botPresets"] = json!([view["preset"]]);
        let (_directory, mut store, intent) = stored(&root);
        let snapshot = pinned(case, store.generation_preparation_snapshot(&intent).unwrap());
        if let Some(name) = route_decline(case) {
            let refusal = PreparationError::Unsupported(name.into());
            assert_eq!(admit_ollama(&snapshot).map(|_| ()), Err(refusal.clone()), "{}", case["id"]);
            // Without that admission the preparation fails as well: an effect the route cannot store is invalid there.
            match prepare_ollama(&snapshot) {
                Err(error) if error == refusal => {}
                Err(PreparationError::Invalid(effect)) if name == "effect" && effect.starts_with("effect") => {}
                other => panic!("{}: {other:?}", case["id"]),
            }
            refused += 1;
            continue;
        }
        let prepared = prepare_ollama(&snapshot).unwrap_or_else(|error| panic!("{}: {error:?}", case["id"]));
        let messages: Vec<Value> = prepared.messages.iter().map(|message| json!({"role": message.role, "content": message.content})).collect();
        assert_eq!(Value::Array(messages), result["messages"], "{}", case["id"]);
        assert_eq!(json!({"inputTokens": prepared.input_tokens, "outputTokens": prepared.output_tokens, "maxContext": prepared.max_context}),
            result["generation"], "{}", case["id"]);
        assert_eq!(json!(prepared.last_memory), result["lastMemory"], "{}", case["id"]);
    }
    assert_eq!(refused, 9);
}

// The conversation's `scriptstate` and `note` once the send stores `plan`, as its Variables effect writes them.
fn stored_after(conversation: &Value, plan: &crate::native_generation::preparation_effects::PreparedVariablePlan) -> (Value, Value) {
    let mut scriptstate = conversation.get("scriptstate").cloned().unwrap_or(Value::Null);
    if !plan.assignments.is_empty() {
        if scriptstate.is_null() { scriptstate = json!({}); }
        for (name, value) in &plan.assignments { scriptstate[format!("${name}")] = value.clone(); }
    }
    let note = plan.note.as_ref().map_or_else(|| conversation.get("note").cloned().unwrap_or(Value::Null), |note| note.value.clone());
    (scriptstate, note)
}

// Every start trigger row of the Send oracle, through the admission that reserves a send and the owner's preparation of
// the reserved send: a row the route runs reserves, and its admission converts the variables and author note the
// preparation then stores; a row the route declines refuses with its `decline` and reserves nothing. Neither makes a
// host request.
#[test]
fn start_trigger_rows_run_natively_or_decline_before_host_requests() {
    use crate::native_generation::prepare::{admission_host_requests, admission_variable_plan, admit_ollama, prepare_ollama_owned};
    use crate::native_generation::{operation::Start, service::{self, GenerationError}};
    let fixture = cases();
    let effects = fixture["effects"].as_array().unwrap().iter().map(|case| {
        let mut case = case.clone();
        if case.get("messages").is_none() { case["messages"] = fixture["effectMessages"].clone(); }
        case
    });
    let rows: Vec<Value> = fixture["cases"].as_array().unwrap().iter().cloned().chain(effects)
        .filter(|case| case["observeTriggers"] == true).collect();
    let (mut native, mut declined) = (0, 0);
    for case in &rows {
        let (_directory, mut store, mut intent) = stored(&database(case));
        // An effects row has no Send oracle entry and pins nothing.
        let recorded = case.get("outcome").is_none();
        if recorded { intent.host = recorded_host(case); }
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        let snapshot = if recorded { pinned(case, snapshot) } else { snapshot };
        assert_eq!(admission_host_requests(&snapshot).1, 0, "{}", case["id"]);
        match route_decline(case) {
            Some(name) => {
                let refusal = PreparationError::Unsupported(name.into());
                assert_eq!(admit_ollama(&snapshot).map(|_| ()), Err(refusal.clone()), "{}", case["id"]);
                assert!(matches!(service::reserve_ollama(&mut store, &intent), Err(GenerationError::Preparation(error)) if error == refusal),
                    "{}", case["id"]);
                assert_eq!(store.revision().unwrap(), intent.target.expected_revision, "{}", case["id"]);
                declined += 1;
            }
            None => {
                assert_eq!(admit_ollama(&snapshot).map(|_| ()), Ok(()), "{}", case["id"]);
                let plan = admission_variable_plan(&snapshot).unwrap();
                // The owner prepares from its snapshot of the reserved send, with the message IDs it assigns, under the
                // draws the row pins.
                let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
                let Start::Started { owner, .. } = store.generation_resume_preparation(&ticket).unwrap() else { panic!("{}", case["id"]) };
                store.generation_ensure_history_ids(&owner).unwrap();
                let mut owned = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
                if recorded { owned.entropy = snapshot.entropy.clone(); }
                let attempt = prepare_ollama_owned(&owned);
                let failure = case["outcome"] == "context-failure";
                assert!(match &attempt.result { Ok(_) => !failure, Err(error) => failure && *error == PreparationError::ContextLimit },
                    "{}: {:?}", case["id"], attempt.result.as_ref().err());
                // The admission and the owned run each draw the IDs of the messages the trigger appends; nothing else differs.
                let (admitted, probed) = without_appended_ids(&plan, &snapshot.history.messages);
                let (prepared, drawn) = without_appended_ids(&attempt.variables, &owned.history.messages);
                assert_eq!(admitted, prepared, "{}", case["id"]);
                assert_eq!(probed.len(), drawn.len(), "{}", case["id"]);
                assert!(probed.iter().all(|id| !drawn.contains(id)), "{}", case["id"]);
                if case["triggerEditsMessages"] == true { assert!(attempt.variables.messages.is_some(), "{}", case["id"]); }
                native += 1;
            }
        }
    }
    assert_eq!((native, declined), (18, 10));
}

// The start trigger reads the selected preset's template defaults and module selection, maps case with the tables the
// WebView reported, and takes appended message IDs apart from the send's recorded draws. What TS throws on declines
// before any host request.
#[test]
fn the_start_trigger_reads_the_effective_records_and_declines_before_host_requests() {
    use crate::native_generation::prepare::{admission_host_requests, admit_ollama, prepare_ollama_owned};
    let start = |effects: Value| json!([{"comment":"","type":"start","conditions":[],"effect":effects}]);
    let snapshot_of = |database: &Value, host: Option<HostCapture>| {
        let (_directory, mut store, mut intent) = stored(database);
        intent.host = host;
        store.generation_preparation_snapshot(&intent).unwrap()
    };
    let assignments = |snapshot: &PreparationSnapshot| {
        let attempt = prepare_ollama_owned(snapshot);
        attempt.result.unwrap();
        attempt.variables.assignments
    };
    let triggers = || Some(PreparationError::Unsupported("triggers".into()));
    let fixture = cases();
    let row = |id: &str| database(fixture["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap());
    // The template defaults of the selected preset, over the stored root mirror.
    assert_eq!(assignments(&snapshot_of(&row("trigger-template-default-preset"), None)),
        [("readTmpl".to_owned(), json!("preset-effective")), ("readBoth".to_owned(), json!("character-default"))]);
    // A module the stored root mirror names but the selected preset does not select adds no trigger; selected, the
    // route refuses it.
    let module = json!({"id":"mirror","name":"Mirror","trigger":start(json!([
        {"type":"v2SetVar","operator":"=","var":"fromMirror","valueType":"value","value":"yes","indent":0}]))});
    let integration = |selected: &str| database(&json!({"root":{"modules":[module],"enabledModules":[],"moduleIntergration":"mirror"},
        "preset":{"moduleIntergration":selected}}));
    assert_eq!(assignments(&snapshot_of(&integration(""), None)), Vec::<(String, Value)>::new());
    assert_eq!(admit_ollama(&snapshot_of(&integration("mirror"), None)).err(), Some(PreparationError::Unsupported("modules".into())));
    // Case beyond ASCII maps with the Unicode 17 tables the WebView reported; other tables, or none, decline.
    let upper = database(&json!({"character":{"triggerscript":start(json!([
        {"type":"v2ToUpperCase","outputVar":"upper","source":"\u{e0}b","sourceType":"value","indent":0}]))}}));
    let now = i64::try_from(now()).unwrap();
    let host = |unicode| Some(HostCapture::new(false, unicode, vec![(now - 400 * 86_400_000, 0)], now + 86_400_000));
    assert_eq!(assignments(&snapshot_of(&upper, host(CaseUnicode::Unicode17))), [("upper".to_owned(), json!("\u{c0}B"))]);
    assert_eq!(admit_ollama(&snapshot_of(&upper, host(CaseUnicode::From18))).err(), triggers());
    assert_eq!(admit_ollama(&snapshot_of(&upper, None)).err(), triggers());
    // `{{getvar}}` of a truthy number the trigger read throws a TypeError in TS, and of an object holding `var` reads that
    // `var`; both decline. A string reads as written.
    let read = |element: &str| database(&json!({"conversation":{"scriptstate":{"$vals":format!("[{element}]")}},
        "character":{"triggerscript":start(json!([
            {"type":"v2GetArrayVar","var":"vals","index":"0","indexType":"value","outputVar":"read","indent":0},
            {"type":"v2SystemPrompt","location":"start","value":"Read {{getvar::read}}.","valueType":"value","indent":0}]))}}));
    for element in ["1.5", r#"{"text":"t","var":{"x":"1"}}"#] {
        assert_eq!(admission_host_requests(&snapshot_of(&read(element), None)), (Err(PreparationError::Unsupported("triggers".into())), 0), "{element}");
    }
    let text = snapshot_of(&read("\"a\""), None);
    assert_eq!(admit_ollama(&text).map(|_| ()), Ok(()));
    assert!(prepare_ollama(&text).unwrap().messages.iter().any(|message| message.content == "Read a."));
    // A message the trigger appends takes its ID from operating-system entropy: a send that recorded no draws admits it.
    let mut impersonate = snapshot_of(&row("trigger-impersonate"), None);
    impersonate.entropy = Some(Entropy::pinned(0.0, &[]));
    assert_eq!(admit_ollama(&impersonate).map(|_| ()), Ok(()));
}

// The IDs of the messages a start trigger appends differ between the admission and the owned run, and between two owned
// runs, so no admitted stage reads them: two preparations of one reserved send build the same request and store the same
// edits apart from those IDs. A history the prompt would read them through declines before reservation.
#[test]
fn appended_message_ids_reach_no_admitted_stage() {
    use crate::native_generation::prepare::{admission_host_requests, admit_ollama, prepare_ollama_owned};
    use crate::native_generation::{operation::Start, service};
    let fixture = cases();
    let case = fixture["cases"].as_array().unwrap().iter().find(|case| case["id"] == "trigger-impersonate").unwrap();
    let (_directory, mut store, mut intent) = stored(&database(case));
    intent.host = recorded_host(case);
    let snapshot = pinned(case, store.generation_preparation_snapshot(&intent).unwrap());
    let ticket = service::reserve_ollama(&mut store, &intent).unwrap();
    let Start::Started { owner, .. } = store.generation_resume_preparation(&ticket).unwrap() else { panic!() };
    store.generation_ensure_history_ids(&owner).unwrap();
    let mut owned = store.generation_owned_preparation_snapshot(&owner).unwrap().unwrap();
    owned.entropy = snapshot.entropy.clone();
    let (first, second) = (prepare_ollama_owned(&owned), prepare_ollama_owned(&owned));
    let request = |attempt: &crate::native_generation::prepare::PreparationAttempt| serde_json::to_value(attempt.result.as_ref().unwrap()).unwrap();
    assert_eq!(request(&first), request(&second));
    let (first_plan, first_ids) = without_appended_ids(&first.variables, &owned.history.messages);
    let (second_plan, second_ids) = without_appended_ids(&second.variables, &owned.history.messages);
    assert_eq!(first_plan, second_plan);
    assert_eq!(first_ids.len(), 2);
    assert!(first_ids.iter().all(|id| !second_ids.contains(id)));
    // `{{history}}` would send the appended IDs; the route declines it with no host request.
    let mut reads = case.clone();
    reads["character"]["depth_prompt"] = json!({"depth":1,"prompt":"Recap {{history}}"});
    let (_directory, mut store, mut intent) = stored(&database(&reads));
    intent.host = recorded_host(&reads);
    let snapshot = pinned(&reads, store.generation_preparation_snapshot(&intent).unwrap());
    assert_eq!(admission_host_requests(&snapshot).1, 0);
    assert_eq!(admit_ollama(&snapshot).map(|_| ()), Err(PreparationError::Unsupported("cbs".into())));
}

// TS sends one request whatever `requestRetrys` holds (the `retry-*` effects of `ollama-oracle.test.ts`), so the route
// admits every stored value.
#[test]
fn a_configured_retry_count_is_admitted() {
    use crate::native_generation::service;
    for retries in [json!(2), json!(10), json!(null), json!("3")] {
        let (_directory, mut store, intent) = stored(&database(&json!({"root":{"requestRetrys":retries}})));
        let ticket = service::reserve_ollama(&mut store, &intent).unwrap_or_else(|error| panic!("{retries}: {error:?}"));
        assert!(matches!(service::prepare_reserved_ollama(&mut store, &ticket).unwrap(), service::PreparedGeneration::Ready { .. }), "{retries}");
    }
}

#[test]
fn editoutput_static_admission_covers_unmatched_rules_before_any_host_or_reservation() {
    use crate::native_generation::{cbs::RegexUnicode, prepare::admission_host_requests, service::{self, GenerationError}};
    let now = now() as i64;
    let host = |unicode, excluded| HostCapture::new(false, CaseUnicode::Unicode16, vec![(now - 86_400_000, 0)], now + 86_400_000)
        .with_regex(unicode, excluded);
    let rule = |pattern: &str, replacement: &str, flag: &str| json!({"type":"editoutput","in":pattern,"out":replacement,"flag":flag,"ableFlag":!flag.is_empty()});
    let cases = [
        (rule("{{char}}", "safe", "g<cbs>"), RegexUnicode::Unicode17, "regex.dynamicPattern"),
        (rule("absent", "@@repeat_back end", ""), RegexUnicode::Unicode17, "regex.repeatBack"),
        (rule("absent", "@@inject", ""), RegexUnicode::Unicode17, "regex.inject"),
        (rule("absent", "@@emo calm", ""), RegexUnicode::Unicode17, "regex.emo"),
        (rule("absent", "safe", "gv"), RegexUnicode::Unicode17, "regex.engine"),
        (rule("\\p{L}", "safe", "gu"), RegexUnicode::Before17, "regex.unicode"),
        (rule("é", "safe", "gi"), RegexUnicode::From18, "regex.unicode"),
        (rule("absent", "{{screenwidth}}", ""), RegexUnicode::Unicode17, "regex.replacementCbs"),
        (rule("absent", "{{getvar::{{random::safe::unsafe}}}}", ""), RegexUnicode::Unicode17, "regex.replacementCbs"),
        (rule("absent", "{{getvar::$1}}", ""), RegexUnicode::Unicode17, "regex.replacementCbs"),
        (rule("absent", "{{description}}", ""), RegexUnicode::Unicode17, "regex.replacementCbs"),
        (rule("absent", "{{#func f}}{{screenwidth}}{{/func}}{{call::f}}", ""), RegexUnicode::Unicode17, "regex.replacementCbs"),
        (rule("absent", "{{setvar::x::value}}", ""), RegexUnicode::Unicode17, "regex.replacementCbs"),
    ];
    let mut cases: Vec<_> = cases.into_iter().map(|(script, unicode, name)| (vec![script], unicode, name)).collect();
    let large_nan_plan: Vec<_> = (0..64).map(|index| rule("absent", "safe", if index == 0 { "g<order NaN>" } else { "g" })).collect();
    cases.push((large_nan_plan, RegexUnicode::Unicode17, "regex.engine"));
    for (scripts, unicode, name) in cases {
        let patch = json!({"character":{"customscript":scripts},"conversation":{"scriptstate":{"$safe":"literal","$unsafe":"{{screenwidth}}"}}});
        let (_directory, mut store, mut intent) = stored(&database(&patch));
        intent.host = Some(host(unicode, false));
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(admission_host_requests(&snapshot), (Err(PreparationError::Unsupported(name.into())), 0), "{patch}");
        assert!(matches!(service::reserve_ollama(&mut store, &intent), Err(GenerationError::Preparation(PreparationError::Unsupported(ref value))) if value == name), "{patch}");
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        assert!(service::active_generations(&store).unwrap().is_empty());
    }
    // Invalid/empty patterns cannot execute their replacement, while the supported rules retain their order.
    let scripts = json!([rule("(", "{{screenwidth}}", ""), rule("", "{{description}}", ""), rule("seed", "{{getvar::{{getvar::key}}}} {{char}} {{user}}", "")]);
    let patch = json!({"character":{"customscript":scripts},"conversation":{"scriptstate":{"$key":"safe","$safe":"literal","$unused":"{{screenwidth}}"}}});
    let (_directory, mut store, mut intent) = stored(&database(&patch));
    intent.host = Some(host(RegexUnicode::Unicode17, false));
    assert!(crate::native_generation::prepare::admit_ollama(&store.generation_preparation_snapshot(&intent).unwrap()).is_ok());
    // Exclusion removes an unsafe preset rule; the engine admits property classes and a small NaN-order plan.
    for patch in [json!({"preset":{"regex":[rule("absent", "{{screenwidth}}", "")]}}),
        json!({"character":{"customscript":[rule("\\p{L}", "safe", "gu")]}}),
        json!({"character":{"customscript":[rule("absent", "safe", "g<order NaN>")]}})] {
        let (_directory, mut store, mut intent) = stored(&database(&patch));
        intent.host = Some(host(RegexUnicode::Unicode17, true));
        assert!(crate::native_generation::prepare::admit_ollama(&store.generation_preparation_snapshot(&intent).unwrap()).is_ok(), "{patch}");
    }
    for (patch, name) in [
        (json!({"root":{"dynamicAssets":true},"character":{"additionalAssets":[["asset","synthetic-path","txt"]]}}), "regex.dynamicAssets"),
        (json!({"character":{"triggerscript":[{"comment":"","type":"manual","conditions":[],"effect":[{"type":"triggerlua","code":"synthetic"}]}]}}), "triggers"),
        (json!({"character":{"triggerscript":[{"comment":"","type":"manual","conditions":[],"effect":[{"type":"triggercode","code":"synthetic"}]}]}}), "triggers"),
        (json!({"root":{"plugins":[{"enabled":true,"version":"3.0","name":"synthetic"}]}}), "plugins"),
    ] {
        let (_directory, mut store, mut intent) = stored(&database(&patch));
        intent.host = Some(host(RegexUnicode::Unicode17, false));
        assert!(matches!(service::reserve_ollama(&mut store, &intent), Err(GenerationError::Preparation(PreparationError::Unsupported(ref value))) if value == name), "{patch}");
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
    }
}


fn effect_budget() -> risunest_ir::Budget {
    risunest_ir::Budget::new(risunest_ir::Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 18,
        max_values: 1 << 16, max_depth: 128, max_work: 1 << 20 },
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap()
}

fn lore_variable(name: &str, budget: &risunest_ir::Budget) -> risunest_ir::stage::Effect {
    risunest_ir::stage::Effect::ChatVariable { name: risunest_ir::Text::from_str(name, budget).unwrap(),
        value: risunest_ir::Value::Text(risunest_ir::Text::from_str("true", budget).unwrap()) }
}

#[test]
fn variable_plan_keeps_assignment_order_and_original_presence() {
    use crate::native_generation::preparation_effects::{preparation_plan, preparation_variables};
    let (_directory, mut store, intent) = stored(&database(&json!({"conversation":{"scriptstate":{
        "$__internal_ka_null":null,"$__internal_da_value":{"nested":[1,false]},"unrelated":"preserved"}}})));
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    snapshot.selected_preset_id = Some("gui-selection".into());
    let budget = effect_budget();
    let names = ["__internal_ka_missing", "__internal_ka_null", "__internal_da_value", "__internal_ka_missing", "__internal_ka_"];
    let effects = names.iter().map(|name| lore_variable(name, &budget)).collect::<Vec<_>>();
    let (plan, result) = preparation_variables(&snapshot, &effects);
    assert_eq!(result, Ok(()));
    assert_eq!(plan.assignments, names.map(|name| (name.to_owned(), json!("true"))));
    assert_eq!(plan.original.len(), 4);
    assert_eq!(plan.original["$__internal_ka_missing"], None);
    assert_eq!(plan.original["$__internal_ka_null"], Some(Value::Null));
    assert_eq!(plan.original["$__internal_da_value"], Some(json!({"nested":[1,false]})));
    assert_eq!(plan.selected_preset_id.as_deref(), Some("gui-selection"));
    for state in [None, Some(Value::Null)] {
        snapshot.history.conversation.as_object_mut().unwrap().remove("scriptstate");
        if let Some(state) = state { snapshot.history.conversation["scriptstate"] = state; }
        snapshot.selected_preset_id = None;
        let (plan, result) = preparation_variables(&snapshot, &effects);
        assert_eq!(result, Ok(()));
        assert!(plan.original.values().all(Option::is_none));
        assert_eq!(plan.selected_preset_id, None);
    }
    for state in [json!([]), json!(7), json!(false), json!("invalid")] {
        snapshot.history.conversation["scriptstate"] = state;
        let (plan, result) = preparation_variables(&snapshot, &effects);
        assert_eq!(result, Err(PreparationError::Invalid("scriptstate".into())));
        assert!(plan.assignments.is_empty());
        assert_eq!(preparation_plan(&snapshot, &effects).1, Err(PreparationError::Unsupported("effect".into())));
    }
}

#[test]
fn variable_plan_rejects_unrepresentable_effects_without_losing_completed_prefix() {
    use crate::native_generation::preparation_effects::{preparation_plan, preparation_variables};
    use risunest_ir::{stage::{Effect, MessageEdit}, OwnedVec, Text, Value as Ir};
    let (_directory, mut store, intent) = stored(&database(&json!({})));
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    let budget = effect_budget();
    let text = |value: &str| Ir::Text(Text::from_str(value, &budget).unwrap());
    let variable = |value| Effect::ChatVariable { name: Text::from_str("trigger_value", &budget).unwrap(), value };
    let note = |value| Effect::ChatField { field: Text::from_str("note", &budget).unwrap(), value };
    // `undefined`, non-finite numbers and arrays or objects decline for now, and the author note is text only.
    let invalids = [
        (variable(Ir::Undefined), "effect.variable.value"),
        (variable(Ir::Number(f64::NAN)), "effect.variable.value"),
        (variable(Ir::Number(f64::INFINITY)), "effect.variable.value"),
        (variable(Ir::Number(f64::NEG_INFINITY)), "effect.variable.value"),
        (variable(Ir::Array(OwnedVec::new(&budget).unwrap())), "effect.variable.value"),
        (variable(Ir::Object(OwnedVec::new(&budget).unwrap())), "effect.variable.value"),
        (variable(Ir::Text(Text::from_units(&[0xd800], &budget).unwrap())), "effect.variable.value"),
        (Effect::ChatVariable { name: Text::from_units(&[0x74, 0xd800], &budget).unwrap(), value: text("true") }, "effect.variable.name"),
        (note(Ir::Undefined), "effect.note"),
        (note(Ir::Number(f64::NAN)), "effect.note"),
        (note(Ir::Object(OwnedVec::new(&budget).unwrap())), "effect.note"),
        (note(Ir::Null), "effect.note"),
        (note(Ir::Bool(true)), "effect.note"),
        (note(Ir::Number(1.0)), "effect.note"),
        (Effect::ChatField { field: Text::from_str("hypaV3Data", &budget).unwrap(), value: Ir::Null }, "effect"),
        (Effect::Message(MessageEdit::Remove { index: 0.0 }), "effect"),
        (Effect::Message(MessageEdit::SetData { index: 0.0, data: Ir::Number(1.0) }), "effect.message"),
        (Effect::CharacterField { field: Text::from_str("name", &budget).unwrap(), value: Text::from_str("Renamed", &budget).unwrap() }, "effect"),
    ];
    for (invalid, name) in invalids {
        let effects = vec![lore_variable("__internal_da_prefix", &budget), invalid, lore_variable("__internal_ka_after", &budget)];
        let (plan, result) = preparation_variables(&snapshot, &effects);
        assert_eq!(result, Err(PreparationError::Invalid(name.into())));
        assert_eq!(plan.assignments, [("__internal_da_prefix".to_owned(), json!("true"))]);
        assert_eq!((plan.original.len(), &plan.note), (1, &None));
        // Admission's dry run declines the same effects before reservation, so TS runs the send.
        assert_eq!(preparation_plan(&snapshot, &effects), (plan, Err(PreparationError::Unsupported("effect".into()))));
    }
}

#[test]
fn preparation_carrier_retains_variables_and_the_original_failure() {
    use crate::native_generation::prepare::{admission_effect_count, preparation_from_outcome, prepare_ollama_owned};
    use risunest_prompt::machine::{PromptFailure, PromptOutcome, PromptOutput};
    use risunest_ir::{stage::Effect, Text, Value as Ir};
    let (_directory, mut store, intent) = stored(&database(&json!({})));
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    let budget = effect_budget();
    let ready = |tokens, effects| PromptOutcome::Ready(PromptOutput { messages: Vec::new(), input_tokens: tokens,
        output_tokens: 1.0, max_tokens: 1.0, max_context: 4096.0, biases: Vec::new(), effects });
    let effects = || vec![lore_variable("__internal_ka_written", &budget),
        Effect::ChatField { field: Text::from_str("lastMemory", &budget).unwrap(), value: Ir::Text(Text::from_str("memory", &budget).unwrap()) }];
    let unrepresentable = || Effect::ChatVariable { name: Text::from_str("unrepresentable", &budget).unwrap(), value: Ir::Undefined };
    let attempt = preparation_from_outcome(&snapshot, Ok(ready(1.0, effects())), Vec::new());
    assert_eq!(attempt.result.unwrap().last_memory.as_deref(), Some("memory"));
    assert_eq!(attempt.variables.assignments, [("__internal_ka_written".to_owned(), json!("true"))]);
    for original in [PreparationError::Invalid("start-stage".into()), PreparationError::Invalid("resume-stage".into()),
        PreparationError::Tokenizer("tokenizer-original".into())] {
        let mut prefix = effects();
        prefix.push(unrepresentable());
        let attempt = preparation_from_outcome(&snapshot, Err(original.clone()), prefix);
        assert_eq!(attempt.result.unwrap_err(), original);
        assert_eq!(attempt.variables.assignments, [("__internal_ka_written".to_owned(), json!("true"))]);
    }
    for (reason, expected) in [
        (PromptFailure::ContextLimit { required_tokens: 5000.0, recheck: true }, PreparationError::ContextLimit),
        (PromptFailure::Unsupported("original-refusal".into()), PreparationError::Unsupported("original-refusal".into())),
        (PromptFailure::Memory(Text::from_str("memory-error", &budget).unwrap()), PreparationError::Invalid("memory".into())),
        (PromptFailure::StopSending, PreparationError::Invalid("stopSending".into())),
    ] {
        let mut prefix = effects();
        prefix.push(unrepresentable());
        let attempt = preparation_from_outcome(&snapshot, Ok(PromptOutcome::Failed { reason, effects: prefix }), Vec::new());
        assert_eq!(attempt.result.unwrap_err(), expected);
        assert_eq!(attempt.variables.assignments.len(), 1);
    }
    let attempt = preparation_from_outcome(&snapshot, Ok(ready(0.5, effects())), Vec::new());
    assert_eq!(attempt.result.unwrap_err(), PreparationError::Invalid("inputTokens".into()));
    assert_eq!(attempt.variables.assignments.len(), 1);
    let invalid_memory = vec![lore_variable("__internal_ka_written", &budget),
        Effect::ChatField { field: Text::from_str("lastMemory", &budget).unwrap(), value: Ir::Null }];
    let attempt = preparation_from_outcome(&snapshot, Ok(ready(1.0, invalid_memory)), Vec::new());
    assert_eq!(attempt.result.unwrap_err(), PreparationError::Invalid("lastMemory".into()));
    assert_eq!(attempt.variables.assignments.len(), 1);
    let mut invalid = effects();
    invalid.push(unrepresentable());
    let attempt = preparation_from_outcome(&snapshot, Ok(ready(1.0, invalid)), Vec::new());
    assert_eq!(attempt.result.unwrap_err(), PreparationError::Invalid("effect.variable.value".into()));
    assert_eq!(attempt.variables.assignments.len(), 1);
    assert_eq!(admission_effect_count(&snapshot), 0);
    let ordinary = prepare_ollama_owned(&snapshot);
    assert!(ordinary.variables.assignments.is_empty());
    let mut refused = snapshot;
    refused.preset["aiModel"] = json!("unsupported-model");
    let attempt = prepare_ollama_owned(&refused);
    assert_eq!(attempt.result.unwrap_err(), PreparationError::Unsupported("model".into()));
    assert!(attempt.variables.assignments.is_empty());
}


fn lore_host() -> HostCapture {
    HostCapture::new(false, CaseUnicode::Unicode16, vec![(0, 0)], 8_640_000_000_000_000)
        .with_regex(crate::native_generation::cbs::RegexUnicode::Unicode17, false)
}

fn lore_entry(id: &str, content: &str) -> Value {
    json!({"id":id,"key":"","content":content,"alwaysActive":true,"insertorder":100,"comment":id})
}

fn lore_database(case: &Value) -> Value {
    let mut db = database(case);
    db["loreBookDepth"] = json!(10);
    db["loreBookToken"] = json!(1000);
    if db["characters"][0].get("loreSettings").is_none() {
        db["characters"][0]["loreSettings"] = json!({"recursiveScanning":false});
    }
    db
}

#[test]
fn real_lore_uses_effective_defaults_and_shares_completed_variables_with_later_cbs() {
    use crate::native_generation::prepare::{admission_effect_count, admit_ollama, prepare_ollama_owned};
    let case = json!({"root":{"templateDefaultVariables":"__internal_ka_first=true"},
        "preset":{"templateDefaultVariables":"__internal_ka_first=false"},
        "character":{"globalLore":[lore_entry("first", "@@keep_activate_after_match\n@@dont_activate_after_match\nfirst lore.")],
            "depth_prompt":{"depth":0,"prompt":"live={{getvar::__internal_ka_first}}/{{getvar::__internal_da_first}}"}},
        "conversation":{"localLore":[lore_entry("second", "@@keep_activate_after_match\nlater={{getvar::__internal_ka_first}}.")]}});
    let (_directory, mut store, mut intent) = stored(&lore_database(&case));
    intent.host = Some(lore_host());
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    let before = store.read_conversation("character", "conversation", None).unwrap().unwrap().value;
    snapshot.entropy = Some(admit_ollama(&snapshot).unwrap());
    assert_eq!(admission_effect_count(&snapshot), 0);
    let attempt = prepare_ollama_owned(&snapshot);
    assert_eq!(attempt.variables.assignments, ["__internal_ka_first", "__internal_da_first", "__internal_ka_second"]
        .map(|name| (name.to_owned(), json!("true"))));
    let messages = attempt.result.unwrap().messages.into_iter().map(|message| message.content).collect::<Vec<_>>().join("\n");
    assert!(messages.contains("later=true."), "{messages}");
    assert!(messages.contains("live=true/true"), "{messages}");
    assert_eq!(store.read_conversation("character", "conversation", None).unwrap().unwrap().value, before);
    assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
}

#[test]
fn lore_text_counts_have_no_message_overhead_and_budget_exclusion_keeps_writes() {
    use crate::native_generation::prepare::{admit_ollama, prepare_ollama_owned};
    let first = "L05ONLY_FIRST";
    let count = crate::native_tokenizer::count_cl100k_text(first).unwrap();
    for tokens in [count, count - 1] {
        let case = json!({"character":{"loreSettings":{"recursiveScanning":false,"tokenBudget":tokens},"globalLore":[
            lore_entry("first", &format!("@@keep_activate_after_match\n{first}")),
            lore_entry("second", "@@keep_activate_after_match\nL05ONLY_SECOND repeated words exceed the first entry token budget.")]}});
        let (_directory, mut store, mut intent) = stored(&lore_database(&case));
        intent.host = Some(lore_host());
        let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        snapshot.entropy = Some(admit_ollama(&snapshot).unwrap());
        let attempt = prepare_ollama_owned(&snapshot);
        assert_eq!(attempt.variables.assignments, ["__internal_ka_first", "__internal_ka_second"]
            .map(|name| (name.to_owned(), json!("true"))));
        let messages = attempt.result.unwrap().messages.into_iter().map(|message| message.content).collect::<Vec<_>>().join("\n");
        assert_eq!(messages.contains(first), tokens == count, "{messages}");
        assert!(!messages.contains("L05ONLY_SECOND"), "{messages}");
    }
}

#[test]
fn lore_probe_preserves_earlier_tokenizer_failure_and_owned_prefix_before_recursive_refusal() {
    use crate::native_generation::{prepare::{admit_ollama, prepare_ollama_owned}, service};
    for module_only in [false, true] {
        for fails in [true, false] {
            let current = if fails { "<|endoftext|> I" } else { "I" };
            let mut case = json!({"character":{"loreSettings":{"recursiveScanning":true},"globalLore":[
                lore_entry("lead", "@@keep_activate_after_match\nlead."),
                lore_entry("current", &format!("@@keep_activate_after_match\n@@dont_activate_after_match\n{current}"))]}});
            if module_only {
                let entries = case["character"].as_object_mut().unwrap().remove("globalLore").unwrap();
                case["root"] = json!({"modules":[{"id":"selected","lorebook":entries}],"enabledModules":["selected"]});
            }
            let (_directory, mut store, mut intent) = stored(&lore_database(&case));
            intent.host = Some(HostCapture::new(true, CaseUnicode::Unicode16, vec![(0, 0)], 8_640_000_000_000_000));
            let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
            let admission = admit_ollama(&snapshot);
            use risunest_lorebook::selector::{LoreFailureSite, LorePhase};
            assert_eq!(crate::native_generation::prepare::admission_lore_failure_site(&snapshot),
                Some(LoreFailureSite { entry: Some(1), phase: if fails { LorePhase::Count } else { LorePhase::Recursive } }));
            if fails { snapshot.entropy = Some(admission.unwrap()); }
            else { assert_eq!(admission.unwrap_err(), PreparationError::Unsupported("case-locale".into())); }
            let attempt = prepare_ollama_owned(&snapshot);
            let expected = if fails { PreparationError::Tokenizer("disallowed_special_token".into()) }
                else { PreparationError::Unsupported("case-locale".into()) };
            assert_eq!(attempt.result.unwrap_err(), expected);
            let names = if fails { vec!["__internal_ka_lead"] }
                else { vec!["__internal_ka_lead", "__internal_ka_current", "__internal_da_current"] };
            assert_eq!(attempt.variables.assignments, names.into_iter().map(|name| (name.to_owned(), json!("true"))).collect::<Vec<_>>());
            assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
            assert!(service::active_generations(&store).unwrap().is_empty());
            let ticket = service::reserve_ollama(&mut store, &intent);
            if fails {
                let ticket = ticket.unwrap();
                if module_only {
                    use crate::native_generation::{operation::Phase, owner::OwnerHost, service::ReadyPreparation};
                    let host = OwnerHost::new("RisuNest module failure test").unwrap();
                    let ReadyPreparation::Failed { error, status } = service::prepare_ready_ollama(&mut store, &ticket, &host).unwrap()
                        else { panic!("module count failure did not retain its failure carrier") };
                    assert_eq!(error, expected);
                    assert_eq!((status.phase, status.preparation_ready), (Phase::Failed, false));
                    let saved = store.read_conversation("character", "conversation", None).unwrap().unwrap().value;
                    assert_eq!(saved["scriptstate"]["$__internal_ka_lead"], "true");
                    assert!(saved["scriptstate"].get("$__internal_ka_current").is_none());
                    assert!(saved["scriptstate"].get("$__internal_da_current").is_none());
                    assert_eq!(saved.get("lastMemory"), snapshot.history.conversation.get("lastMemory"));
                    let effects = store.generation_effects_evidence(&ticket).unwrap();
                    let revision = store.revision().unwrap();
                    service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("must not reevaluate failed module lore"))));
                    assert!(matches!(service::prepare_ready_ollama(&mut store, &ticket, &host).unwrap(), ReadyPreparation::Existing(status)
                        if status.phase == Phase::Failed && status.preparation_error.as_ref() == Some(&expected)));
                    service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
                    assert_eq!(store.revision().unwrap(), revision);
                    assert_eq!(store.read_conversation("character", "conversation", None).unwrap().unwrap().value["scriptstate"], saved["scriptstate"]);
                    assert_eq!(store.generation_effects_evidence(&ticket).unwrap(), effects);
                }
            } else { assert!(ticket.is_err()); assert_eq!(store.revision().unwrap(), intent.target.expected_revision); }
        }
    }
}

#[test]
fn captured_nonempty_lore_keeps_exact_probe_counts_when_selection_has_no_fragments() {
    use crate::native_generation::prepare::{admit_ollama, prepare_ollama_owned};
    for module_only in [false, true] {
        for mode in ["inactive", "excluded", "injection"] {
            let mut entry = lore_entry("only", "@@keep_activate_after_match\nunused lore.");
            if mode == "inactive" { entry["alwaysActive"] = json!(false); }
            if mode == "injection" { entry["content"] = json!("@@inject_at nowhere\nunused lore."); }
            let mut case = json!({"preset":{"maxContext":200,"maxResponse":0},"character":{
                "globalLore":[entry],"loreSettings":{"recursiveScanning":false,"tokenBudget":if mode == "excluded" { 0 } else { 1000 }},
                "depth_prompt":{"depth":0,"prompt":"{{screenwidth}}"}},
                "messages":[{"role":"user","chatId":"long","data":" word".repeat(300)}]});
            if module_only {
                let entries = case["character"].as_object_mut().unwrap().remove("globalLore").unwrap();
                case["root"] = json!({"modules":[{"id":"only","lorebook":entries}],"enabledModules":["only"]});
            }
            let (_directory, mut store, mut intent) = stored(&lore_database(&case));
            intent.host = Some(lore_host());
            let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
            snapshot.entropy = Some(admit_ollama(&snapshot).unwrap());
            assert_eq!(prepare_ollama_owned(&snapshot).result.unwrap_err(), PreparationError::ContextLimit, "{module_only}/{mode}");
        }
    }
}

#[test]
fn preparation_shares_one_tape_between_cbs_lore_probability_and_later_prompt() {
    use crate::native_generation::prepare::{admit_ollama, prepare_ollama_owned};
    let case = json!({"preset":{"mainPrompt":"{{random::MZERO::MONE}}"},"character":{
        "globalLore":[lore_entry("chance", "@@probability 50\n@@keep_activate_after_match\nnot selected.")],
        "depth_prompt":{"depth":0,"prompt":"{{random::DZERO::DONE}}"}}});
    let (_directory, mut store, mut intent) = stored(&lore_database(&case));
    intent.host = Some(lore_host());
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    snapshot.entropy = Some(Entropy::pinned(1_700_000_000_000.0, &[0.0, 0.9, 0.9]));
    assert!(admit_ollama(&snapshot).is_ok());
    let attempt = prepare_ollama_owned(&snapshot);
    assert!(attempt.variables.assignments.is_empty());
    let messages = attempt.result.unwrap().messages.into_iter().map(|message| message.content).collect::<Vec<_>>().join("\n");
    assert!(messages.contains("MZERO") && messages.contains("DONE"), "{messages}");
    assert!(!messages.contains("not selected."), "{messages}");
    snapshot.entropy = Some(Entropy::pinned(1_700_000_000_000.0, &[0.0, 0.9]));
    assert!(matches!(admit_ollama(&snapshot), Err(PreparationError::Invalid(name)) if name == "limit:rng-tape"));
    assert_eq!(prepare_ollama_owned(&snapshot).result.unwrap_err(), PreparationError::Invalid("limit:rng-tape".into()));
    // The probability draw shares the same admission ceiling as the preceding CBS draws.
    snapshot.preset["mainPrompt"] = json!("{{dice::4096d1}}");
    snapshot.history.character["globalLore"] = json!([lore_entry("chance", "@@probability 100\nnot reached.")]);
    snapshot.entropy = Some(Entropy::pinned(1_700_000_000_000.0, &vec![0.0; 4097]));
    assert_eq!(admit_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported("cbs.random-draws".into()));
    assert_eq!(prepare_ollama_owned(&snapshot).result.unwrap_err(), PreparationError::Unsupported("cbs.random-draws".into()));
}

#[test]
fn lore_capabilities_refuse_before_reservation_and_unused_lore_settings_stay_dormant() {
    use crate::native_generation::{prepare::admit_ollama, service};
    let mut unsafe_pattern = lore_entry("pattern", "matched.");
    unsafe_pattern["alwaysActive"] = json!(false);
    unsafe_pattern["key"] = json!("/\\p{L}/u");
    unsafe_pattern["useRegex"] = json!(true);
    for (patch, expected) in [
        (json!({"character":{"globalLore":[unsafe_pattern]}}), "regex-unicode"),
        (json!({"character":{"globalLore":[lore_entry("unicode", "é")] ,"loreSettings":{"recursiveScanning":true}}}), "case-table"),
        (json!({"character":{"globalLore":[lore_entry("host", "{{screenwidth}}") ]}}), "cbs.screenwidth"),
        (json!({"character":{"globalLore":[lore_entry("large", &"x".repeat(1_048_577))]}}), "tokenizer-input-limit"),
        (json!({"root":{"modules":[{"id":"selected","mcp":{"url":"synthetic"}}]},"preset":{"moduleIntergration":"selected"}}), "modules"),
    ] {
        let (_directory, mut store, mut intent) = stored(&lore_database(&patch));
        intent.host = Some(HostCapture::new(false, CaseUnicode::From17, vec![(0, 0)], 8_640_000_000_000_000));
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(admit_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported(expected.into()));
        assert!(service::reserve_ollama(&mut store, &intent).is_err());
        assert!(service::active_generations(&store).unwrap().is_empty());
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        if matches!(expected, "regex-unicode" | "case-table") {
            let mut supported = snapshot;
            supported.host = Some(lore_host());
            assert!(admit_ollama(&supported).is_ok(), "{expected}");
        }
        if expected == "case-table" {
            for (case_unicode, refusal) in [(CaseUnicode::Unicode17, None), (CaseUnicode::From18, Some("case-table"))] {
                intent.host = Some(HostCapture::new(false, case_unicode, vec![(0, 0)], 8_640_000_000_000_000)
                    .with_regex(crate::native_generation::cbs::RegexUnicode::Unicode17, false));
                let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
                assert_eq!(admit_ollama(&snapshot).err(), refusal.map(|name| PreparationError::Unsupported(name.into())), "{case_unicode:?}");
            }
        }
    }
    let db = database(&json!({"root":{"loreBookDepth":{"unused":true},"loreBookToken":[],"modules":[{"id":"unselected","lorebook":"unused"}]},
        "character":{"loreSettings":{"scanDepth":{},"tokenBudget":[],"recursiveScanning":true}},
        "preset":{"moduleIntergration":""}}));
    let (_directory, mut store, intent) = stored(&db);
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    assert!(admit_ollama(&snapshot).is_ok());
    assert!(prepare_ollama(&snapshot).is_ok());
}


#[test]
fn lore_full_history_uses_captured_speakers_and_declines_uncaptured_names() {
    use crate::native_generation::prepare::{admit_ollama, prepare_ollama_owned};
    for (saying, named, reset, refusal) in [
        ("character", false, false, Some("case-locale")),
        ("character", true, false, None),
        ("foreign", false, false, Some("historySpeakerLookup")),
        ("foreign", true, false, None),
        ("foreign", false, true, Some("historySpeakerLookup")),
        ("foreign", true, true, None),
    ] {
        let mut message = json!({"role":"char","chatId":"scanned","saying":saying,"data":"needle","disabled":!reset});
        if named { message["name"] = json!("safe"); }
        let mut messages = vec![message];
        if reset { messages.push(json!({"role":"char","chatId":"reset","name":"safe","data":"reset","disabled":"allBefore"})); }
        messages.push(json!({"role":"user","chatId":"latest","data":"Question."}));
        let mut entry = lore_entry("found", "@@keep_activate_after_match\nmatched full history.");
        entry["alwaysActive"] = json!(false);
        entry["key"] = json!("needle");
        let case = json!({"character":{"name":"I","globalLore":[entry]},"messages":messages});
        let (_directory, mut store, mut intent) = stored(&lore_database(&case));
        intent.host = Some(HostCapture::new(true, CaseUnicode::Unicode16, vec![(0, 0)], 8_640_000_000_000_000));
        let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        let admission = admit_ollama(&snapshot);
        if let Some(refusal) = refusal {
            assert_eq!(admission.unwrap_err(), PreparationError::Unsupported(refusal.into()));
        } else {
            snapshot.entropy = Some(admission.unwrap());
            let attempt = prepare_ollama_owned(&snapshot);
            assert!(attempt.result.is_ok(), "{:?}", attempt.result);
            assert_eq!(attempt.variables.assignments, [("__internal_ka_found".to_owned(), json!("true"))]);
        }
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
    }
}

#[test]
fn selected_module_lore_and_scripts_follow_canonical_order_and_regex_exclusion() {
    use crate::native_generation::prepare::{admission_effect_count, admit_ollama, prepare_ollama_owned};
    let rule = |suffix: &str| json!({"type":"editprocess","in":"CHAIN(?:\\|[A-Z])*","out":format!("$&|{suffix}")});
    let module = |id: &str, namespace: &str| json!({"id":id,"namespace":namespace,
        "lorebook":[lore_entry(&format!("module-{id}"), &format!("@@keep_activate_after_match\nMODULE-{id} sees={{{{getvar::__internal_ka_module-B}}}}/{{{{getvar::moduleDefault}}}}."))],
        "regex":[rule(id)]});
    for excluded in [false, true] {
        let mut selected = persona("selected", json!("Selected user"), json!("Selected persona."));
        selected["embeddedModule"] = json!({"id":"not-bound","trigger":[{"type":"start","effect":[{"type":"v2StopTrigger"}]}]});
        let mut bound = persona("bound", json!("Bound user"), json!("Bound persona."));
        bound["embeddedModule"] = json!({"id":"B","lorebook":[lore_entry("embedded-poison", "EMBEDDED_POISON")],
            "trigger":[{"type":"start","effect":[{"type":"v2StopTrigger"}]}]});
        let case = json!({
            // Both stores refuse a repeated library module ID, so the bound persona's embedded `B` is the duplicate to drop.
            "root":{"modules":[module("B", "ns.b"),module("A", "ns.a"),module("C", "ns.c"),
                {"id":"D","trigger":[{"type":"start","effect":[{"type":"v2StopTrigger"}]}]}],
                "enabledModules":["A","ns.b"],"moduleIntergration":"D","personas":[selected,bound],"selectedPersona":0,
                "templateDefaultVariables":"moduleDefault=ROOT"},
            "preset":{"moduleIntergration":" ns.c ","templateDefaultVariables":"moduleDefault=PRESET","regex":[rule("P")]},
            "character":{"firstMessage":"CHAIN","modules":["A"],"globalLore":[],"customscript":[rule("H")]},
            "conversation":{"modules":["ns.b"],"bindedPersona":"bound","localLore":[]}
        });
        let (_directory, mut store, mut intent) = stored(&lore_database(&case));
        intent.host = Some(lore_host().with_regex(crate::native_generation::cbs::RegexUnicode::Unicode17, excluded));
        let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        assert_eq!(snapshot.root["moduleIntergration"], "D");
        assert_eq!(snapshot.preset["moduleIntergration"], " ns.c ");
        snapshot.entropy = Some(admit_ollama(&snapshot).unwrap());
        assert_eq!(admission_effect_count(&snapshot), 0);
        let attempt = prepare_ollama_owned(&snapshot);
        assert_eq!(attempt.variables.assignments, ["B","A","C"].map(|id| (format!("__internal_ka_module-{id}"), json!("true"))));
        let turn = attempt.result.unwrap();
        assert_eq!(turn.user_name, "Bound user");
        let messages = turn.messages.into_iter().map(|message| message.content).collect::<Vec<_>>().join("\n");
        let chain: Vec<_> = messages.lines().filter(|line| line.starts_with("CHAIN")).collect();
        assert_eq!(chain, vec![if excluded { "CHAIN|H" } else { "CHAIN|P|H|B|A|C" }], "{messages}");
        assert!(messages.contains("MODULE-C sees=true/PRESET."), "{messages}");
        assert!(!messages.contains("POISON"), "{messages}");
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
        assert!(crate::native_generation::service::active_generations(&store).unwrap().is_empty());
    }
}

#[test]
fn selected_unsupported_module_contributions_refuse_before_reservation_and_unselected_records_stay_dormant() {
    use crate::native_generation::{prepare::admit_ollama, service};
    for contribution in [
        json!({"trigger":[{"type":"start","effect":[{"type":"triggerlua","code":""}]}]}),
        json!({"assets":[["synthetic","asset","bin"]]}),
        json!({"customModuleToggle":"synthetic-toggle"}),
        json!({"mcp":{"url":"synthetic"}}),
    ] {
        for selected in [false, true] {
            let mut module = contribution.clone();
            module["id"] = json!("selected");
            let case = json!({"root":{"modules":[module],"enabledModules":if selected {json!(["selected"])}else{json!([])}},
                "preset":{"moduleIntergration":""}});
            let (_directory, mut store, mut intent) = stored(&lore_database(&case));
            intent.host = Some(lore_host());
            let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
            let admitted = admit_ollama(&snapshot);
            if selected {
                assert_eq!(admitted.unwrap_err(), PreparationError::Unsupported("modules".into()), "{contribution}");
                assert!(service::reserve_ollama(&mut store, &intent).is_err());
            } else {
                assert!(admitted.is_ok(), "{contribution}");
                assert!(service::reserve_ollama(&mut store, &intent).is_ok());
            }
            assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
            if selected { assert!(service::active_generations(&store).unwrap().is_empty()); }
        }
    }
    for module in [
        json!({"id":"selected","trigger":[],"assets":[],"customModuleToggle":false,"mcp":{}}),
        json!({"id":"selected","trigger":null,"assets":null,"customModuleToggle":null,"mcp":null}),
    ] {
        let (_directory, mut store, mut intent) = stored(&lore_database(&json!({
            "root":{"modules":[module],"enabledModules":["selected"]},"preset":{"moduleIntergration":""}
        })));
        intent.host = Some(lore_host());
        assert!(admit_ollama(&store.generation_preparation_snapshot(&intent).unwrap()).is_ok());
    }
    for malformed in [json!({"regex":7}), json!({"lorebook":7})] {
        let mut module = malformed.clone();
        module["id"] = json!("selected");
        let (_directory, mut store, mut intent) = stored(&lore_database(&json!({
            "root":{"modules":[module],"enabledModules":["selected"]},"preset":{"moduleIntergration":""}
        })));
        intent.host = Some(lore_host());
        let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
        let error = admit_ollama(&snapshot).unwrap_err();
        if malformed.get("regex").is_some() { assert_eq!(error, PreparationError::Unsupported("regex.scripts".into())); }
        assert!(service::reserve_ollama(&mut store, &intent).is_err());
        assert!(service::active_generations(&store).unwrap().is_empty());
        assert_eq!(store.revision().unwrap(), intent.target.expected_revision);
    }
    let (_directory, mut store, mut intent) = stored(&lore_database(&json!({
        "root":{"modules":[{"id":"selected","lorebook":lore_entry("single", "@@keep_activate_after_match\nSINGLE_MODULE_ENTRY")}],
            "enabledModules":["selected"]},"preset":{"moduleIntergration":""}
    })));
    intent.host = Some(lore_host());
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    let attempt = crate::native_generation::prepare::prepare_ollama_owned(&snapshot);
    assert_eq!(attempt.variables.assignments, [("__internal_ka_single".to_owned(), json!("true"))]);
    assert!(attempt.result.unwrap().messages.iter().any(|message| message.content.contains("SINGLE_MODULE_ENTRY")));
    let (_directory, mut store, mut intent) = stored(&lore_database(&json!({
        "root":{"modules":[{"id":"selected","regex":7}],"enabledModules":["selected"]},
        "preset":{"moduleIntergration":""},"character":{"customscript":[{"type":"editprocess","in":"First question\\.","out":"CHARACTER_ONLY"}]}
    })));
    intent.host = Some(lore_host().with_regex(crate::native_generation::cbs::RegexUnicode::Unicode17, true));
    let snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    assert!(admit_ollama(&snapshot).is_ok());
    let messages = prepare_ollama(&snapshot).unwrap().messages.into_iter().map(|message| message.content).collect::<Vec<_>>().join("\n");
    assert!(messages.contains("CHARACTER_ONLY"), "{messages}");
}
