//! `reformater` (`src/ts/process/request/request.ts:362`) for the OpenAI-compatible settings; the shaping itself is shared
//! with the other families in [`crate::common::reformat`].

use super::settings::Settings;
use super::BuildError;
use crate::common::reformat::{reformat as shared, SystemReplacement};
use risunest_ir::chat::PromptMessage;
use risunest_ir::provider::LlmFlag;
use risunest_ir::Budget;

/// `reformater(formated, modelInfo)`. A conversation made only of system messages with `HasFirstSystemPrompt` throws in
/// TS and returns [`BuildError::Thrown`] here.
pub fn reformat(formated: Vec<PromptMessage>, flags: &[LlmFlag], settings: &Settings, budget: &Budget) -> Result<Vec<PromptMessage>, BuildError> {
    let replacement = SystemReplacement { content: settings.system_content_replacement.as_ref(), role: settings.system_role_replacement };
    shared(formated, flags, &replacement, budget)
}
