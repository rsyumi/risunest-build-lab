//! `applyChatTemplate` (`src/ts/process/templates/chatTemplate.ts`): the prompt array rendered through a built-in
//! instruct template or the user's Jinja template, for the text-completion providers.

use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};
use risunest_jinja::{RenderOptions, Template};
use std::borrow::Cow;

/// `chatTemplates`, the built-in templates by `instructChatTemplate` name.
pub const CHAT_TEMPLATES: [(&str, &str); 8] = [
    ("llama3", "{% set bos_token = '<|begin_of_text|>' %}{% set loop_messages = messages %}{% for message in loop_messages %}{% set content = '<|start_header_id|>' + message['role'] + '<|end_header_id|>\n\n'+ message['content'] | trim + '<|eot_id|>' %}{% if loop.index0 == 0 %}{% set content = bos_token + content %}{% endif %}{{ content }}{% endfor %}{{ '<|start_header_id|>assistant<|end_header_id|>\n\n' }}"),
    ("llama2", "{% set bos_token = '<|begin_of_text|>' %}{% if messages[0]['role'] == 'system' %}{% set loop_messages = messages[1:] %}{% set system_message = messages[0]['content'] %}{% elif USE_DEFAULT_PROMPT == true and not '<<SYS>>' in messages[0]['content'] %}{% set loop_messages = messages %}{% set system_message = 'DEFAULT_SYSTEM_MESSAGE' %}{% else %}{% set loop_messages = messages %}{% set system_message = false %}{% endif %}{% for message in loop_messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if loop.index0 == 0 and system_message != false %}{% set content = '<<SYS>>\n' + system_message + '\n<</SYS>>\n\n' + message['content'] %}{% else %}{% set content = message['content'] %}{% endif %}{% if message['role'] == 'user' %}{{ bos_token + '[INST] ' + content.strip() + ' [/INST]' }}{% elif message['role'] == 'system' %}{{ '<<SYS>>\n' + content.strip() + '\n<</SYS>>\n\n' }}{% elif message['role'] == 'assistant' %}{{ ' '  + content.strip() + ' ' + eos_token }}{% endif %}{% endfor %}"),
    ("chatml", "{% for message in messages %}{{'<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n'}}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}"),
    ("gpt2", "{% for message in messages %}{{'<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n'}}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}"),
    ("gemma", "{% if messages[0]['role'] == 'system' %}{{ raise_exception('System role not supported') }}{% endif %}{% for message in messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if (message['role'] == 'assistant') %}{% set role = 'model' %}{% else %}{% set role = message['role'] %}{% endif %}{{ '<start_of_turn>' + role + '\n' + message['content'] | trim + '<end_of_turn>\n' }}{% endfor %}{% if add_generation_prompt %}{{'<start_of_turn>model\n'}}{% endif %}"),
    ("mistral", "{% for message in messages %}{% if message['role'] == 'user' %}{{ ' [INST] ' + message['content'] + ' [/INST]' }}{% elif message['role'] == 'assistant' %}{{ ' ' + message['content'] + ' ' + eos_token}}{% else %}{{ raise_exception('Only user and assistant roles are supported!') }}{% endif %}{% endfor %}"),
    ("vicuna", "{%- set ns = namespace(found=false) -%}\n{%- for message in messages -%}\n    {%- if message['role'] == 'system' -%}\n        {%- set ns.found = true -%}\n    {%- endif -%}\n{%- endfor -%}\n{%- if not ns.found -%}\n    {{- '' + 'A chat between a curious user and an artificial intelligence assistant. The assistant gives helpful, detailed, and polite answers to the user\\'s questions.' + '\\n\\n' -}}\n{%- endif %}\n{%- for message in messages %}\n    {%- if message['role'] == 'system' -%}\n        {{- '' + message['content'] + '\\n\\n' -}}\n    {%- else -%}\n        {%- if message['role'] == 'user' -%}\n            {{-'USER: ' + message['content'] + '\\n'-}}\n        {%- else -%}\n            {{-'ASSISTANT: ' + message['content'] + '</s>\\n' -}}\n        {%- endif -%}\n    {%- endif -%}\n{%- endfor -%}\n{%- if add_generation_prompt -%}\n    {{-'ASSISTANT:'-}}\n{%- endif -%}"),
    ("alpaca", "{%- set ns = namespace(found=false) -%}\n{%- for message in messages -%}\n    {%- if message['role'] == 'system' -%}\n        {%- set ns.found = true -%}\n    {%- endif -%}\n{%- endfor -%}\n{%- if not ns.found -%}\n    {{- '' + 'Below is an instruction that describes a task. Write a response that appropriately completes the request.' + '\\n\\n' -}}\n{%- endif %}\n{%- for message in messages %}\n    {%- if message['role'] == 'system' -%}\n        {{- '' + message['content'] + '\\n\\n' -}}\n    {%- else -%}\n        {%- if message['role'] == 'user' -%}\n            {{-'### Instruction:\\n' + message['content'] + '\\n\\n'-}}\n        {%- else -%}\n            {{-'### Response:\\n' + message['content'] + '\\n\\n' -}}\n        {%- endif -%}\n    {%- endif -%}\n{%- endfor -%}\n{%- if add_generation_prompt -%}\n    {{-'### Response:\\n'-}}\n{%- endif -%}"),
];

/// The settings `applyChatTemplate` reads.
#[derive(Debug, Clone, Copy)]
pub struct ChatTemplateSettings<'a> {
    /// `arg.type`, which wins over `instructChatTemplate` unless it is absent.
    pub type_override: Option<&'a Text>,
    /// `arg.custom`, which wins over `JinjaTemplate` unless it is absent.
    pub custom: Option<&'a Text>,
    /// `db.instructChatTemplate`: a [`CHAT_TEMPLATES`] name or `jinja`.
    pub instruct_chat_template: Option<&'a Text>,
    /// `db.JinjaTemplate`.
    pub jinja_template: Option<&'a Text>,
    /// `getCurrentCharacter().name`, the template's `risu_char`; `None` renders as undefined.
    pub char_name: Option<&'a Text>,
    /// `getUserName()`, the template's `risu_user`.
    pub user_name: &'a Text,
}

/// `templateEffect`: `(no_system_messages, alter_user_assistant_roles)` of a template name.
fn effects(name: &Text) -> (bool, bool) {
    if name.equals_ascii(b"gemma") { (true, false) } else if name.equals_ascii(b"mistral") { (true, true) } else { (false, false) }
}

fn invalid(name: &'static str) -> StageError { StageError::Invalid(Cow::Borrowed(name)) }

fn text(value: &str, budget: &Budget) -> Result<Text, StageError> { Ok(Text::from_str(value, budget)?) }

fn joined(left: &Text, separator: &str, right: &Text, budget: &Budget) -> Result<Text, StageError> {
    let mut out = left.copy(budget)?;
    out.append(&text(separator, budget)?)?;
    out.append(right)?;
    Ok(out)
}

/// Renders `messages` as `applyChatTemplate(messages, arg)` does. A template the JS engine rejects is `Invalid`
/// (`jinja-syntax`, `jinja-runtime` or `jinja-raise-exception`); a template the engine port refuses is `Unsupported`.
pub fn apply_chat_template(messages: &[PromptMessage], settings: &ChatTemplateSettings<'_>, options: &RenderOptions, budget: &Budget) -> Result<Text, StageError> {
    let name = settings.type_override.or(settings.instruct_chat_template);
    // `if(!type) throw`.
    let Some(name) = name.filter(|name| name.len_units() != 0) else { return Err(invalid("instructChatTemplate")) };
    let source = if name.equals_ascii(b"jinja") {
        settings.custom.or(settings.jinja_template).ok_or_else(|| invalid("JinjaTemplate"))?.copy(budget)?
    } else {
        // `new Template(undefined)` throws for a name `chatTemplates` lacks.
        let (_, source) = CHAT_TEMPLATES.iter().find(|(known, _)| name.equals_ascii(known.as_bytes())).ok_or_else(|| invalid("instructChatTemplate"))?;
        text(source, budget)?
    };
    let template = Template::new(&source, budget)?;

    let (no_system, alternate) = effects(name);
    let mut formatted: Vec<(Role, Text)> = Vec::new();
    for message in messages {
        budget.work(1)?;
        let (mut role, mut content) = match message.role {
            Role::User | Role::Assistant | Role::System => (message.role, message.content.copy(budget)?),
            Role::Function => continue,
        };
        if no_system && role == Role::System {
            role = Role::User;
            content = joined(&text("System: ", budget)?, "", &content, budget)?;
        }
        if !alternate {
            formatted.push((role, content));
            continue;
        }
        let even = formatted.len() % 2 == 0;
        if role == Role::User {
            if even { formatted.push((Role::User, content)); } else { merge(&mut formatted, &content, budget)?; }
        } else if !even || formatted.is_empty() {
            if formatted.is_empty() { formatted.push((Role::User, Text::from_str("", budget)?)); }
            formatted.push((Role::Assistant, content));
        } else {
            merge(&mut formatted, &content, budget)?;
        }
    }

    let mut list = Value::Array(risunest_ir::OwnedVec::new(budget)?);
    if let Value::Array(items) = &mut list {
        for (role, content) in formatted {
            let mut entry = Value::Object(risunest_ir::OwnedVec::new(budget)?);
            let role = match role { Role::User => "user", Role::Assistant => "assistant", _ => "system" };
            entry.define(text("role", budget)?, Value::Text(text(role, budget)?), budget)?;
            entry.define(text("content", budget)?, Value::Text(content), budget)?;
            items.push(entry)?;
        }
    }
    let mut context = Value::Object(risunest_ir::OwnedVec::new(budget)?);
    context.define(text("messages", budget)?, list, budget)?;
    context.define(text("add_generation_prompt", budget)?, Value::Bool(true), budget)?;
    let char_name = match settings.char_name { Some(name) => Value::Text(name.copy(budget)?), None => Value::Undefined };
    context.define(text("risu_char", budget)?, char_name, budget)?;
    context.define(text("risu_user", budget)?, Value::Text(settings.user_name.copy(budget)?), budget)?;
    context.define(text("eos_token", budget)?, Value::Text(text("", budget)?), budget)?;
    context.define(text("bos_token", budget)?, Value::Text(text("", budget)?), budget)?;
    Ok(template.render(&context, options, budget)?)
}

/// `formatedMessages[formatedMessages.length - 1].content += "\n" + message.content`.
fn merge(formatted: &mut [(Role, Text)], content: &Text, budget: &Budget) -> Result<(), StageError> {
    if let Some(last) = formatted.last_mut() { last.1 = joined(&last.1, "\n", content, budget)?; }
    Ok(())
}
