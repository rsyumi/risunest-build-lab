//! Replays `tests/fixtures/rust-core/triggers/run.*.json`, recorded from the real `runTrigger` by
//! `benchmarks/rust-core-oracles/triggers.oracle.test.ts`.
//!
//! CBS runs through `risunest-cbs-experimental` over the case's database, regular expressions through
//! `risunest-regex::engine::JsPatternEngine`, and the `v2Tokenize` and system prompt token counts through the tokenizer
//! crate's plugin counter, the harness answering `Math.ceil(length / 4)` tokens as the oracle's `tokenizerFunc` does.
//! A `v2RunLLM` request gets the case's next scripted `llm` reply, as the oracle's `requestChatData` returns it, and
//! `v2CheckSimilarity` searches through [`ReversedSimilarity`], as the oracle's `HypaProcesser` answers; both must
//! match the calls TS made. The reversed search proves only the trigger's side (the split, the documents and query,
//! the join and the outputVar), not ranking or the embedding and cache flow. CBS and the effects draw `Math.random()`
//! from one tape. A case marked `native: "unsupported-<name>"` must be
//! declined with that name, and the runner's `admit` must give that decline before any host request or effect unless the
//! case is also marked `admitted`, a decline the run meets while running, which may follow plugin tokenizer counts.
//! Every other case passes admission. A case's `earlier` writes, which the oracle commits through the session, and its
//! `earlierEdits`, chat edits as an earlier Lua run leaves them in the stored chat, reach the runner through
//! `TriggerRunner::apply` after admission, as the prompt machine's `absorb` hands over what earlier stages changed:
//! the runner's CBS processor and its input both take them.
//!
//! The input is built as the route owner builds it ([`owner_input`]): the modules come from the effective
//! `moduleIntergration` and the template variables from the effective `templateDefaultVariables`, which a case with a
//! preset stores differently in its root mirrors. The replay maps case with `CaseTables::Unicode16`, so the expected file
//! must come from a runtime whose Unicode version is 16.0.

mod support;

use risunest_ir::chat::MessageRole;
use risunest_ir::json::parse_json;
use risunest_ir::provider::{FailureCategory, OutputShape, ProviderFailure, ProviderOutput};
use risunest_ir::stage::{Effect, HostReply, HostRequest, MessageEdit, StageError, TriggerMode, TriggerRunner};
use risunest_ir::text::Text;
use risunest_cbs_experimental::cbs::processor::CaseTables;
use risunest_ir::value::Value;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use support::*;

#[derive(Deserialize)]
struct ExpectedFile {
    schema: u32,
    #[serde(rename = "fixtureSha256")]
    fixture_sha256: String,
    node: String,
    unicode: String,
    cases: Vec<ExpectedCase>,
}

#[derive(Deserialize)]
struct ExpectedCase { id: String, result: Recorded }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    returned: String,
    #[serde(rename = "systemPrompt")]
    system_prompt: Option<SystemPrompt>,
    tokens: Option<u32>,
    #[serde(rename = "stopSending")]
    stop_sending: Option<bool>,
    #[serde(rename = "sendAIprompt")]
    send_ai_prompt: Option<bool>,
    writes: Vec<(String, serde_json::Value)>,
    /// `[role, data, chatId or "new"]` per message; a declined case can hold a role that is not a string.
    messages: serde_json::Value,
    note: serde_json::Value,
    scriptstate: Vec<(String, serde_json::Value)>,
    #[serde(rename = "triggerId")]
    trigger_id: Option<String>,
    random: usize,
    /// The values TS hands its tokenizer; a declined case can hand it one that is not a string.
    tokenizer: Vec<serde_json::Value>,
    console: Vec<Vec<String>>,
    hosts: Vec<serde_json::Value>,
    #[serde(default)]
    error: Option<Thrown>,
}

/// The prompt builder reads these three; a v1 effect's other location lands on a key nothing reads.
#[derive(Deserialize, Debug, PartialEq)]
struct SystemPrompt { start: String, historyend: String, promptend: String }

#[derive(Deserialize)]
struct Thrown { name: String, message: String }

/// The expected file with each isolated surrogate escape written as the text `\u{xxxx}`, as `string` spells one;
/// `serde_json` strings hold no isolated surrogate.
fn isolated_surrogates(json: &str) -> String {
    let bytes = json.as_bytes();
    let unit = |at: usize| json.get(at..at + 4).and_then(|hex| u16::from_str_radix(hex, 16).ok());
    let high = |unit: u16| (0xd800..0xdc00).contains(&unit);
    let low = |unit: u16| (0xdc00..0xe000).contains(&unit);
    let mut out = String::with_capacity(json.len());
    let mut at = 0;
    while at < json.len() {
        if bytes[at] != b'\\' {
            let character = json[at..].chars().next().unwrap();
            out.push(character);
            at += character.len_utf8();
            continue;
        }
        if bytes[at + 1] != b'u' {
            out.push_str(&json[at..at + 2]);
            at += 2;
            continue;
        }
        let first = unit(at + 2).unwrap();
        if high(first) && json.get(at + 6..at + 8) == Some("\\u") && unit(at + 8).is_some_and(low) {
            out.push_str(&json[at..at + 12]);
            at += 12;
        } else if high(first) || low(first) {
            out.push_str(&format!("\\\\u{{{first:04x}}}"));
            at += 6;
        } else {
            out.push_str(&json[at..at + 6]);
            at += 6;
        }
    }
    out
}

fn json(value: &Value, budget: &risunest_ir::budget::Budget) -> serde_json::Value {
    let mut text = String::new();
    write_json(value, &mut text, budget);
    serde_json::from_str(&isolated_surrogates(&text)).unwrap()
}

/// The host answer to a scripted `llm` reply: `success` the text, `streaming` the stream as the owner collects it (its
/// last chunk), `multiline` the role and text of each pair, `fail` a provider failure with the `result` TS shows, and
/// `throw` what `requestChatData` throws.
fn llm_reply(reply: &Value, budget: &risunest_ir::budget::Budget) -> HostReply {
    let kind = text_field(reply, "type", budget).unwrap();
    let text = |value: &str| Text::from_str(value, budget).unwrap();
    let output = |texts: Vec<Text>, shape: OutputShape| HostReply::Llm(Ok(ProviderOutput { texts, shape, thoughts: Vec::new(),
        tool_calls: Vec::new(), usage: None, finish: None }));
    match kind.as_str() {
        "success" => output(vec![text(&text_field(reply, "result", budget).unwrap())], OutputShape::Text),
        "streaming" => {
            let chunks = field(reply, "chunks", budget).and_then(Value::array).unwrap();
            let last = chunks.last().map_or_else(|| text(""), |chunk| if let Value::Text(chunk) = chunk { chunk.copy(budget).unwrap() } else { panic!("chunk") });
            output(vec![last], OutputShape::Streamed)
        }
        "multiline" => {
            let (roles, texts) = field(reply, "result", budget).and_then(Value::array).unwrap().iter().map(|entry| match entry.array() {
                Some([Value::Text(role), Value::Text(data)]) => (if string(role) == "user" { MessageRole::User } else { MessageRole::Char },
                    data.copy(budget).unwrap()),
                _ => panic!("multiline"),
            }).unzip();
            output(texts, OutputShape::Multiline(roles))
        }
        "fail" => HostReply::Llm(Err(ProviderFailure { category: FailureCategory::Http, stage: None, http_status: Some(500),
            sdk_message: Some(text(&text_field(reply, "result", budget).unwrap())), body_failure: None })),
        "throw" => HostReply::Failed { message: text(&text_field(reply, "message", budget).unwrap()) },
        other => panic!("llm reply {other}"),
    }
}

/// A request as the oracle records the call: `['llm', model, formated, the other requestChatData arguments]`, and
/// `['similarity', the added texts, the query]` from the embedding request `ReversedSimilarity` makes.
fn host_call(host: &HostRequest, budget: &risunest_ir::budget::Budget) -> serde_json::Value {
    let text = |text: &Text| serde_json::Value::String(string(text));
    match host {
        HostRequest::Llm(request) => {
            let messages = request.messages.iter().map(|message| json(&message.to_value(budget).unwrap(), budget)).collect();
            serde_json::json!(["llm", string(&request.mode), serde_json::Value::Array(messages), json(&request.options, budget)])
        }
        HostRequest::Embed(request) => {
            let (query, documents) = request.texts.split_last().unwrap();
            serde_json::json!(["similarity", documents.iter().map(text).collect::<Vec<_>>(), text(query)])
        }
        other => panic!("unexpected host request {other:?}"),
    }
}

/// A value as the TS record writes it: `undefined` is null there.
fn recorded(value: &Value, budget: &risunest_ir::budget::Budget) -> serde_json::Value {
    match value { Value::Undefined => serde_json::Value::Null, other => json(other, budget) }
}

/// `slice` positions over `length` messages.
fn relative(value: f64, length: usize) -> usize {
    let value = if value.is_nan() { 0.0 } else { value.trunc() };
    if value < 0.0 { (length as f64 + value).max(0.0) as usize } else { value.min(length as f64) as usize }
}

/// The conversation after the delivered effects: messages with known ids or `new`, the note and the scriptstate entries.
struct Conversation {
    messages: Vec<(String, serde_json::Value, String)>,
    note: serde_json::Value,
    scriptstate: Vec<(String, serde_json::Value)>,
    writes: Vec<(String, serde_json::Value)>,
}

/// The `earlier` writes, then the `earlierEdits`, of a case as the effects an earlier stage returns.
fn earlier_writes(case: &Value, budget: &risunest_ir::budget::Budget) -> Vec<Effect> {
    let mut effects: Vec<Effect> = field(case, "earlier", budget).and_then(Value::array).unwrap_or(&[]).iter().map(|pair| {
        let pair = pair.array().expect("an earlier write is [key, value]");
        let Value::Text(name) = &pair[0] else { panic!("an earlier key is a string") };
        Effect::ChatVariable { name: name.copy(budget).unwrap(), value: copy(&pair[1], budget) }
    }).collect();
    let number = |value: &Value| match value { Value::Number(number) => *number, other => panic!("an edit index is a number, not {other:?}") };
    let text = |value: &Value| match value { Value::Text(text) => text.copy(budget).unwrap(), other => panic!("not a string: {other:?}") };
    let role = |value: &Value| if string(&text(value)) == "user" { MessageRole::User } else { MessageRole::Char };
    for edit in field(case, "earlierEdits", budget).and_then(Value::array).unwrap_or(&[]) {
        let [(kind, arguments)] = edit.object().expect("an edit is an object") else { panic!("an edit has one key") };
        let list = || arguments.array().expect("edit arguments");
        effects.push(Effect::Message(match string(kind).as_str() {
            "remove" => MessageEdit::Remove { index: number(arguments) },
            "insert" => MessageEdit::Insert { index: number(&list()[0]), role: role(&list()[1]), data: copy(&list()[2], budget),
                chat_id: text(&list()[3]) },
            "setRole" => MessageEdit::SetRole { index: number(&list()[0]), role: role(&list()[1]) },
            "replace" => MessageEdit::Replace { messages: copy(arguments, budget) },
            other => panic!("earlier edit {other}"),
        }));
    }
    effects
}

/// A message id as the oracle accepts a new one: a version 4 UUID.
fn uuid(id: &str) -> bool {
    id.len() == 36 && id.chars().enumerate().all(|(at, character)| match at {
        8 | 13 | 18 | 23 => character == '-',
        14 => character == '4',
        19 => "89ab".contains(character),
        _ => character.is_ascii_hexdigit() && !character.is_ascii_uppercase(),
    })
}

fn apply(chat: &Value, earlier: &[Effect], effects: &[Effect], budget: &risunest_ir::budget::Budget) -> Result<Conversation, String> {
    let mut messages: Vec<(String, serde_json::Value, String)> = field(chat, "message", budget).and_then(Value::array).unwrap().iter()
        .map(|row| (text_field(row, "role", budget).unwrap(), recorded(field(row, "data", budget).unwrap(), budget),
            text_field(row, "chatId", budget).unwrap()))
        .collect();
    let known: Vec<String> = messages.iter().map(|(_, _, id)| id.clone()).collect();
    let mut note = field(chat, "note", budget).map_or(serde_json::Value::Null, |note| json(note, budget));
    let mut scriptstate: Vec<(String, serde_json::Value)> = field(chat, "scriptstate", budget).and_then(Value::object).unwrap_or(&[]).iter()
        .map(|(key, value)| (string(key), json(value, budget))).collect();
    let mut writes = Vec::new();
    for (at, effect) in earlier.iter().chain(effects).enumerate() {
        match effect {
            Effect::ChatVariable { name, value } => {
                let value = recorded(value, budget);
                let key = format!("${}", string(name));
                // The earlier writes are stored before the run, which records only its own.
                if at >= earlier.len() { writes.push((string(name), value.clone())); }
                match scriptstate.iter_mut().find(|(existing, _)| *existing == key) {
                    Some((_, slot)) => *slot = value,
                    None => scriptstate.push((key, value)),
                }
            }
            Effect::ChatField { field, value } if field.equals_ascii(b"note") => note = json(value, budget),
            Effect::Message(MessageEdit::SetData { index, data }) => {
                if index.fract() != 0.0 || *index < 0.0 { return Err(format!("SetData at {index}")); }
                let Some(message) = messages.get_mut(*index as usize) else { return Err(format!("SetData past the end at {index}")) };
                message.1 = recorded(data, budget);
            }
            Effect::Message(MessageEdit::Cut { start, end }) => {
                let from = relative(*start, messages.len());
                let to = end.map_or(messages.len(), |end| relative(end, messages.len())).max(from);
                messages = messages[from..to].to_vec();
            }
            Effect::Message(MessageEdit::Append { role, data, chat_id }) => {
                let id = string(chat_id);
                if !uuid(&id) || known.contains(&id) { return Err(format!("appended message id {id}")); }
                let role = match role { MessageRole::User => "user", MessageRole::Char => "char" };
                messages.push((role.into(), recorded(data, budget), id));
            }
            // The Lua edits of `earlierEdits`: `splice` starts, the `at` rule and an assigned array.
            Effect::Message(MessageEdit::Remove { index }) => {
                let at = relative(*index, messages.len());
                if at < messages.len() { messages.remove(at); }
            }
            Effect::Message(MessageEdit::Insert { index, role, data, chat_id }) => {
                let id = string(chat_id);
                if !uuid(&id) || known.contains(&id) { return Err(format!("inserted message id {id}")); }
                let role = match role { MessageRole::User => "user", MessageRole::Char => "char" };
                messages.insert(relative(*index, messages.len()), (role.into(), recorded(data, budget), id));
            }
            Effect::Message(MessageEdit::SetRole { index, role }) => {
                let at = if index.trunc() < 0.0 { messages.len() as f64 + index.trunc() } else { index.trunc() };
                if at >= 0.0 && (at as usize) < messages.len() {
                    messages[at as usize].0 = match role { MessageRole::User => "user", MessageRole::Char => "char" }.into();
                }
            }
            Effect::Message(MessageEdit::Replace { messages: replaced }) => {
                messages = replaced.array().expect("a replaced array").iter().map(|row| (text_field(row, "role", budget).unwrap(),
                    recorded(field(row, "data", budget).unwrap(), budget), text_field(row, "chatId", budget).unwrap())).collect();
            }
            other => return Err(format!("unexpected effect {other:?}")),
        }
    }
    let messages = messages.into_iter().map(|(role, data, id)| {
        let id = if known.contains(&id) { id } else { "new".into() };
        (role, data, id)
    }).collect();
    Ok(Conversation { messages, note, scriptstate, writes })
}

fn mode(name: &str) -> TriggerMode {
    match name {
        "start" => TriggerMode::Start,
        "output" => TriggerMode::Output,
        "input" => TriggerMode::Input,
        "manual" => TriggerMode::Manual,
        "display" => TriggerMode::Display,
        "request" => TriggerMode::Request,
        other => panic!("mode {other}"),
    }
}

#[test]
fn replays_the_typescript_trigger_oracle() {
    let expected: ExpectedFile = serde_json::from_str(&isolated_surrogates(std::str::from_utf8(EXPECTED).unwrap())).unwrap();
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest, "the expected file was recorded from other cases");
    assert_eq!(expected.schema, 1);
    // The replay maps case with `CaseTables::Unicode16`; tables of another Unicode version can map other code points.
    assert_eq!(expected.unicode, "16.0", "recorded with Node {}", expected.node);
    assert!(!CASES.contains(&b'\r'));

    let file_budget = budget();
    let file = parse_json(std::str::from_utf8(CASES).unwrap(), &file_budget).unwrap();
    let base = field(&file, "base", &file_budget).unwrap();
    let cases = field(&file, "cases", &file_budget).and_then(Value::array).unwrap();
    assert_eq!(cases.len(), expected.cases.len());

    let mut mismatches = Vec::new();
    let (mut compared, mut failures, mut nulls, mut tokenized) = (0, 0, 0, 0);
    let mut declined = std::collections::BTreeMap::<String, usize>::new();
    let (mut in_admission, mut while_running) = (0, std::collections::BTreeMap::<String, usize>::new());
    for (case, recorded) in cases.iter().zip(&expected.cases) {
        let budget = budget();
        let id = text_field(case, "id", &budget).unwrap();
        assert_eq!(id, recorded.id);
        let result = &recorded.result;
        let mut differ = |what: &str, detail: String| mismatches.push(format!("{id}: {what}: {detail}"));
        let native = text_field(case, "native", &budget);
        let random: Vec<f64> = field(case, "random", &budget).and_then(Value::array).unwrap_or(&[]).iter()
            .map(|value| if let Value::Number(value) = value { *value } else { panic!("tape values are numbers") }).collect();
        let database = parse_json(&database_text(base, case, &budget), &budget).unwrap();
        let character = character(&database, &budget);
        let chat = conversation(character, &budget);
        let (root, mut input) = owner_input(&database, &budget);
        input.case_tables = CaseTables::Unicode16;
        let previous = text_field(case, "previousTriggerId", &budget);
        input.previous_trigger_id = previous.as_deref().map(|id| risunest_ir::text::Text::from_str(id, &budget).unwrap());
        let mode_name = text_field(case, "mode", &budget).unwrap();
        let manual = text_field(case, "manualName", &budget);
        let trigger_id = text_field(case, "triggerId", &budget);
        let request = request(mode(&mode_name), manual.as_deref(), trigger_id.as_deref(), &budget);
        let tape = SharedTape::new(random);
        let mut run = runner(&root, input, &tape, &budget);
        // The prompt machine asks the runner it holds, whose counter and similarity search decide what it declines.
        let admission = run.admit(&request, &budget);
        let running = match field(case, "admitted", &budget) {
            None => false,
            Some(Value::Bool(true)) if native.is_some() => true,
            Some(other) => panic!("{id}: `admitted` is true beside a `native` decline, not {other:?}"),
        };
        match (&admission, native.as_deref().filter(|_| !running).map(|native| native.trim_start_matches("unsupported-"))) {
            (Ok(()), None) => {}
            (Err(StageError::Unsupported(name)), Some(expected)) if name == expected => in_admission += 1,
            (admission, expected) => differ("admission", format!("expected the decline {expected:?}, got {admission:?}")),
        }
        let earlier = earlier_writes(case, &budget);
        for effect in &earlier {
            if let Err(error) = run.apply(effect, &budget) { differ("earlier", format!("apply failed with {error:?}")); }
        }
        let script = field(case, "llm", &budget).and_then(Value::array).unwrap_or(&[]);
        let mut replies = script.iter();
        let driven = drive_with(&mut run, request, &mut |host: &HostRequest| match host {
            HostRequest::Llm(_) => replies.next().map_or_else(|| HostReply::Failed { message: Text::from_str("llm-script exhausted", &budget).unwrap() },
                |reply| llm_reply(reply, &budget)),
            other => panic!("{id}: unexpected host request {other:?}"),
        }, &budget);

        if let Some(native) = &native {
            let name = native.strip_prefix("unsupported-").expect("native names a decline");
            *declined.entry(name.to_string()).or_insert(0) += 1;
            if running { *while_running.entry(name.to_string()).or_insert(0) += 1; }
            if !matches!(&driven.result, Err(StageError::Unsupported(declined)) if declined == name) {
                differ("native", format!("expected Unsupported({name}), got {:?}", driven.result.as_ref().map(|output| output.effects.len())));
            }
            // A decline while running may follow `v2Tokenize` counts, which TS makes again without a visible effect; the
            // owner drops what it took before them. Admission declines before any.
            if !running && (!driven.tokenizer.is_empty() || !driven.effects.is_empty()) {
                differ("native", format!("declined after {} host requests and {} effects", driven.tokenizer.len(), driven.effects.len()));
            }
            if driven.after_error != 0 { differ("native", format!("{} effects after the decline", driven.after_error)); }
            continue;
        }
        let hosts: Vec<serde_json::Value> = driven.hosts.iter().map(|host| host_call(host, &budget)).collect();
        if hosts != result.hosts { differ("hosts", format!("ts {:?}\n rust {hosts:?}", result.hosts)); }
        if tape.consumed() != result.random { differ("random", format!("ts {} rust {}", result.random, tape.consumed())); }
        let tokenizer: Vec<serde_json::Value> = driven.tokenizer.iter().cloned().map(serde_json::Value::String).collect();
        if tokenizer != result.tokenizer { differ("tokenizer", format!("ts {:?}\n rust {:?}", result.tokenizer, driven.tokenizer)); }
        if !driven.tokenizer.is_empty() { tokenized += 1; }
        let console: Vec<Vec<String>> = driven.console.iter().map(|line| vec![line.clone()]).collect();
        if console != result.console { differ("console", format!("ts {:?} rust {console:?}", result.console)); }
        let trigger_id = match (&driven.result, &result.error) {
            (Ok(output), _) => output.current_trigger_id.as_ref().map(string),
            // The owner keeps its previous ID after a failed invocation.
            (Err(_), _) => previous.clone(),
        };
        if trigger_id != result.trigger_id { differ("triggerId", format!("ts {:?} rust {trigger_id:?}", result.trigger_id)); }
        match apply(chat, &earlier, &driven.effects, &budget) {
            Ok(after) => {
                if after.writes != result.writes { differ("writes", format!("ts {:?}\n rust {:?}", result.writes, after.writes)); }
                let messages = serde_json::to_value(&after.messages).unwrap();
                if messages != result.messages { differ("messages", format!("ts {}\n rust {messages}", result.messages)); }
                if after.note != result.note { differ("note", format!("ts {:?} rust {:?}", result.note, after.note)); }
                if after.scriptstate != result.scriptstate {
                    differ("scriptstate", format!("ts {:?}\n rust {:?}", result.scriptstate, after.scriptstate));
                }
            }
            Err(error) => differ("effects", error),
        }
        match (&driven.result, &result.error) {
            (Ok(output), None) => {
                let system = SystemPrompt { start: string(&output.system_prompt_start), historyend: string(&output.system_prompt_history_end),
                    promptend: string(&output.system_prompt_end) };
                match result.returned.as_str() {
                    "result" => {
                        if Some(&system) != result.system_prompt.as_ref() {
                            differ("systemPrompt", format!("ts {:?} rust {system:?}", result.system_prompt));
                        }
                        if Some(output.tokens) != result.tokens { differ("tokens", format!("ts {:?} rust {}", result.tokens, output.tokens)); }
                        if Some(output.stop_sending) != result.stop_sending {
                            differ("stopSending", format!("ts {:?} rust {}", result.stop_sending, output.stop_sending));
                        }
                        if Some(output.send_ai_prompt) != result.send_ai_prompt {
                            differ("sendAIprompt", format!("ts {:?} rust {}", result.send_ai_prompt, output.send_ai_prompt));
                        }
                    }
                    "null" => {
                        nulls += 1;
                        let empty = SystemPrompt { start: String::new(), historyend: String::new(), promptend: String::new() };
                        if system != empty || output.tokens != 0 || output.stop_sending || output.send_ai_prompt {
                            differ("returned", "TS returned null but the run produced output".into());
                        }
                    }
                    other => differ("returned", format!("ts returned {other}, rust finished")),
                }
                compared += 1;
            }
            (Err(error), Some(thrown)) => {
                let same = match error {
                    StageError::Invalid(_) => thrown.name == "SyntaxError" || thrown.name == "TypeError",
                    StageError::Limit(name) => name == "rng-tape" && thrown.message == "rng-tape exhausted",
                    // A host call that threw, as the scripted `requestChatData` throws an `Error`.
                    StageError::Host(message) => thrown.name == "Error" && thrown.message == String::from_utf16_lossy(message),
                    _ => false,
                };
                if !same { differ("error", format!("ts {}: {} rust {error:?}", thrown.name, thrown.message)); }
                compared += 1;
                failures += 1;
            }
            (Ok(_), Some(thrown)) => differ("error", format!("ts threw {}: {}, rust finished", thrown.name, thrown.message)),
            (Err(error), None) => differ("error", format!("rust failed with {error:?}")),
        }
    }
    println!("trigger oracle: {compared} compared ({failures} errors, {nulls} without triggers, {tokenized} with token requests), \
        declined {declined:?} ({in_admission} in admission, while running {while_running:?}); {} mismatches", mismatches.len());
    assert!(in_admission > 0 && !while_running.is_empty(), "admission declined {in_admission}, the runs {while_running:?}");
    assert!(compared >= 170, "only {compared} cases compared");
    assert!(failures >= 3 && tokenized >= 3, "{failures} errors and {tokenized} token cases compared");
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
}
