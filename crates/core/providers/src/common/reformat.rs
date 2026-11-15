//! `reformater` (`src/ts/process/request/request.ts:362`): the flag-driven message shaping that runs before every
//! provider format.

use super::js;
use super::BuildError;
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::provider::LlmFlag;
use risunest_ir::{Budget, Text};

/// `db.systemContentReplacement` and `db.systemRoleReplacement`; an empty content or no role uses the defaults.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemReplacement<'a> {
    pub content: Option<&'a Text>,
    pub role: Option<Role>,
}

fn flagged(flags: &[LlmFlag], flag: LlmFlag) -> bool { flags.contains(&flag) }

/// `reformater(formated, modelInfo)`. A conversation made only of system messages with `HasFirstSystemPrompt` throws in
/// TS (`formated[0].role` of an empty array) and returns [`BuildError::Thrown`] here.
pub fn reformat(
    mut formated: Vec<PromptMessage>, flags: &[LlmFlag], settings: &SystemReplacement<'_>, budget: &Budget,
) -> Result<Vec<PromptMessage>, BuildError> {
    let mut system_prompt: Option<PromptMessage> = None;
    if !flagged(flags, LlmFlag::HasFullSystemPrompt) {
        if flagged(flags, LlmFlag::HasFirstSystemPrompt) {
            let mut rest = formated.into_iter().peekable();
            loop {
                match rest.peek() {
                    None => return Err(BuildError::Thrown(js::text("Cannot read properties of undefined (reading 'role')", budget)?)),
                    Some(first) if first.role == Role::System => {}
                    Some(_) => break,
                }
                let first = rest.next().expect("peeked");
                match system_prompt.as_mut() {
                    Some(prompt) => {
                        prompt.content.append(&js::text("\n\n", budget)?)?;
                        prompt.content.append(&first.content)?;
                    }
                    None => system_prompt = Some(first),
                }
            }
            formated = rest.collect();
        }
        let replacement = settings.content.filter(|value| value.len_units() != 0);
        for message in formated.iter_mut() {
            if message.role != Role::System { continue; }
            message.content = match replacement {
                Some(template) => js::replace_first(template, "{{slot}}", &message.content, budget)?,
                None => js::concat(&js::text("system: ", budget)?, &message.content, budget)?,
            };
            message.role = settings.role.unwrap_or(Role::User);
        }
    }

    if flagged(flags, LlmFlag::RequiresAlternateRole) {
        let mut merged: Vec<PromptMessage> = Vec::new();
        for message in formated {
            let Some(last) = merged.last_mut() else { merged.push(message); continue };
            if last.role != message.role { merged.push(message); continue; }
            last.content.append(&js::text("\n", budget)?)?;
            last.content.append(&message.content)?;
            if let Some(extra) = message.multimodals { last.multimodals.get_or_insert_with(Vec::new).extend(extra); }
            if let Some(extra) = message.thoughts { last.thoughts.get_or_insert_with(Vec::new).extend(extra); }
            if message.cache_point == Some(true) && last.cache_point != Some(true) { last.cache_point = Some(true); }
        }
        formated = merged;
    }

    if flagged(flags, LlmFlag::MustStartWithUserInput) && formated.first().is_none_or(|first| first.role != Role::User) {
        formated.insert(0, PromptMessage::new(Role::User, Text::from_str(" ", budget)?));
    }
    if let Some(prompt) = system_prompt { formated.insert(0, prompt); }
    Ok(formated)
}
