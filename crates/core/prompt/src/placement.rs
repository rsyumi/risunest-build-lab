//! Lore fragments at the parse and insertion sites of prompt preparation.

use crate::items::closing;
use crate::machine::{js, message, Ctx, Prompt};
use risunest_ir::chat::{PromptMessage, Role};
use risunest_ir::stage::{InjectOperation, LoreFragment, StageError};
use risunest_ir::Budget;

pub(crate) struct Placement { fragments: Vec<LoreFragment> }

impl Placement {
    pub(crate) fn new(fragments: Vec<LoreFragment>) -> Self { Self { fragments } }

    // Replacements returned by the callback are literal, including their dollar signs.
    fn positions(&self, text: &[u16], remove: bool, budget: &Budget) -> Result<(js::S, bool), StageError> {
        budget.work(text.len())?;
        let prefix = js::s("{{position::");
        let mut output = Vec::with_capacity(text.len());
        let (mut last, mut from, mut replaced) = (0, 0, false);
        while let Some(at) = js::find(text, &prefix, from) {
            match closing(text, at + prefix.len(), false) {
                Some(end) => {
                    output.extend_from_slice(&text[last..at]);
                    if !remove {
                        let position = js::concat(&[&js::s("pt_"), &text[at + prefix.len()..end]]);
                        let mut first = true;
                        for fragment in &self.fragments {
                            budget.work(1)?;
                            if fragment.position.units().eq(position.iter().copied()) {
                                if !first { output.push(0x0a); }
                                output.extend(js::units(&fragment.prompt, budget)?);
                                first = false;
                            }
                        }
                    }
                    last = end + 2;
                    from = last;
                    replaced = true;
                }
                None => from = at + 1,
            }
        }
        output.extend_from_slice(&text[last..]);
        Ok((output, replaced))
    }

    pub(crate) fn resolve(&self, text: &[u16], budget: &Budget) -> Result<js::S, StageError> {
        let mut output = text.to_vec();
        for _ in 0..5 {
            let (next, replaced) = self.positions(&output, false, budget)?;
            output = next;
            if !replaced { break; }
        }
        Ok(self.positions(&output, true, budget)?.0)
    }

    pub(crate) fn position(&self, text: &[u16], location: Option<&[u16]>, budget: &Budget) -> Result<js::S, StageError> {
        let mut output = text.to_vec();
        for fragment in &self.fragments {
            budget.work(1)?;
            let Some(inject) = &fragment.inject else { continue };
            if inject.lore || !location.is_some_and(|location| inject.location.units().eq(location.iter().copied())) { continue; }
            let content = js::units(&fragment.prompt, budget)?;
            output = match inject.operation {
                InjectOperation::Append => js::concat(&[&output, &[0x20], &content]),
                InjectOperation::Prepend => js::concat(&[&content, &[0x20], &output]),
                InjectOperation::Replace => js::replace(&output, &js::units(&inject.param, budget)?, &content, false, budget)?,
            };
        }
        self.resolve(&output, budget)
    }

    fn parse(&self, ctx: &mut Ctx, fragment: &LoreFragment) -> Result<PromptMessage, StageError> {
        let budget = ctx.budget();
        let content = self.resolve(&js::units(&fragment.prompt, &budget)?, &budget)?;
        let content = ctx.parse_plain(&content)?;
        message(fragment.role, &content, &budget)
    }

    pub(crate) fn groups(&self, ctx: &mut Ctx, prompt: &mut Prompt) -> Result<(), StageError> {
        ctx.budget().work(self.fragments.len().saturating_mul(2))?;
        for fragment in &self.fragments {
            if fragment.position.len_units() == 0 && fragment.inject.is_none() {
                let index = prompt.add(self.parse(ctx, fragment)?);
                prompt.groups.lorebook.push(index);
            }
        }
        for fragment in &self.fragments {
            let before = fragment.position.equals_ascii(b"before_desc");
            if before || [b"after_desc".as_slice(), b"personality", b"scenario"].iter().any(|pos| fragment.position.equals_ascii(pos)) {
                let index = prompt.add(self.parse(ctx, fragment)?);
                if before { prompt.groups.description.insert(0, index); } else { prompt.groups.description.push(index); }
            }
        }
        Ok(())
    }

    pub(crate) fn depth_zero(&self, ctx: &mut Ctx, prompt: &mut Prompt) -> Result<(), StageError> {
        ctx.budget().work(self.fragments.len().saturating_mul(2))?;
        for assistant in [false, true] {
            for fragment in &self.fragments {
                if fragment.position.equals_ascii(b"depth") && fragment.depth == 0.0 && (fragment.role == Role::Assistant) == assistant {
                    let index = prompt.add(self.parse(ctx, fragment)?);
                    prompt.groups.post_everything.push(index);
                }
            }
        }
        Ok(())
    }

    fn depths(&self) -> impl Iterator<Item = &LoreFragment> {
        self.fragments.iter().filter(|fragment| fragment.position.equals_ascii(b"reverse_depth")
            || (fragment.position.equals_ascii(b"depth") && fragment.depth > 0.0))
    }

    pub(crate) async fn count_depth(&self, ctx: &mut Ctx) -> Result<f64, StageError> {
        ctx.budget().work(self.fragments.len())?;
        let mut tokens = 0.0;
        for fragment in self.depths() {
            let chat = self.parse(ctx, fragment)?;
            tokens += ctx.count(&chat).await?;
        }
        Ok(tokens)
    }

    pub(crate) fn insert_depth(&self, ctx: &mut Ctx, prompt: &mut Prompt) -> Result<(), StageError> {
        ctx.budget().work(self.fragments.len())?;
        for fragment in self.depths() {
            let chat = self.parse(ctx, fragment)?;
            let length = prompt.groups.chats.len() as f64;
            let start = if fragment.position.equals_ascii(b"depth") { fragment.depth } else { length - fragment.depth };
            let start = if start.is_nan() { 0.0 } else { start.trunc() };
            let at = if start < 0.0 { (length + start).max(0.0) } else { start.min(length) } as usize;
            let index = prompt.add(chat);
            prompt.groups.chats.insert(at, index);
        }
        Ok(())
    }
}
