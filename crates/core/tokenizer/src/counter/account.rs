//! The counts `getCharToken`, `getChatToken` and `tokenizeAccurate` (`src/ts/tokenizer.ts`) build from `tokenize`.

use super::Counter;
use risunest_ir::stage::{HostReply, StageError, Step, TextContext, TextProcessor, TokenCounter, TokenInput};
use risunest_ir::{Budget, Text};
use std::borrow::Cow;
use std::collections::VecDeque;

/// The character fields `getCharToken` reads.
#[derive(Debug, Clone, Copy)]
pub struct CharacterFields<'a> {
    pub name: &'a Text,
    pub desc: &'a Text,
    /// `personality`; `None` counts as the empty text.
    pub personality: Option<&'a Text>,
    /// `scenario`; `None` counts as the empty text.
    pub scenario: Option<&'a Text>,
    /// The `content` of each `globalLore` entry.
    pub lore: &'a [Text],
}

/// The [`TextTotals`] bucket `getCharToken` adds a text to: `persistant` or `dynamic`.
pub const PERSISTANT: usize = 0;
pub const DYNAMIC: usize = 1;

fn units(text: &Text) -> Vec<u16> { text.units().collect() }

fn find(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    haystack.get(from..)?.windows(needle.len()).position(|window| window == needle).map(|position| position + from)
}

// `source.replace(/pattern/g, replacement)` for a pattern without groups: `$$`, `$&`, `` $` `` and `$'` in the
// replacement expand as `GetSubstitution` expands them, and every other `$` stays literal.
fn replace_all(source: &[u16], pattern: &[u16], replacement: &[u16], budget: &Budget) -> Result<Vec<u16>, StageError> {
    let mut out = Vec::new();
    let mut last = 0;
    while let Some(at) = find(source, pattern, last) {
        budget.work(at - last + replacement.len())?;
        out.extend_from_slice(&source[last..at]);
        let end = at + pattern.len();
        let mut index = 0;
        while index < replacement.len() {
            let unit = replacement[index];
            let next = replacement.get(index + 1).copied();
            let expansion: Option<&[u16]> = match (unit, next) {
                (0x24, Some(0x24)) => Some(&replacement[index..index + 1]),
                (0x24, Some(0x26)) => Some(&source[at..end]),
                (0x24, Some(0x60)) => Some(&source[..at]),
                (0x24, Some(0x27)) => Some(&source[end..]),
                _ => None,
            };
            match expansion {
                Some(expansion) => {
                    budget.work(expansion.len())?;
                    out.extend_from_slice(expansion);
                    index += 2;
                }
                None => { out.push(unit); index += 1; }
            }
        }
        last = end;
    }
    out.extend_from_slice(&source[last..]);
    Ok(out)
}

// `basicTokenize`'s text: `{{char}}`, then `<char>`, replaced by the name.
fn with_name(text: &Text, name: &[u16], budget: &Budget) -> Result<Text, StageError> {
    budget.work(text.len_units())?;
    let first = replace_all(&units(text), &"{{char}}".encode_utf16().collect::<Vec<_>>(), name, budget)?;
    let second = replace_all(&first, &"<char>".encode_utf16().collect::<Vec<_>>(), name, budget)?;
    Ok(Text::from_units(&second, budget)?)
}

// A lorebook entry's content without its `@@` decorator lines and empty lines, joined by line feeds again.
fn lore_body(content: &Text) -> Vec<u16> {
    let mut out = Vec::new();
    let mut first = true;
    for line in units(content).split(|unit| *unit == 0x0a) {
        if line.is_empty() || line.starts_with(&[0x40, 0x40]) { continue; }
        if !first { out.push(0x0a); }
        first = false;
        out.extend_from_slice(line);
    }
    out
}

/// The texts `getCharToken` tokenizes, in TS order, each with its bucket.
pub fn character_texts(fields: &CharacterFields<'_>, budget: &Budget) -> Result<Vec<(Text, usize)>, StageError> {
    let name = units(fields.name);
    let empty = Text::from_str("", budget)?;
    let mut texts = Vec::new();
    for text in [fields.desc, fields.personality.unwrap_or(&empty), fields.scenario.unwrap_or(&empty)] {
        texts.push((with_name(text, &name, budget)?, PERSISTANT));
    }
    for content in fields.lore {
        budget.work(content.len_units())?;
        let body = Text::from_units(&lore_body(content), budget)?;
        texts.push((with_name(&body, &name, budget)?, DYNAMIC));
    }
    Ok(texts)
}

/// The texts `getChatToken` tokenizes: each message's `data`, every one in bucket 0. `ChatTokenizer(0, 'name')`
/// adds nothing to them, since the chats it builds carry neither a name nor multimodals.
pub fn chat_texts(messages: &[Text], budget: &Budget) -> Result<Vec<(Text, usize)>, StageError> {
    messages.iter().map(|message| Ok((message.copy(budget)?, 0))).collect()
}

/// The text `tokenizeAccurate(data, consistantChar)` counts: `data` without its first `{{slot}}`, through
/// `risuChatParser` with `tokenizeAccurate: true`. The processor carries those options; a `cbs-experimental`
/// processor takes them in its `Snapshot` (`tokenize_accurate`, `consistant_char`).
pub fn accurate_text(data: &Text, processor: &mut dyn TextProcessor, budget: &Budget) -> Result<Text, StageError> {
    budget.work(data.len_units())?;
    let source = units(data);
    let slot: Vec<u16> = "{{slot}}".encode_utf16().collect();
    let stripped = match find(&source, &slot, 0) {
        Some(at) => Text::from_units(&[&source[..at], &source[at + slot.len()..]].concat(), budget)?,
        None => data.copy(budget)?,
    };
    Ok(processor.process(&stripped, &TextContext::default(), budget)?.text)
}

/// Sums `tokenize(text)` into buckets, one text at a time, passing plugin tokenizer requests through.
pub struct TextTotals {
    counter: Counter,
    texts: VecDeque<(Text, usize)>,
    totals: Vec<f64>,
    waiting: Option<usize>,
}

impl TextTotals {
    pub fn new(counter: Counter) -> Self { Self { counter, texts: VecDeque::new(), totals: Vec::new(), waiting: None } }
    pub fn into_counter(self) -> Counter { self.counter }

    pub fn start(&mut self, texts: Vec<(Text, usize)>, buckets: usize, budget: &Budget) -> Result<Step<Vec<u32>>, StageError> {
        if self.waiting.is_some() || !self.texts.is_empty() { return Err(StageError::Invalid(Cow::Borrowed("text-totals-busy"))); }
        if texts.iter().any(|(_, bucket)| *bucket >= buckets) { return Err(StageError::Invalid(Cow::Borrowed("text-totals-bucket"))); }
        self.texts = texts.into();
        self.totals = vec![0.0; buckets];
        let step = self.advance(budget);
        self.settle(step)
    }

    pub fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<Vec<u32>>, StageError> {
        let Some(bucket) = self.waiting.take() else { return Err(StageError::Invalid(Cow::Borrowed("text-totals-resume"))) };
        let step = match self.counter.resume(reply, budget) {
            Ok(Step::Done(count)) => { self.totals[bucket] += count as f64; self.advance(budget) }
            Ok(Step::NeedsHost(request)) => { self.waiting = Some(bucket); Ok(Step::NeedsHost(request)) }
            Err(error) => Err(error),
        };
        self.settle(step)
    }

    fn settle(&mut self, step: Result<Step<Vec<u32>>, StageError>) -> Result<Step<Vec<u32>>, StageError> {
        if !matches!(step, Ok(Step::NeedsHost(_))) { self.texts.clear(); self.waiting = None; }
        step
    }

    fn advance(&mut self, budget: &Budget) -> Result<Step<Vec<u32>>, StageError> {
        while let Some((text, bucket)) = self.texts.pop_front() {
            match self.counter.count(TokenInput::Text(&text), budget)? {
                Step::Done(count) => self.totals[bucket] += count as f64,
                Step::NeedsHost(request) => { self.waiting = Some(bucket); return Ok(Step::NeedsHost(request)); }
            }
        }
        self.totals.iter().map(|total| {
            if *total <= u32::MAX as f64 { Ok(*total as u32) } else { Err(StageError::Invalid(Cow::Borrowed("token-count"))) }
        }).collect::<Result<Vec<_>, _>>().map(Step::Done)
    }
}
