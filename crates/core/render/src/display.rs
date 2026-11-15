//! The string glue of `renderMarkdown` (`src/ts/parser/parser.svelte.ts`) around `md.render`: the KaTeX `$$` spans,
//! the curly quotes made straight before rendering, `risuUnescape`, and the private-use quote placeholders written the
//! way `MarkdownRenderSettings` asks.

use crate::error::{RenderError, Result};
use crate::meter::Meter;
use crate::utils::{js_trim, s, units_of, Units};
use crate::{markdown_units, Engine, JsEngine};
use risunest_ir::{Budget, Text};

const QUOTE_UNITS: [u16; 4] = [0xe9b0, 0xe9b1, 0xe9b2, 0xe9b3];
/// `risuUnescape`'s replacements for U+E9B8 to U+E9BF.
const UNESCAPED: [&str; 8] = ["{", "}", "(", ")", "&lt;", "&gt;", ":", ";"];

/// How `renderMarkdown` writes the quote placeholders markdown-it's smartquotes leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteMode {
    /// `unformatQuotes`: the quotes alone.
    Unformat,
    /// `blockquoteStyling`: a quoted run on one line becomes a `<mark risu-mark="blockquote1|2">` set off by `<br><br>`.
    Blockquote,
    /// Neither: each quote pair in a `<mark risu-mark="quote1|quote2">`.
    Mark,
}

/// The quotes and mode `renderMarkdown` reads from `MarkdownRenderSettings`.
#[derive(Debug)]
pub struct QuoteSettings {
    /// Opening and closing double quote, then opening and closing single quote.
    pub quotes: [Text; 4],
    pub mode: QuoteMode,
}

impl QuoteSettings {
    /// `renderMarkdown`'s reading: `customQuotesData` (the curly quotes when it is absent) under `customQuotes`, and
    /// `unformatQuotes` ahead of `blockquoteStyling`.
    pub fn from_settings(custom_quotes: bool, custom_quotes_data: Option<[Text; 4]>, unformat_quotes: bool, blockquote_styling: bool,
        budget: &Budget) -> Result<Self> {
        let quotes = match custom_quotes_data.filter(|_| custom_quotes) {
            Some(quotes) => quotes,
            None => [Text::from_str("\u{201c}", budget)?, Text::from_str("\u{201d}", budget)?, Text::from_str("\u{2018}", budget)?,
                Text::from_str("\u{2019}", budget)?],
        };
        let mode = if unformat_quotes { QuoteMode::Unformat } else if blockquote_styling { QuoteMode::Blockquote } else { QuoteMode::Mark };
        Ok(Self { quotes, mode })
    }
}

/// The `$$…$$` spans of `source`, as `[start, end)` of the whole match: `/\$\$(.*?)\$\$/gs`.
fn math_spans(src: &[u16]) -> Vec<(usize, usize)> {
    let pair = |from: usize| (from..src.len().saturating_sub(1)).find(|&index| src[index] == 0x24 && src[index + 1] == 0x24);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(start) = pair(pos) {
        let Some(close) = pair(start + 2) else { break };
        spans.push((start, close + 2));
        pos = close + 2;
    }
    spans
}

/// The TeX of each `$$…$$` span `renderMarkdown` hands to KaTeX, in source order, with U+E9B8 to U+E9BB restored to
/// `{`, `}`, `(` and `)` as it does before calling `katex.renderToString`.
pub fn find_math(source: &Text, budget: &Budget) -> Result<Vec<Text>> {
    let meter = Meter::new(budget);
    let src: Units = units_of(&source);
    meter.alloc(2 * src.len())?;
    meter.work(src.len())?;
    let mut found = Vec::new();
    for (start, end) in math_spans(&src) {
        let tex: Units = src[start + 2..end - 2].iter().map(|&unit| match unit {
            0xe9b8 => 0x7b,
            0xe9b9 => 0x7d,
            0xe9ba => 0x28,
            0xe9bb => 0x29,
            unit => unit,
        }).collect();
        found.push(Text::from_units(&tex, budget)?);
    }
    meter.finish()?;
    Ok(found)
}

/// Whether the four quote passes can run as one: each writes a nonempty literal holding no quote unit, so no pass sees
/// another's output and the text only grows.
fn literal_quotes(last: &[Units; 4]) -> bool {
    last.iter().all(|replacement| !replacement.is_empty() && !replacement.iter().any(|unit| *unit == 0x24 || QUOTE_UNITS.contains(unit)))
}

/// `risuUnescape` when `unescape`, then the quote passes as one, writing `last[unit - U+E9B0]` for each quote unit. A
/// `last` of empty replacements leaves the quote units, which every unit this rewrites shares the top 12 bits with.
fn replace_quotes(src: &[u16], last: &[Units; 4], unescape: bool, meter: &Meter) -> Result<Units> {
    meter.work(src.len())?;
    let unescaped: [Units; 8] = UNESCAPED.map(s);
    let mut out = Units::with_capacity(src.len());
    let mut copied = 0;
    let mut offset = 0;
    for chunk in src.chunks(16) {
        if chunk.iter().fold(false, |found, unit| found | (*unit & 0xfff0 == 0xe9b0)) {
            for (index, &unit) in chunk.iter().enumerate() {
                let replacement: &[u16] = match unit {
                    0xe9b0..=0xe9b3 if !last[(unit - 0xe9b0) as usize].is_empty() => &last[(unit - 0xe9b0) as usize],
                    0xe9b8..=0xe9bf if unescape => &unescaped[(unit - 0xe9b8) as usize],
                    _ => continue,
                };
                out.extend_from_slice(&src[copied..offset + index]);
                out.extend_from_slice(replacement);
                copied = offset + index + 1;
            }
        }
        offset += chunk.len();
    }
    out.extend_from_slice(&src[copied..]);
    meter.alloc(2 * out.len())?;
    meter.text(out.len())?;
    Ok(out)
}

/// `renderMarkdown(md, source, settings)` for the `md` or `mdHighlight` instance.
///
/// `math` holds, for each span [`find_math`] returns, what `katex.renderToString(tex, { displayMode: false,
/// throwOnError: true, output: 'mathml' })` returned, or `None` where it threw and the span stays as written. A source
/// whose span count differs from `math` declines as `markdown-katex`, since KaTeX runs outside this crate.
pub fn render_display_markdown(source: &Text, engine: Engine, settings: &QuoteSettings, math: &[Option<Text>], js: &JsEngine,
    budget: &Budget) -> Result<Text> {
    let meter = Meter::new(budget);
    let src: Units = units_of(&source);
    meter.alloc(4 * src.len())?;
    meter.work(src.len())?;
    let spans = math_spans(&src);
    if spans.len() != math.len() { return Err(RenderError::Unsupported("markdown-katex")); }
    let mut data = Units::with_capacity(src.len());
    let mut pos = 0;
    for ((start, end), rendered) in spans.iter().zip(math) {
        data.extend_from_slice(&src[pos..*start]);
        match rendered {
            Some(rendered) => {
                meter.alloc(2 * rendered.len_units())?;
                data.extend(rendered.units());
            }
            None => data.extend_from_slice(&src[*start..*end]),
        }
        pos = *end;
    }
    data.extend_from_slice(&src[pos..]);
    drop(src);
    for unit in data.iter_mut() {
        match *unit {
            0x201c | 0x201d => *unit = 0x22,
            0x2018 | 0x2019 => *unit = 0x27,
            _ => {}
        }
    }
    meter.text(data.len())?;
    // The tokens' charges end with the Markdown render, as the tokens do; only the HTML stays charged.
    let rendered = {
        let markdown = Meter::new(budget);
        let rendered = markdown_units(&data, engine, js, &markdown)?;
        markdown.finish()?;
        rendered
    };
    meter.alloc(2 * rendered.len())?;
    drop(data);
    let quotes: Vec<Units> = settings.quotes.iter().map(|quote| quote.units().collect()).collect();
    let [open2, close2, open1, close1] = QUOTE_UNITS;
    let joined = |left: &str, quote: &[u16], right: &str| [&s(left)[..], quote, &s(right)[..]].concat();
    let replaced = |text: Units, unit: u16, replacement: &[u16]| replace_unit(&text, unit, replacement, &meter);
    // What the last four passes write for `QUOTE_UNITS`, in their order.
    let last: [Units; 4] = match settings.mode {
        QuoteMode::Unformat | QuoteMode::Blockquote => [0, 1, 2, 3].map(|index| quotes[index].clone()),
        QuoteMode::Mark => [joined("<mark risu-mark=\"quote2\">", &quotes[0], ""), joined("", &quotes[1], "</mark>"),
            joined("<mark risu-mark=\"quote1\">", &quotes[2], ""), joined("", &quotes[3], "</mark>")],
    };
    let literal = literal_quotes(&last);
    if literal && settings.mode != QuoteMode::Blockquote {
        let text = replace_quotes(&rendered, &last, true, &meter)?;
        drop(rendered);
        meter.finish()?;
        return Ok(Text::from_units(&text, budget)?);
    }
    let mut text = replace_quotes(&rendered, &[Units::new(), Units::new(), Units::new(), Units::new()], true, &meter)?;
    drop(rendered);
    match settings.mode {
        QuoteMode::Unformat => {
            for (unit, quote) in QUOTE_UNITS.iter().zip(&quotes) { text = replaced(text, *unit, quote)?; }
        }
        QuoteMode::Blockquote => {
            let mark1 = joined("<mark risu-mark=\"quote1\">", &quotes[2], "");
            let end1 = joined("", &quotes[3], "</mark>");
            text = replace_pairs(&text, open2, close2, &meter, |content, out| {
                let content = replace_unit(&replace_unit(content, open1, &mark1, &meter)?, close1, &end1, &meter)?;
                out.extend(s("<br><br><mark risu-mark=\"blockquote2\">"));
                out.extend_from_slice(&quotes[0]);
                out.extend_from_slice(&content);
                out.extend_from_slice(&quotes[1]);
                out.extend(s("</mark><br><br>"));
                Ok(())
            })?;
            text = replace_pairs(&text, open1, close1, &meter, |content, out| {
                out.extend(s("<br><br><mark risu-mark=\"blockquote1\">"));
                out.extend_from_slice(&quotes[2]);
                out.extend_from_slice(content);
                out.extend_from_slice(&quotes[3]);
                out.extend(s("</mark><br><br>"));
                Ok(())
            })?;
            if literal {
                text = replace_quotes(&text, &last, false, &meter)?;
            } else {
                for (unit, quote) in QUOTE_UNITS.iter().zip(&quotes) { text = replaced(text, *unit, quote)?; }
            }
        }
        QuoteMode::Mark => {
            for (unit, replacement) in QUOTE_UNITS.iter().zip(&last) { text = replaced(text, *unit, replacement)?; }
        }
    }
    meter.text(text.len())?;
    meter.finish()?;
    Ok(Text::from_units(&text, budget)?)
}

/// The line pass of `postTranslationParse(data, settings)` before its `renderHighlightableMarkdown`: each line of
/// `data.split('\n')` that starts with `<` once trimmed stays trimmed.
pub fn post_translation_lines(data: &Text, budget: &Budget) -> Result<Text> {
    let meter = Meter::new(budget);
    let src: Units = units_of(&data);
    meter.alloc(4 * src.len())?;
    meter.work(src.len())?;
    let mut out = Units::with_capacity(src.len());
    for (index, line) in src.split(|unit| *unit == 0x0a).enumerate() {
        if index > 0 { out.push(0x0a); }
        let trimmed = js_trim(line);
        out.extend_from_slice(if trimmed.first() == Some(&0x3c) { trimmed } else { line });
    }
    meter.finish()?;
    Ok(Text::from_units(&out, budget)?)
}

/// GetSubstitution for a pattern without captures: what a string `replacement` inserts for the match `[start, end)`
/// of `text`. `$$`, `$&`, `` $` `` and `$'` expand; any other `$` stays as written.
pub(crate) fn substitute(replacement: &[u16], text: &[u16], start: usize, end: usize, out: &mut Units) {
    let mut index = 0;
    while index < replacement.len() {
        let unit = replacement[index];
        if unit == 0x24 {
            match replacement.get(index + 1) {
                Some(0x24) => { out.push(0x24); index += 2; continue; }
                Some(0x26) => { out.extend_from_slice(&text[start..end]); index += 2; continue; }
                Some(0x60) => { out.extend_from_slice(&text[..start]); index += 2; continue; }
                Some(0x27) => { out.extend_from_slice(&text[end..]); index += 2; continue; }
                _ => {}
            }
        }
        out.push(unit);
        index += 1;
    }
}

/// `text.replace(/<unit>/gu, replacement)` with a string replacement, for a BMP `unit` that is not a surrogate.
pub(crate) fn replace_unit(text: &[u16], unit: u16, replacement: &[u16], meter: &Meter) -> Result<Units> {
    meter.work(text.len())?;
    let mut out = Units::with_capacity(text.len());
    for (index, &value) in text.iter().enumerate() {
        if value != unit {
            out.push(value);
            continue;
        }
        let before = out.len();
        substitute(replacement, text, index, index + 1, &mut out);
        meter.alloc(2 * (out.len() - before))?;
        meter.text(out.len())?;
    }
    Ok(out)
}

fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

/// Units in the code point at `index`.
pub(crate) fn code_point_len(text: &[u16], index: usize) -> usize {
    let pair = (0xd800..0xdc00).contains(&text[index]) && text.get(index + 1).is_some_and(|next| (0xdc00..0xe000).contains(next));
    if pair { 2 } else { 1 }
}

/// `text.replace(/<open>(.+?)<close>/gum, (full, content) => ...)`, with `write` appending the replacement for
/// `content`. An `open` without its `close` on the line ends the search on that line, since a later `open` there sees
/// less of the line, so each unit is scanned once.
fn replace_pairs(text: &[u16], open: u16, close: u16, meter: &Meter, mut write: impl FnMut(&[u16], &mut Units) -> Result<()>)
    -> Result<Units> {
    meter.work(text.len())?;
    let mut out = Units::with_capacity(text.len());
    let mut pos = 0;
    let mut index = 0;
    while index < text.len() {
        if text[index] != open {
            index += 1;
            continue;
        }
        // At least one code point, none a line terminator, then the first `close`. Stepping by units finds the same
        // `close`, since it and the line terminators are BMP characters outside the surrogates.
        let mut end = index + 1;
        let mut found = None;
        while end < text.len() && !is_line_terminator(text[end]) {
            if end > index + 1 && text[end] == close {
                found = Some(end);
                break;
            }
            end += 1;
        }
        meter.work(end - index)?;
        let Some(end) = found else {
            index = end;
            continue;
        };
        out.extend_from_slice(&text[pos..index]);
        let before = out.len();
        write(&text[index + 1..end], &mut out)?;
        meter.alloc(2 * (out.len() - before))?;
        meter.text(out.len())?;
        pos = end + 1;
        index = end + 1;
    }
    out.extend_from_slice(&text[pos..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    /// One line of 20 000 openers without a close costs work linear in its length.
    #[test]
    fn unmatched_openers_scan_each_unit_once() {
        let line: Units = vec![0xe9b0; 20_000];
        let limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22, max_depth: 128,
            max_work: 4 * line.len() };
        let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits");
        let meter = Meter::new(&budget);
        let out = replace_pairs(&line, 0xe9b0, 0xe9b1, &meter, |_, _| Ok(())).expect("linear work");
        meter.finish().expect("linear work");
        assert_eq!(out, line);
    }

    /// The tokens' charges end with the Markdown render: 100,466 units of the timing inputs' prose peak at 2,140,512 budget bytes
    /// with the render's own meter and at 3,608,248 when the tokens share the stage's meter, so it renders under a
    /// 2,800,000-byte limit only with its own.
    #[test]
    fn markdown_tokens_are_released_before_the_html_passes() {
        let timing = |index: usize| format!("\"You came back,\" Mira said, setting the lantern down on the table. *She did not look up.* Block {index}.\n\n\
            The rain had not stopped since morning. Water ran along the gutters and pooled by the door, and the old clock above the hearth ticked on as if \
            nothing had changed. \"I said I would,\" he answered.\n\n밤새 내린 비에 길이 젖어 있었다. 그는 젖은 외투를 벗어 의자에 걸었다. \"늦어서 미안해.\" \
            *잠시 침묵이 흘렀다.* {index}\n\nShe poured two cups of tea and pushed one across. Steam curled between them. \"Sit. Tell me what you found, \
            and do not leave anything out this time.\"\n");
        let mut source = String::new();
        let mut index = 0;
        while source.encode_utf16().count() < 100_000 { source += &timing(index); index += 1; }
        assert_eq!(source.encode_utf16().count(), 100_466);
        let limits = Limits { max_owned_bytes: 2_800_000, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22,
            max_depth: 128, max_work: 1 << 40 };
        let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits");
        let text = Text::from_str(&source, &budget).expect("source");
        let settings = QuoteSettings::from_settings(false, None, false, false, &budget).expect("quotes");
        render_display_markdown(&text, Engine::Highlight, &settings, &[], &JsEngine::UNICODE_16, &budget).expect("within the limit");
    }
}
