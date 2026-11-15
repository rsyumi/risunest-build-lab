//! Replays `tests/fixtures/rust-core/templates/glue.*.json`, recorded from the real `applyChatTemplate` and
//! `templateCheck` by `benchmarks/rust-core-oracles/templates-glue.oracle.test.ts`.
//!
//! A rendered prompt or warning list must match exactly; a case TS throws on must be an error that is not
//! `Unsupported`. A case marked `decline` must answer that `Unsupported`, and no other case may decline. The
//! `builtins` case pins `CHAT_TEMPLATES` to the TS `chatTemplates` sources and the template effects the port hard-codes.

use risunest_ir::budget::{Budget, Limits};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::json::parse_json;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_jinja::{CaseTables, RenderOptions};
use risunest_prompt::templates::chat_template::{apply_chat_template, ChatTemplateSettings, CHAT_TEMPLATES};
use risunest_prompt::templates::template_check::template_check;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/templates/glue.cases.json");
const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/templates/glue.expected.json");

#[derive(Deserialize)]
struct CaseFile { schema: u32, cases: Vec<Case> }

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum Case {
    Builtins { id: String },
    Chat { id: String, messages: Vec<Message>, settings: Settings, #[serde(default)] decline: Option<String> },
    Check {
        id: String,
        #[serde(rename = "promptTemplate", default)]
        prompt_template: Option<serde_json::Value>,
        #[serde(default)]
        decline: Option<String>,
    },
}

#[derive(Deserialize)]
struct Message { role: String, content: String }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(rename = "type")]
    kind: Option<String>,
    custom: Option<String>,
    #[serde(rename = "instructChatTemplate")]
    instruct_chat_template: Option<String>,
    #[serde(rename = "JinjaTemplate")]
    jinja_template: Option<String>,
    #[serde(rename = "charName")]
    char_name: Option<String>,
    #[serde(rename = "userName")]
    user_name: String,
}

#[derive(Deserialize)]
struct ExpectedFile {
    schema: u32,
    #[serde(rename = "fixtureSha256")]
    fixture_sha256: String,
    cases: Vec<ExpectedCase>,
}

#[derive(Deserialize)]
struct ExpectedCase { id: String, result: serde_json::Value }

fn budget() -> Budget {
    let limits = Limits {
        max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22, max_depth: 64, max_work: 1 << 30,
    };
    Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
}

fn string(text: &Text) -> String { String::from_utf16(&text.units().collect::<Vec<_>>()).expect("well-formed output") }

fn role(name: &str) -> Role {
    match name { "user" => Role::User, "assistant" => Role::Assistant, "system" => Role::System, "function" => Role::Function, other => panic!("{other}") }
}

/// `Ok` of the recorded `ok`, `Err` of a recorded throw.
fn recorded(result: &serde_json::Value) -> Result<&serde_json::Value, ()> {
    match (result.get("ok"), result.get("error")) {
        (Some(ok), None) => Ok(ok),
        (None, Some(_)) => Err(()),
        _ => panic!("result shape {result}"),
    }
}

fn compare<T>(id: &str, decline: Option<&String>, outcome: Result<T, StageError>, expected: &serde_json::Value, show: impl Fn(&T) -> serde_json::Value) -> Option<String> {
    if let Some(name) = decline {
        return match outcome {
            Err(StageError::Unsupported(found)) if found == *name => None,
            _ => Some(format!("{id}: expected decline {name}")),
        };
    }
    match (outcome, recorded(expected)) {
        (Err(StageError::Unsupported(name)), _) => Some(format!("{id}: undeclared decline {name}")),
        (Ok(value), Ok(ok)) => (show(&value) != *ok).then(|| format!("{id}:\n  rust {}\n  ts   {ok}", show(&value))),
        (Err(_), Err(())) => None,
        (Ok(value), Err(())) => Some(format!("{id}: ts threw, rust gave {}", show(&value))),
        (Err(error), Ok(ok)) => Some(format!("{id}: ts gave {ok}, rust failed {error:?}")),
    }
}

#[test]
fn replays_the_template_glue_oracle() {
    let cases: CaseFile = serde_json::from_slice(CASES).expect("cases");
    let expected: ExpectedFile = serde_json::from_slice(EXPECTED).expect("expected");
    assert_eq!(expected.schema, cases.schema);
    let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(expected.fixture_sha256, digest, "regenerate the expected file");
    assert_eq!(expected.cases.len(), cases.cases.len());
    let mut failures = Vec::new();
    let (mut chats, mut checks) = (0, 0);
    for (case, recorded_case) in cases.cases.iter().zip(&expected.cases) {
        let budget = budget();
        let failure = match case {
            Case::Builtins { id } => {
                assert_eq!(*id, recorded_case.id);
                let result = &recorded_case.result;
                let templates: Vec<(String, String)> = serde_json::from_value(result["templates"].clone()).expect("templates");
                let ours: Vec<(String, String)> = CHAT_TEMPLATES.iter().map(|(name, source)| (name.to_string(), source.to_string())).collect();
                assert_eq!(ours, templates, "CHAT_TEMPLATES differs from chatTemplates");
                // `effects` in chat_template.rs: gemma drops system messages, mistral also merges roles to alternate.
                assert_eq!(result["effects"], serde_json::json!([["gemma", ["no_system_messages"]], ["mistral", ["no_system_messages", "alter_user_assistant_roles"]]]));
                None
            }
            Case::Chat { id, messages, settings, decline } => {
                assert_eq!(*id, recorded_case.id);
                chats += 1;
                let text = |value: &Option<String>| value.as_deref().map(|value| Text::from_str(value, &budget).unwrap());
                let (kind, custom, instruct, jinja, char_name) = (text(&settings.kind), text(&settings.custom), text(&settings.instruct_chat_template), text(&settings.jinja_template), text(&settings.char_name));
                let user_name = Text::from_str(&settings.user_name, &budget).unwrap();
                let prompt: Vec<PromptMessage> = messages.iter().map(|message| PromptMessage::new(role(&message.role), Text::from_str(&message.content, &budget).unwrap())).collect();
                let settings = ChatTemplateSettings {
                    type_override: kind.as_ref(), custom: custom.as_ref(), instruct_chat_template: instruct.as_ref(), jinja_template: jinja.as_ref(),
                    char_name: char_name.as_ref(), user_name: &user_name,
                };
                // The recording's Node 24.12 maps case with Unicode 16 tables.
                let options = RenderOptions { case_tables: CaseTables::Unicode16, ..RenderOptions::default() };
                let outcome = apply_chat_template(&prompt, &settings, &options, &budget);
                compare(id, decline.as_ref(), outcome, &recorded_case.result, |text| serde_json::Value::String(string(text)))
            }
            Case::Check { id, prompt_template, decline } => {
                assert_eq!(*id, recorded_case.id);
                checks += 1;
                let value = match prompt_template {
                    Some(json) => parse_json(&json.to_string(), &budget).expect("promptTemplate"),
                    None => Value::Undefined,
                };
                let outcome = template_check(&value, &budget);
                compare(id, decline.as_ref(), outcome, &recorded_case.result, |warnings| warnings.iter().map(|text| serde_json::Value::String(string(text))).collect())
            }
        };
        if let Some(failure) = failure { failures.push(failure); }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.cases.len(), failures.join("\n"));
    assert!(chats > 0 && checks > 0);
    println!("template glue oracle: {chats} chat cases, {checks} check cases");
}
