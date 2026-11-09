//! Replay helpers the text-completion families share: the fixture database, the reformatted messages, the scripted
//! replies and the request and outcome comparisons.

use crate::common::owner::{self, BufferedReply};
use crate::common::params;
use crate::common::reformat::{reformat, SystemReplacement};
use crate::common::replay_kit::{lossy, message, model_descriptor, pair_list, sampling_settings, t, to_value, Db, Jv};
use crate::common::settings::SamplingSettings;
use crate::common::BuildError;
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ModelDescriptor};
use risunest_ir::{json, Budget, Text, Value};

pub(crate) fn s(text: &Text) -> String { lossy(&text.units().collect::<Vec<_>>()) }

pub(crate) fn ids_of(cases: &[Jv], field: &str) -> Vec<String> {
    cases.iter().filter(|entry| entry.has(field)).map(|entry| entry.need("id").string()).collect()
}

/// What every family reads from a case before its own settings.
pub(crate) struct Common<'a> {
    pub(crate) db: Db<'a>,
    pub(crate) ai_model: Text,
    pub(crate) descriptor: ModelDescriptor,
    pub(crate) mode: Text,
    pub(crate) additional_params: Vec<(Text, Text)>,
    pub(crate) sampling: SamplingSettings,
    /// `db.forceReplaceUrl` for `reverse_proxy`, the custom model URL for `xcustom:::`.
    pub(crate) custom_url: Option<Text>,
}

pub(crate) fn common<'a>(fixture: &'a Jv, entry: &'a Jv, model: &Jv, b: &Budget) -> Common<'a> {
    let db = Db::new(fixture, entry);
    let ai_model = entry.need("model").text(b);
    let custom_models: Vec<Jv> = entry.get("customModels").map(|items| items.list().to_vec()).unwrap_or_default();
    let found = custom_models.iter().find(|found| found.need("id").text(b).equals(&ai_model));
    let custom_params = found.and_then(|found| found.get("params")).map(|value| value.text(b));
    let additional_params = params::additional_parameters(
        &ai_model, &pair_list(db.need("additionalParams"), b), db.flag("applyAdditionalParamsToAll"), custom_params.as_ref(), b,
    ).unwrap();
    let custom_url = if ai_model.equals_ascii(b"reverse_proxy") { Some(db.text("forceReplaceUrl", b)) }
        else { found.and_then(|found| found.get("url")).map(|value| value.text(b)) };
    let mode = t(&entry.get("mode").map(Jv::string).unwrap_or_else(|| "model".to_owned()), b);
    let sampling = sampling_settings(&db, b);
    Common { db, ai_model, descriptor: model_descriptor(model, b), mode, additional_params, sampling, custom_url }
}

/// `reformater(formated, modelInfo)` of the case messages.
pub(crate) fn messages(entry: &Jv, common: &Common<'_>, b: &Budget) -> Result<Vec<PromptMessage>, BuildError> {
    let messages = entry.need("messages").list().iter().map(|item| message(item, b)).collect();
    let replacement = Some(common.db.text("systemContentReplacement", b)).filter(|value| value.len_units() != 0);
    let role = match common.db.need("systemRoleReplacement").string().as_str() {
        "" => None, "system" => Some(Role::System), "user" => Some(Role::User), "assistant" => Some(Role::Assistant), other => panic!("role {other}"),
    };
    reformat(messages, &common.descriptor.flags, &SystemReplacement { content: replacement.as_ref(), role }, b)
}

pub(crate) fn assert_request(id: &str, spec: &HttpRequestSpec, want: &Jv) {
    let headers: Vec<(String, String)> = want.need("headers").list().iter().map(|pair| (pair.list()[0].string(), pair.list()[1].string())).collect();
    let method = match want.need("method").string().as_str() { "POST" => HttpMethod::Post, "GET" => HttpMethod::Get, "DELETE" => HttpMethod::Delete, other => panic!("{id}: method {other}") };
    assert_eq!(spec.method, method, "{id}: method");
    assert_eq!(spec.url, want.need("url").string(), "{id}: url");
    assert_eq!(spec.headers, headers, "{id}: headers");
    match want.get("body") {
        Some(Jv::Null) | None => assert!(spec.body.is_empty(), "{id}: body"),
        Some(body) => assert_eq!(String::from_utf8(spec.body.clone()).expect("utf-8"), body.string(), "{id}: body"),
    }
}

/// A scripted reply, owned so that a [`BufferedReply`] can borrow it.
pub(crate) enum OwnedReply { Json { status: u16, data: Value }, NotJson { status: u16, body: Text }, Transport { error: Text } }

impl OwnedReply {
    pub(crate) fn borrow(&self) -> BufferedReply<'_> {
        match self {
            Self::Json { status, data } => BufferedReply::Json { status: *status, data },
            Self::NotJson { status, body } => BufferedReply::NotJson { status: *status, body },
            Self::Transport { error } => BufferedReply::Transport { error },
        }
    }
}

pub(crate) fn json_text(value: &Jv, b: &Budget) -> String {
    match value {
        Jv::Null => "null".into(),
        Jv::Bool(flag) => flag.to_string(),
        Jv::Num(number) => number.to_string(),
        other => s(&json::stringify(&to_value(other, b), b).unwrap().expect("a value")),
    }
}

/// The body bytes of a scripted reply.
pub(crate) fn reply_bytes(reply: &Jv, b: &Budget) -> Vec<u8> {
    match reply.get("text") { Some(text) => text.string().into_bytes(), None => json_text(reply.need("json"), b).into_bytes() }
}

/// What `globalFetch` hands back for a scripted reply.
pub(crate) fn reply(reply: &Jv, b: &Budget) -> OwnedReply {
    let status = reply.get("status").map(Jv::num).unwrap_or(200.0) as u16;
    if let Some(message) = reply.get("throws") { return OwnedReply::Transport { error: t(&format!("TypeError: {}", message.string()), b) }; }
    let body = owner::response_text(&reply_bytes(reply, b), b).unwrap();
    match json::parse_json(&s(&body), b) {
        Ok(data) => OwnedReply::Json { status, data },
        Err(_) => OwnedReply::NotJson { status, body },
    }
}

/// Checks a TS `thrown` record against the message the native path produced.
pub(crate) fn assert_thrown(id: &str, record: &Jv, text: &Text) {
    assert_eq!(record.need("type").string(), "thrown", "{id}");
    let error = record.need("error").string();
    assert!(error.ends_with(&format!(": {}", s(text))), "{id}: threw `{}`, TypeScript `{error}`", s(text));
}
