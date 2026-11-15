//! The request `requestCohere` sends: a `v1/chat` body with the last user turn as `message`, the earlier turns as
//! `chat_history` and a leading system message as `preamble`.

use crate::common::built::{finish, utf8, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::params;
use crate::common::settings::SamplingSettings;
use crate::common::BuildError;
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::provider::{LlmFormat, ProviderCall};
use risunest_ir::{Budget, Text};

const DEFAULT_URL: &str = "https://api.cohere.com/v1/chat";

/// The database fields `requestCohere` reads.
pub struct CohereSettings {
    /// `db.cohereAPIKey`.
    pub cohere_key: Text,
    /// `getAdditionalParameters(aiModel)`.
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for CohereSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CohereSettings").field("cohere_key", &"<redacted>")
            .field("additional_params", &format_args!("{} entries, <redacted>", self.additional_params.len())).finish()
    }
}

/// What `requestChatDataMain` takes from `arg` besides the [`ProviderCall`].
pub struct CohereInputs<'a> {
    /// `targ.aiModel`.
    pub ai_model: &'a Text,
    /// `arg.key`: the custom model or reverse proxy key, when there is one.
    pub key: Option<&'a Text>,
    /// `arg.customURL`.
    pub custom_url: Option<&'a Text>,
    pub sampling: &'a SamplingSettings,
    pub settings: &'a CohereSettings,
}

fn undefined_role(budget: &Budget) -> Result<BuildError, BuildError> {
    Ok(BuildError::Thrown(js::text("Cannot read properties of undefined (reading 'role')", budget)?))
}

fn history_entry(role: &str, message: &Text, budget: &Budget) -> Result<J, BuildError> {
    let mut entry = js::new_object();
    js::set_str(&mut entry, "role", js::str_value(role, budget)?, budget)?;
    js::set_str(&mut entry, "message", J::Str(message.copy(budget)?), budget)?;
    Ok(entry)
}

/// Builds the request `requestCohere` sends. `call.messages` are the messages after `reformat`. The request is always
/// buffered: `useStreaming` is not read.
pub fn build_request(call: &ProviderCall, inputs: &CohereInputs<'_>, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    if call.model.format != LlmFormat::Cohere { return Err(BuildError::Unsupported("format")); }
    let mut formated: Vec<&PromptMessage> = call.messages.iter().collect();

    // The last user turn, with the turns after it folded in.
    let mut last_prompt = js::text("", budget)?;
    let Some(last) = formated.last() else { return Err(undefined_role(budget)?) };
    if last.role == Role::User {
        last_prompt = last.content.copy(budget)?;
        formated.pop();
    } else {
        loop {
            let Some(chat) = formated.pop() else {
                return Err(BuildError::Fail(js::text("Cohere requires a user message to generate a response", budget)?));
            };
            let mut prompt = match chat.role {
                Role::User => js::text("", budget)?,
                Role::System => js::text("system: ", budget)?,
                Role::Assistant => js::text("assistant: ", budget)?,
                Role::Function => js::text("function: ", budget)?,
            };
            prompt.append(&js::text("\n", budget)?)?;
            prompt.append(&chat.content)?;
            prompt.append(&last_prompt)?;
            last_prompt = prompt;
            if chat.role == Role::User { break; }
        }
    }

    // A leading system message becomes the preamble.
    let mut preamble: Option<&Text> = None;
    let Some(first) = formated.first() else { return Err(undefined_role(budget)?) };
    if first.role == Role::System {
        preamble = Some(&first.content);
        formated.remove(0);
    }

    let mut history = Vec::new();
    for chat in &formated {
        let role = match chat.role {
            Role::Assistant => "CHATBOT",
            Role::System => "SYSTEM",
            Role::User => "USER",
            Role::Function => continue,
        };
        if chat.content.len_units() == 0 { continue; }
        history.push(history_entry(role, &chat.content, budget)?);
    }
    let has_history = !history.is_empty();

    let mut body = js::new_object();
    js::set_str(&mut body, "message", J::Str(last_prompt), budget)?;
    js::set_str(&mut body, "chat_history", J::Arr(history), budget)?;
    let names: Vec<Text> = ["temperature", "top_k", "top_p", "presence_penalty", "frequency_penalty"].iter().map(|name| js::text(name, budget)).collect::<Result<_, _>>()?;
    for (name, value) in params::apply_parameters(&names, inputs.sampling, &call.mode, &call.model.id, budget)? {
        let renamed = if name.equals_ascii(b"top_k") { js::text("k", budget)? } else if name.equals_ascii(b"top_p") { js::text("p", budget)? } else { name };
        js::set(&mut body, renamed, js::from_value(&value, budget)?, budget)?;
    }
    if !inputs.ai_model.equals_ascii(b"cohere-command-r-03-2024") && !inputs.ai_model.equals_ascii(b"cohere-command-r-plus-04-2024") {
        js::set_str(&mut body, "safety_mode", js::str_value("NONE", budget)?, budget)?;
    }
    if let Some(preamble) = preamble.filter(|text| text.len_units() != 0) {
        if has_history {
            js::set_str(&mut body, "preamble", J::Str(preamble.copy(budget)?), budget)?;
        } else {
            let mut message = js::text("system: ", budget)?;
            message.append(preamble)?;
            js::set_str(&mut body, "message", J::Str(message), budget)?;
        }
    }

    let key = inputs.key.unwrap_or(&inputs.settings.cohere_key);
    let mut headers = js::new_object();
    js::set_str(&mut headers, "Authorization", J::Str(js::concat(&js::text("Bearer ", budget)?, key, budget)?), budget)?;
    js::set_str(&mut headers, "Content-Type", js::str_value("application/json", budget)?, budget)?;
    params::apply_additional_parameters(&mut body, &mut headers, &inputs.settings.additional_params, budget)?;

    let url = match inputs.custom_url { Some(url) => utf8(url)?, None => DEFAULT_URL.to_owned() };
    finish(url, body, &headers, false, budget)
}
