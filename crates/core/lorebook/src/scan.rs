//! Scan text, scan depth and recursion.
//!
//! An entry scans the latest `scanDepth` messages (`getSliceStart`, `src/ts/process/lorebook.svelte.ts:162`) and,
//! unless `@@no_recursive_search` is set, the content of every recursively activated entry so far. Matching reads
//! each message's `data`; the `\x01{{name}}:data\x01` prompt built with `DBState.db.username` or the speaker's name
//! (`:198-211`) only reaches the match log.

use crate::matching::MessageView;
use risunest_ir::budget::{Budget, OwnedVec};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use std::borrow::Cow;

/// One conversation message as the lorebook scans it.
#[derive(Debug)]
pub struct LoreMessage {
    /// `role === 'user'`.
    pub user: bool,
    /// `data`; `None` when it is not a string, which TS fails on once the message is scanned.
    pub data: Option<Text>,
    /// The speaker in a non-user message's prompt: `name ?? <saying character>.name ?? character name`.
    pub name: Text,
}

/// `getSliceStart(length, searchDepth)`: the index `slice(length - searchDepth)` starts at.
pub fn slice_start(length: usize, depth: f64) -> usize {
    let length_f = length as f64;
    let requested = length_f - depth;
    if requested.is_nan() { return 0; }
    let integer = if requested < 0.0 { requested.ceil() } else { requested.floor() };
    if integer < 0.0 { return (length_f + integer).max(0.0) as usize; }
    integer.min(length_f) as usize
}

/// `getBaseMessageViews`: views of `messages[start..]`, numbered from the slice start.
pub fn message_views(messages: &[LoreMessage], start: usize, username: &Text, tailored: bool, budget: &Budget)
    -> Result<OwnedVec<MessageView>, StageError> {
    message_views_with_tables(messages, start, username, tailored, crate::matching::CaseTables::Native, budget)
}

pub(crate) fn message_views_with_tables(messages: &[LoreMessage], start: usize, username: &Text, tailored: bool,
    tables: crate::matching::CaseTables, budget: &Budget) -> Result<OwnedVec<MessageView>, StageError> {
    let mut views = OwnedVec::new(budget)?;
    for (index, message) in messages[start.min(messages.len())..].iter().enumerate() {
        budget.work(1)?;
        let Some(data) = &message.data else { return Err(StageError::Invalid(Cow::Borrowed("message-data"))) };
        let (source, speaker) = if message.user { ("user", username) } else { ("char", &message.name) };
        let source = Text::from_str(&format!("message {index} by {source}"), budget)?;
        let mut prompt = Text::from_str("\u{1}{{", budget)?;
        prompt.append(speaker)?;
        prompt.append(&Text::from_str("}}:", budget)?)?;
        prompt.append(data)?;
        prompt.append(&Text::from_str("\u{1}", budget)?)?;
        views.push(MessageView::with_case_tables(source, prompt, data.copy(budget)?, tailored, tables, budget)?)?;
    }
    Ok(views)
}

#[cfg(test)]
mod tests {
    use super::slice_start;

    #[test]
    fn the_slice_start_follows_array_slice_of_length_minus_depth() {
        assert_eq!(slice_start(10, 3.0), 7);
        assert_eq!(slice_start(10, 30.0), 0);
        assert_eq!(slice_start(10, 0.0), 10);
        assert_eq!(slice_start(10, -2.0), 10);
        assert_eq!(slice_start(10, 2.5), 7);
        assert_eq!(slice_start(10, 10.5), 0);
        assert_eq!(slice_start(10, f64::NAN), 0);
        assert_eq!(slice_start(10, f64::INFINITY), 0);
        assert_eq!(slice_start(10, f64::NEG_INFINITY), 10);
    }
}
