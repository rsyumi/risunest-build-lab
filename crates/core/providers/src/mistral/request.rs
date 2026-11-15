//! The Mistral branch of `requestOpenAI` (`openAI/requests.ts`, `LLMFormat.Mistral`): the chat is folded into strictly
//! alternating `system`/`user`/`assistant` messages and sent to `v1/chat/completions` with four sampling parameters.

use crate::common::built::{finish, utf8, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::params;
use crate::common::settings::SamplingSettings;
use crate::common::BuildError;
use risunest_ir::chat::Role;
use risunest_ir::provider::{LlmFlag, LlmFormat, ProviderCall};
use risunest_ir::{Budget, Text};

const DEFAULT_URL: &str = "https://api.mistral.ai/v1/chat/completions";

/// The database fields the Mistral branch and the shared message preparation read.
pub struct MistralSettings {
    /// `db.mistralKey`.
    pub mistral_key: Text,
    /// `db.newOAIHandle`.
    pub new_oai_handle: bool,
    /// `db.reverseProxyOobaMode`.
    pub reverse_proxy_ooba_mode: bool,
}

impl std::fmt::Debug for MistralSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("MistralSettings").field("mistral_key", &"<redacted>")
            .field("new_oai_handle", &self.new_oai_handle).field("reverse_proxy_ooba_mode", &self.reverse_proxy_ooba_mode).finish()
    }
}

/// What `requestChatDataMain` takes from `arg` besides the [`ProviderCall`].
pub struct MistralInputs<'a> {
    /// `targ.aiModel`, which is also the `model` sent.
    pub ai_model: &'a Text,
    /// `arg.key`.
    pub key: Option<&'a Text>,
    /// `arg.customURL`.
    pub custom_url: Option<&'a Text>,
    /// `arg.maxTokens`.
    pub max_tokens: f64,
    pub sampling: &'a SamplingSettings,
    pub settings: &'a MistralSettings,
}

/// A chat entry as the fold reads it: `role` and `content`.
struct Entry {
    role: &'static str,
    content: Text,
}

fn role_name(role: Role) -> &'static str {
    match role { Role::System => "system", Role::User => "user", Role::Assistant => "assistant", Role::Function => "function" }
}

fn entry_json(entry: &Entry, budget: &Budget) -> Result<J, BuildError> {
    let mut object = js::new_object();
    js::set_str(&mut object, "role", js::str_value(entry.role, budget)?, budget)?;
    js::set_str(&mut object, "content", J::Str(entry.content.copy(budget)?), budget)?;
    Ok(object)
}

fn join(parts: &[&Text], budget: &Budget) -> Result<Text, BuildError> {
    let mut output = js::text("", budget)?;
    for part in parts { output.append(part)?; }
    Ok(output)
}

/// Builds the request. `call.messages` are the messages after `reformat`. Tool-call markers and user images turn the
/// chat into tool calls or content parts, which this branch does not model; they are declined.
pub fn build_request(call: &ProviderCall, inputs: &MistralInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    let settings = inputs.settings;
    if call.model.format != LlmFormat::Mistral { return Err(BuildError::Unsupported("format")); }
    if inputs.ai_model.equals_ascii(b"reverse_proxy") && settings.reverse_proxy_ooba_mode { return Err(BuildError::Unsupported("reverse-proxy-ooba-mode")); }

    // The preparation `requestOpenAI` does for every format.
    let mut chat: Vec<(Role, Text, bool)> = Vec::new();
    for message in &call.messages {
        let marker = js::find_units(&js::units(&message.content), &js::utf16("<tool_call>"), 0).is_some();
        if message.content.len_units() != 0 && marker { return Err(BuildError::Unsupported("tool-call-marker")); }
        if message.role == Role::User && message.multimodals.as_ref().is_some_and(|items| !items.is_empty()) {
            return Err(BuildError::Unsupported("multimodal-user-message"));
        }
        let mut content = message.content.copy(budget)?;
        let mut keeps_media = false;
        if message.role != Role::Function {
            if settings.new_oai_handle && message.memo.as_ref().is_some_and(|memo| js::starts_with(memo, "NewChat")) { content = js::text("", budget)?; }
        } else {
            keeps_media = message.multimodals.as_ref().is_some_and(|items| !items.is_empty());
        }
        chat.push((message.role, content, keeps_media));
    }
    if settings.new_oai_handle { chat.retain(|(_, content, keeps_media)| content.len_units() != 0 || *keeps_media); }
    let developer = call.model.flags.contains(&LlmFlag::DeveloperRole);

    // The fold into alternating roles.
    let mut folded: Vec<Entry> = Vec::new();
    for (index, (role, content, _)) in chat.iter().enumerate() {
        let name = if developer && *role == Role::System { "developer" } else { role_name(*role) };
        if index == 0 {
            if name == "user" || name == "system" {
                folded.push(Entry { role: if name == "user" { "user" } else { "system" }, content: content.copy(budget)? });
            } else {
                let colon = js::text(":", budget)?;
                let label = js::text(name, budget)?;
                folded.push(Entry { role: "system", content: join(&[&label, &colon, content], budget)? });
            }
            continue;
        }
        let newline = js::text("\n", budget)?;
        if folded.last().is_some_and(|previous| previous.role == name) {
            let last = folded.last_mut().expect("checked");
            last.content = join(&[&last.content, &newline, content], budget)?;
        } else if name == "system" {
            let label = js::text("System:", budget)?;
            if folded.last().is_some_and(|previous| previous.role == "user") {
                let last = folded.last_mut().expect("checked");
                last.content = join(&[&last.content, &newline, &label, content], budget)?;
            } else {
                folded.push(Entry { role: "user", content: join(&[&label, content], budget)? });
            }
        } else if name == "function" {
            folded.push(Entry { role: "user", content: content.copy(budget)? });
        } else {
            folded.push(Entry { role: if name == "user" { "user" } else if name == "assistant" { "assistant" } else { "developer" }, content: content.copy(budget)? });
        }
    }

    let mut body = js::new_object();
    js::set_str(&mut body, "model", J::Str(inputs.ai_model.copy(budget)?), budget)?;
    let messages = folded.iter().map(|entry| entry_json(entry, budget)).collect::<Result<Vec<_>, _>>()?;
    js::set_str(&mut body, "messages", J::Arr(messages), budget)?;
    js::set_str(&mut body, "safe_prompt", J::Bool(false), budget)?;
    js::set_str(&mut body, "max_tokens", J::Num(inputs.max_tokens), budget)?;
    let names: Vec<Text> = ["temperature", "presence_penalty", "frequency_penalty", "top_p"].iter().map(|name| js::text(name, budget)).collect::<Result<_, _>>()?;
    for (name, value) in params::apply_parameters(&names, inputs.sampling, &call.mode, &call.model.id, budget)? {
        js::set(&mut body, name, js::from_value(&value, budget)?, budget)?;
    }

    let key = inputs.key.unwrap_or(&settings.mistral_key);
    let mut headers = js::new_object();
    js::set_str(&mut headers, "Authorization", J::Str(js::concat(&js::text("Bearer ", budget)?, key, budget)?), budget)?;
    let url = match inputs.custom_url { Some(url) => utf8(url)?, None => DEFAULT_URL.to_owned() };
    finish(url, body, &headers, false, budget)
}

