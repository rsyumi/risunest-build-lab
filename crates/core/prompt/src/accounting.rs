//! Token accounting and context trimming.

use crate::machine::{Ctx, Prompt};
use risunest_ir::stage::StageError;
use risunest_ir::Budget;
use risunest_records::settings::Settings;

/// The per-message accounting of the `ChatTokenizer` a send builds, which [`crate::machine::PromptStages::tokens`]
/// applies: `chatAdditonalTokens` and whether a message name counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageAccounting {
    pub additional_tokens: u32,
    pub use_name: bool,
}

/// `new ChatTokenizer(gpt ? 5 : 3, gpt ? 'noName' : 'name', true)` for the effective `aiModel`.
pub fn message_accounting(settings: &Settings<'_>, budget: &Budget) -> Result<MessageAccounting, StageError> {
    let model = crate::machine::Model::read(settings, budget)?;
    Ok(MessageAccounting { additional_tokens: if model.gpt { 5 } else { 3 }, use_name: !model.gpt })
}

/// Drops the oldest chats while the total exceeds the context. `Err` carries the required tokens when one chat is
/// left and the total still exceeds it.
pub(crate) async fn trim(ctx: &mut Ctx, prompt: &Prompt, chats: &mut Vec<usize>, mut tokens: f64, max_context: f64)
    -> Result<Result<f64, f64>, StageError> {
    let mut removed = 0;
    while tokens > max_context {
        if chats.len() - removed <= 1 { return Ok(Err(tokens)); }
        tokens -= ctx.count(&prompt.arena[chats[removed]]).await?;
        removed += 1;
    }
    chats.drain(..removed);
    Ok(Ok(tokens))
}

/// The final token recheck: the array's total, emptying removable entries from the front while it exceeds the
/// context. `Err` carries the required tokens when no removable entry is left.
pub(crate) async fn recheck(ctx: &mut Ctx, prompt: &mut Prompt, formated: &mut Vec<usize>, max_context: f64)
    -> Result<Result<f64, f64>, StageError> {
    let mut tokens = 0.0;
    for index in formated.iter() { tokens += ctx.count(&prompt.arena[*index]).await?; }
    if tokens <= max_context { return Ok(Ok(tokens)); }
    let mut pointer = 0;
    while tokens > max_context {
        let Some(&index) = formated.get(pointer) else { return Ok(Err(tokens)) };
        if prompt.arena[index].removable == Some(true) {
            tokens -= ctx.count(&prompt.arena[index]).await?;
            let budget = ctx.budget();
            prompt.arena[index].content = risunest_ir::Text::from_str("", &budget)?;
        }
        pointer += 1;
    }
    formated.retain(|index| {
        let entry = &prompt.arena[*index];
        entry.content.len_units() != 0 || entry.multimodals.as_ref().is_some_and(|values| !values.is_empty())
    });
    Ok(Ok(tokens))
}
