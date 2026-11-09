//! Text operations of HypaV3: summary chunk splitting (`splitBySeparator`, `hypav3.ts:121-132`), inlay sanitizing
//! (`sanitizeSummaryContent`) and the auxiliary-model summary cleanup in `summarize` (`:1728-1789`).
use crate::hypa_v2::SummaryOutput;
use risunest_ir::stage::StageError;
use risunest_ir::text::is_whitespace;
use risunest_ir::{Budget, OwnedVec, Text, TextBuilder};

/// What `text.split(separatorRegExp)` splits on, for the separators this port evaluates.
pub(crate) enum Separator {
    /// The empty pattern, which splits between every UTF-16 unit.
    Units,
    Literal(OwnedVec<u16>),
}

fn units(text: &Text, budget: &Budget) -> Result<OwnedVec<u16>, StageError> {
    budget.work(text.len_units())?;
    let mut units = OwnedVec::new(budget)?;
    for (index, unit) in text.units().enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        units.push(unit)?;
    }
    Ok(units)
}

fn literal(value: &[u8], budget: &Budget) -> Result<Separator, StageError> {
    let mut units = OwnedVec::new(budget)?;
    for &unit in value { units.push(u16::from(unit))?; }
    Ok(Separator::Literal(units))
}

fn line_terminator(unit: u16) -> bool { matches!(unit, 10 | 13 | 0x2028 | 0x2029) }

/// Parses the configured separator. `/pattern/flags` (`/^\/(.+)\/([gimuy]*)$/`) and plain patterns are evaluated
/// when they denote a fixed string; a pattern `new RegExp` rejects falls back to `"\n\n"` as TS does. `None` is any
/// other pattern, or the `i` or `u` flag, which this port does not evaluate.
pub(crate) fn separator(value: &Text, budget: &Budget) -> Result<Option<Separator>, StageError> {
    let all = units(value, budget)?;
    let all = all.as_slice();
    let mut pattern = all;
    let mut flags: &[u16] = &[];
    if all.first() == Some(&47) && !all.iter().any(|&unit| line_terminator(unit)) {
        let last = all.iter().rposition(|&unit| unit == 47).expect("leading slash");
        if last >= 2 && all[last + 1..].iter().all(|&unit| b"gimuy".iter().any(|&flag| u16::from(flag) == unit)) {
            pattern = &all[1..last];
            flags = &all[last + 1..];
        }
    }
    let mut seen = 0u8;
    for &flag in flags {
        let bit = 1u8 << b"gimuy".iter().position(|&known| u16::from(known) == flag).expect("flag");
        // A repeated flag makes `new RegExp` throw.
        if seen & bit != 0 { return Ok(Some(literal(b"\n\n", budget)?)); }
        seen |= bit;
    }
    // `g`, `m` and `y` do not change how a fixed-string pattern splits; `i` and `u` do.
    if flags.iter().any(|&flag| flag == u16::from(b'i') || flag == u16::from(b'u')) { return Ok(None); }
    let mut result = OwnedVec::new(budget)?;
    let mut at = 0;
    while at < pattern.len() {
        budget.work(1)?;
        let unit = pattern[at];
        if unit == u16::from(b'\\') {
            // `\` at the end of a pattern is a syntax error.
            let Some(&next) = pattern.get(at + 1) else { return Ok(Some(literal(b"\n\n", budget)?)) };
            let escaped = match u8::try_from(next) {
                Ok(b'n') => 10,
                Ok(b'r') => 13,
                Ok(b't') => 9,
                Ok(b'f') => 12,
                Ok(b'v') => 11,
                // `\0` before a digit is a legacy octal escape.
                Ok(b'0') if !pattern.get(at + 2).is_some_and(|unit| (48..=57).contains(unit)) => 0,
                Ok(byte) if byte.is_ascii_alphanumeric() => return Ok(None),
                _ => next,
            };
            result.push(escaped)?;
            at += 2;
            continue;
        }
        if b"^$.*+?()[]{}|".iter().any(|&meta| u16::from(meta) == unit) { return Ok(None); }
        result.push(unit)?;
        at += 1;
    }
    Ok(Some(if result.as_slice().is_empty() { Separator::Units } else { Separator::Literal(result) }))
}

/// `text.split(separator)` per `RegExp.prototype[@@split]`.
pub(crate) fn split(text: &Text, separator: &Separator, budget: &Budget) -> Result<OwnedVec<Text>, StageError> {
    let all = units(text, budget)?;
    let all = all.as_slice();
    let mut pieces = OwnedVec::new(budget)?;
    match separator {
        Separator::Units => {
            // An empty string matches the empty pattern and yields no piece.
            for &unit in all {
                budget.work(1)?;
                pieces.push(Text::from_units(&[unit], budget)?)?;
            }
        }
        Separator::Literal(pattern) => {
            let pattern = pattern.as_slice();
            let (mut start, mut at) = (0, 0);
            while at < all.len() {
                budget.work(1)?;
                if at & 4095 == 0 { budget.check()?; }
                if all.len() - at >= pattern.len() && &all[at..at + pattern.len()] == pattern {
                    budget.work(pattern.len())?;
                    pieces.push(Text::from_units(&all[start..at], budget)?)?;
                    at += pattern.len();
                    start = at;
                } else { at += 1; }
            }
            pieces.push(Text::from_units(&all[start..], budget)?)?;
        }
    }
    Ok(pieces)
}

pub(crate) fn whitespace_only(content: &Text, budget: &Budget) -> Result<bool, StageError> {
    budget.work(content.len_units())?;
    for (index, unit) in content.units().enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        if !is_whitespace(unit) { return Ok(false); }
    }
    Ok(true)
}

fn starts_with(units: &[u16], at: usize, prefix: &[u8]) -> bool {
    units.len() - at.min(units.len()) >= prefix.len() && units[at..at + prefix.len()].iter().zip(prefix).all(|(&unit, &byte)| unit == u16::from(byte))
}

/// `content.replace(/{{(inlay|inlayed|inlayeddata)::(.+?)}}/g, "[Image]")`.
pub(crate) fn sanitize(content: &Text, budget: &Budget) -> Result<Text, StageError> {
    let all = units(content, budget)?;
    let all = all.as_slice();
    let mut result = TextBuilder::new(budget)?;
    let (mut copied, mut at) = (0, 0);
    while at < all.len() {
        budget.work(1)?;
        if at & 4095 == 0 { budget.check()?; }
        let mut end = None;
        if starts_with(all, at, b"{{") {
            // The three names are mutually exclusive once `::` must follow.
            for name in [b"inlay::".as_slice(), b"inlayed::", b"inlayeddata::"] {
                if !starts_with(all, at + 2, name) { continue; }
                // `.+?`: at least one unit that is no line terminator, then the first `}}`.
                let mut close = at + 2 + name.len();
                let first = close;
                while close < all.len() && !line_terminator(all[close]) {
                    budget.work(1)?;
                    close += 1;
                    if close > first && starts_with(all, close, b"}}") { end = Some(close + 2); break; }
                }
                break;
            }
        }
        match end {
            Some(end) => {
                for &unit in &all[copied..at] { result.unit(unit)?; }
                for &byte in b"[Image]" { result.unit(u16::from(byte))?; }
                copied = end;
                at = end;
            }
            None => at += 1,
        }
    }
    for &unit in &all[copied..] { result.unit(unit)?; }
    Ok(result.finish()?)
}

/// `response.result.replace(/<Thoughts>[\s\S]*?<\/Thoughts>/g, "")`.
fn strip_thoughts(result: &Text, budget: &Budget) -> Result<Text, StageError> {
    let all = units(result, budget)?;
    let all = all.as_slice();
    let mut output = TextBuilder::new(budget)?;
    let (mut copied, mut at) = (0, 0);
    while at < all.len() {
        budget.work(1)?;
        if at & 4095 == 0 { budget.check()?; }
        if starts_with(all, at, b"<Thoughts>") {
            let mut close = at + 10;
            while close < all.len() && !starts_with(all, close, b"</Thoughts>") { budget.work(1)?; close += 1; }
            // Without a closing tag no later opening can match either.
            if close >= all.len() { break; }
            for &unit in &all[copied..at] { output.unit(unit)?; }
            copied = close + 11;
            at = copied;
        } else { at += 1; }
    }
    for &unit in &all[copied..] { output.unit(unit)?; }
    Ok(output.finish()?)
}

fn failed(message: &str, budget: &Budget) -> Result<SummaryOutput, StageError> {
    Ok(SummaryOutput::Failed(Text::from_str(message, budget)?))
}

/// What HypaV3's `summarize` makes of a successful auxiliary-model reply (`:1757-1789`): the summary, or the
/// `String(error)` text of the error it throws. A `fail` reply is `Failed("Error: " + result)`.
pub fn sub_model_summary(result: &Text, budget: &Budget) -> Result<SummaryOutput, StageError> {
    if whitespace_only(result, budget)? { return failed("Error: Empty summary returned", budget); }
    let summary = strip_thoughts(result, budget)?.trim(budget)?;
    if summary.len_units() == 0 { return failed("Error: Empty summary after removing thoughts content", budget); }
    Ok(SummaryOutput::Success(summary))
}
