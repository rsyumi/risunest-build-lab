//! The string stages `parseMarkdownForDisplay` (`src/ts/parser/parser.svelte.ts`) runs before Markdown:
//! `parseInlayAssets`, `parseThoughtsAndTools` (with `getStreamingThoughtPreview` and `renderStreamingThoughtPreview`
//! from `streamingThoughtPreview.ts`) and `encodeStyle`. What they read from storage, the language file or the DOM
//! comes in as arguments.

use crate::display::{code_point_len, substitute};
use crate::error::{RenderError, Result};
use crate::js_engine::{JsEngine, UnicodeVersion};
use crate::meter::Meter;
use crate::utils::{find, find_units, s, starts_with, units_of, Units};
use risunest_ir::{Budget, Text};
use std::collections::HashMap;

/// `InlayBlobType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlayKind { Image, Video, Audio, Signature }

/// The fields of an `InlayRenderSource` the markup reads.
#[derive(Debug)]
pub struct InlaySource {
    pub kind: InlayKind,
    pub mime: Text,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

/// One stored inlay `parseInlayAssets` can find: the metadata on the web, or the native render source.
#[derive(Debug)]
pub struct InlayEntry { pub id: Text, pub source: InlaySource }

/// A `DeferredInlayMarkerRegistry.register(id, source)` call the markup stands for, in call order.
#[derive(Debug, PartialEq)]
pub struct InlayRegistration { pub slot: u64, pub id: Text }

#[derive(Debug)]
pub struct InlayResult {
    pub text: Text,
    /// Registrations to make in this order on the render's registry, whose next slot was `first_slot`.
    pub registrations: Vec<InlayRegistration>,
}

/// What the inlay stage reads besides the text.
pub struct InlayInputs<'a> {
    pub sources: &'a [InlayEntry],
    /// `hideAllImages`.
    pub hide_all_images: bool,
    /// The slot the render's registry hands out next, or `None` when the registry is disposed and hands out none.
    pub first_slot: Option<u64>,
    /// `CSS.supports('contain', 'size') && CSS.supports('contain-intrinsic-size', '1px 1px')` in the WebView.
    pub image_reservation: bool,
}

fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

/// The matches of `/{{(inlay|inlayed|inlayeddata)::(.+?)}}/g`, as `[start, end)`. An opener without its `}}` on the
/// line ends the search on that line, since a later opener there sees less of the line, so each unit is scanned once.
fn inlay_matches(src: &[u16], meter: &Meter) -> Result<Vec<(usize, usize)>> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < src.len() {
        let rest = &src[index..];
        let prefix = ["{{inlay::", "{{inlayed::", "{{inlayeddata::"].iter().find(|prefix| starts_with(rest, prefix));
        let Some(after) = prefix.map(|prefix| index + prefix.len()) else {
            index += 1;
            continue;
        };
        // One or more units that are not line terminators, then the first `}}`.
        let mut at = after;
        let mut close = None;
        while at < src.len() && !is_line_terminator(src[at]) {
            if at > after && src[at] == 0x7d && src.get(at + 1) == Some(&0x7d) {
                close = Some(at + 2);
                break;
            }
            at += 1;
        }
        meter.work(at - index)?;
        match close {
            Some(end) => {
                found.push((index, end));
                index = end;
            }
            None => index = at,
        }
    }
    Ok(found)
}

fn escape_attribute(value: &[u16], out: &mut Units) {
    for &unit in value {
        match unit {
            0x26 => out.extend(s("&amp;")),
            0x22 => out.extend(s("&quot;")),
            0x27 => out.extend(s("&#39;")),
            0x3c => out.extend(s("&lt;")),
            0x3e => out.extend(s("&gt;")),
            unit => out.push(unit),
        }
    }
}

/// `mediaMimeHint`: only ASCII case matters, since every hint is ASCII and no other character lowercases to one of
/// its letters.
fn mime_hint(mime: &[u16]) -> Option<Units> {
    let mut lower: Units = mime.iter().map(|&unit| if (0x41..=0x5a).contains(&unit) { unit + 0x20 } else { unit }).collect();
    if lower == s("audio/mp3") { lower = s("audio/mpeg"); }
    ["video/mp4", "video/webm", "audio/webm", "audio/mpeg", "audio/ogg", "audio/wav", "audio/flac"].iter().any(|hint| lower == s(hint)).then_some(lower)
}

/// `validImageDimensions`, then the dimensions as JS prints them.
fn dimensions(source: &InlaySource) -> Option<(u64, u64)> {
    let valid = |value: Option<f64>| value.filter(|value| value.fract() == 0.0 && *value > 0.0 && *value <= 4294967295.0).map(|value| value as u64);
    Some((valid(source.width)?, valid(source.height)?))
}

/// `renderDeferredInlaySourceMarkup(id, source, registry)` with a registry that hands out `slot`.
fn inlay_markup(id: &[u16], source: &InlaySource, slot: Option<u64>, reservation: bool, out: &mut Units) {
    let marker = slot.map(|slot| [&s(" data-risu-inlay-slot=\"")[..], &s(&base36(slot))[..], &s("\"")[..]].concat()).unwrap_or_default();
    let size = dimensions(source);
    let push_id = |out: &mut Units| {
        out.extend(s("data-risu-inlay-id=\""));
        escape_attribute(id, out);
        out.push(0x22);
    };
    match source.kind {
        InlayKind::Image => {
            out.extend(s("<img "));
            push_id(out);
            out.extend_from_slice(&marker);
            if let Some((width, height)) = size {
                out.extend(s(&format!(" width=\"{width}\" height=\"{height}\"")));
                if slot.is_some() && reservation {
                    out.extend(s(&format!(" data-risu-image-size=\"{width} {height}\" style=\"--risu-image-width:{width}px;--risu-image-height:{height}px\"")));
                }
            }
            out.extend(s(" loading=\"lazy\"/>"));
        }
        InlayKind::Video | InlayKind::Audio => {
            let tag = if source.kind == InlayKind::Video { "video" } else { "audio" };
            out.extend(s(&format!("<{tag} controls><source ")));
            push_id(out);
            out.extend_from_slice(&marker);
            let mime: Units = source.mime.units().collect();
            if let Some(hint) = mime_hint(&mime) {
                out.extend(s(" type=\""));
                out.extend(hint);
                out.push(0x22);
            }
            out.extend(s(&format!("></{tag}>")));
        }
        InlayKind::Signature => {}
    }
}

fn base36(mut value: u64) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % 36) as usize]);
        value /= 36;
        if value == 0 { break; }
    }
    digits.reverse();
    String::from_utf8(digits).expect("ascii")
}

/// The ids `parseInlayAssets` looks up, one per `{{inlay::id}}`, `{{inlayed::id}}` or `{{inlayeddata::id}}` in order:
/// the stored inlays the host finds for them are the `sources` of [`parse_inlay_assets`].
pub fn find_inlays(data: &Text, budget: &Budget) -> Result<Vec<Text>> {
    let meter = Meter::new(budget);
    let text: Units = units_of(&data);
    meter.alloc(2 * text.len())?;
    let mut ids = Vec::new();
    for (start, end) in inlay_matches(&text, &meter)? {
        let inlay = &text[start..end];
        let colon = (0..inlay.len() - 1).find(|&index| inlay[index] == 0x3a && inlay[index + 1] == 0x3a).expect("::");
        ids.push(Text::from_units(&inlay[colon + 2..inlay.len() - 2], budget)?);
    }
    meter.finish()?;
    Ok(ids)
}

/// Whether `units` holds `{{inlay`, which every match text starts with.
fn holds_opener(units: &[u16]) -> bool { units.windows(7).any(|window| starts_with(window, "{{inlay")) }

/// Whether `text` ends with the start of an opener, which the text after an empty replacement could complete.
fn ends_in_opener(text: &[u16]) -> bool {
    (1..=text.len().min(15)).any(|length| {
        let tail = &text[text.len() - length..];
        ["{{inlay::", "{{inlayed::", "{{inlayeddata::"].iter()
            .any(|opener| opener.len() >= length && tail.iter().zip(opener.bytes()).all(|(unit, byte)| *unit == byte as u16))
    })
}

/// [`substitute`] for a match whose text before it is all of `out` so far and whose text after it is `after`.
fn substitute_after(replacement: &[u16], matched: &[u16], after: &[u16], out: &mut Units) {
    let before = out.len();
    let mut index = 0;
    while index < replacement.len() {
        let unit = replacement[index];
        if unit == 0x24 {
            match replacement.get(index + 1) {
                Some(0x24) => { out.push(0x24); index += 2; continue; }
                Some(0x26) => { out.extend_from_slice(matched); index += 2; continue; }
                Some(0x60) => { out.extend_from_within(..before); index += 2; continue; }
                Some(0x27) => { out.extend_from_slice(after); index += 2; continue; }
                _ => {}
            }
        }
        out.push(unit);
        index += 1;
    }
}

/// Charges a buffer's growth past the most it has held.
fn charge_growth(meter: &Meter, buffer: &Units, held: &mut usize) -> Result<()> {
    if buffer.len() > *held {
        meter.alloc(2 * (buffer.len() - *held))?;
        *held = buffer.len();
    }
    Ok(())
}

/// `parseInlayAssets(data, registry, hideAllImages)`: each `{{inlay::id}}`, `{{inlayed::id}}` or
/// `{{inlayeddata::id}}` with a stored image, video or audio becomes its deferred markup, the first occurrence of the
/// match text at a time, as `data.replace(inlay, markup)` does.
///
/// Each match is the first occurrence of its text, so one pass writes the result, until the text before the next
/// match could hold an earlier one: a skipped match or a replacement holding `{{inlay`, or an empty replacement right
/// after the start of an opener. Every other place an occurrence could start is one where the regex was tried and
/// failed. This depends on [`inlay_markup`] and the `inlayed` wrapper starting with `<` and ending with `>` or a line
/// break, which no opener holds, so no opener spans the edges of a replacement. From there each replacement searches
/// the whole text, as JS does.
pub fn parse_inlay_assets(data: &Text, inputs: &InlayInputs, budget: &Budget) -> Result<InlayResult> {
    let meter = Meter::new(budget);
    let source: Units = units_of(&data);
    meter.alloc(2 * source.len())?;
    meter.work(source.len())?;
    let matches = inlay_matches(&source, &meter)?;
    // The first entry for each id, as a search of the list in order finds it.
    let mut entries: HashMap<Units, &InlayEntry> = HashMap::new();
    if !matches.is_empty() {
        for entry in inputs.sources {
            let id: Units = entry.id.units().collect();
            meter.work(id.len())?;
            meter.alloc(2 * id.len() + 32)?;
            entries.entry(id).or_insert(entry);
        }
    }
    let mut registrations = Vec::new();
    let mut next_slot = inputs.first_slot;
    // `text` is the result for `source[..cursor]` until `general`, and the whole text after it.
    let (mut text, mut next, mut replacement) = (Units::with_capacity(source.len()), Units::new(), Units::new());
    let (mut text_held, mut next_held, mut cursor, mut general) = (0, 0, 0, false);
    for (start, end) in matches {
        let inlay = &source[start..end];
        meter.work(inlay.len())?;
        let colon = (0..inlay.len() - 1).find(|&index| inlay[index] == 0x3a && inlay[index + 1] == 0x3a).expect("::");
        let id = &inlay[colon + 2..inlay.len() - 2];
        let inlayed = starts_with(inlay, "{{inlayed");
        let Some(entry) = entries.get(id).filter(|entry| entry.source.kind != InlayKind::Signature) else {
            if !general && holds_opener(&inlay[2..]) {
                text.extend_from_slice(&source[cursor..]);
                cursor = source.len();
                general = true;
                charge_growth(&meter, &text, &mut text_held)?;
            }
            continue;
        };
        replacement.clear();
        if !(entry.source.kind == InlayKind::Image && inputs.hide_all_images) {
            if inlayed { replacement.extend(s("<div class=\"risu-inlay-image\">")); }
            inlay_markup(id, &entry.source, next_slot, inputs.image_reservation, &mut replacement);
            if let Some(slot) = next_slot {
                registrations.push(InlayRegistration { slot, id: entry.id.copy(budget)? });
                next_slot = Some(slot + 1);
            }
            if inlayed { replacement.extend(s("</div>\n\n")); }
        }
        if !general {
            text.extend_from_slice(&source[cursor..start]);
            let at = text.len();
            substitute_after(&replacement, inlay, &source[end..], &mut text);
            meter.work(start - cursor + text.len() - at)?;
            cursor = end;
            meter.text(text.len() + source.len() - cursor)?;
            debug_assert!(replacement.is_empty() || (replacement[0] == 0x3c && matches!(replacement.last(), Some(0x3e | 0x0a))),
                "an opener could span the edges of the markup");
            if if replacement.is_empty() { ends_in_opener(&text) } else { holds_opener(&text[at..]) } {
                text.extend_from_slice(&source[cursor..]);
                cursor = source.len();
                general = true;
            }
            charge_growth(&meter, &text, &mut text_held)?;
            continue;
        }
        meter.work(text.len())?;
        let Some(at) = find_units(&text, inlay, 0) else { continue };
        next.clear();
        next.extend_from_slice(&text[..at]);
        substitute(&replacement, &text, at, at + inlay.len(), &mut next);
        next.extend_from_slice(&text[at + inlay.len()..]);
        meter.text(next.len())?;
        charge_growth(&meter, &next, &mut next_held)?;
        std::mem::swap(&mut text, &mut next);
        std::mem::swap(&mut text_held, &mut next_held);
    }
    text.extend_from_slice(&source[cursor..]);
    charge_growth(&meter, &text, &mut text_held)?;
    meter.finish()?;
    Ok(InlayResult { text: Text::from_units(&text, budget)?, registrations })
}

/// `StreamingThoughtMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThoughtMode { Off, Recent, Collapsed }

/// The language strings `parseThoughtsAndTools` reads.
pub struct ThoughtLabels<'a> {
    /// `language.cot`.
    pub cot: &'a Text,
    /// `language.toolCalled`, with its `{{tool}}` slot.
    pub tool_called: &'a Text,
}

const OPEN_TAG: &str = "<Thoughts>";
const CLOSE_TAG: &str = "</Thoughts>";
const MAX_PREVIEW: usize = 800;
const PREVIEW_LINES: usize = 4;

struct Preview { before: Units, after: Units, recent: Units, truncated: bool }

/// Units of the first extended grapheme cluster of `text` (not empty), as `getStreamingThoughtPreview` drops it with
/// the engine's `Intl.Segmenter`: without one, one unit when `text` starts with a low surrogate and none otherwise;
/// with Unicode 16 or 17 tables, [`settled_first_cluster`]; with other tables, a decline.
fn first_cluster(text: &[u16], segmenter: Option<UnicodeVersion>) -> Result<usize> {
    match segmenter {
        None => Ok(usize::from((0xdc00..=0xdfff).contains(&text[0]))),
        Some(version @ (UnicodeVersion::Unicode16 | UnicodeVersion::Unicode17)) => settled_first_cluster(text, version),
        Some(UnicodeVersion::Before16 | UnicodeVersion::From18) => Err(RenderError::Unsupported("thought-preview-grapheme")),
    }
}

/// The first extended grapheme cluster under Unicode 16 or 17 when its first two code points settle it without the
/// break tables: an ASCII, Hangul syllable, kana or CJK ideograph second code point after a first one that neither
/// prepends nor is a Hangul leading jamo, or CR LF. Anything else declines. The two versions differ only in U+11A3A,
/// which prepends in Unicode 16 and not in the recorded Unicode 17 segmenter.
fn settled_first_cluster(text: &[u16], version: UnicodeVersion) -> Result<usize> {
    let first = code_point_len(text, 0);
    let Some(&second) = text.get(first) else { return Ok(first) };
    let lead = if first == 2 { 0x10000 + (((text[0] as u32) - 0xd800) << 10) + ((text[1] as u32) - 0xdc00) } else { text[0] as u32 };
    if lead == 0x0d { return Ok(if second == 0x0a { 2 } else { 1 }); }
    let simple = matches!(second, 0x00..=0x7f | 0xac00..=0xd7a3 | 0x3041..=0x3096 | 0x30a1..=0x30fa | 0x3400..=0x4dbf | 0x4e00..=0x9fff);
    let prepends = matches!(lead, 0x0600..=0x0605 | 0x06dd | 0x070f | 0x0890..=0x0891 | 0x08e2 | 0x0d4e | 0x110bd | 0x110cd | 0x111c2..=0x111c3
        | 0x113d1 | 0x1193f | 0x11941 | 0x11a84..=0x11a89 | 0x11d46 | 0x11f02) || (lead == 0x11a3a && version == UnicodeVersion::Unicode16);
    let leading_jamo = matches!(lead, 0x1100..=0x115f | 0xa960..=0xa97c);
    if simple && !prepends && !leading_jamo { Ok(first) } else { Err(RenderError::Unsupported("thought-preview-grapheme")) }
}

/// `appendSpan` of `getStreamingThoughtPreview`: text outside thoughts is kept, thought text keeps a bounded suffix.
fn append_span<'s>(source: &'s [u16], end: usize, span_start: usize, depth: usize, outside: &mut Vec<&'s [u16]>, recent: &mut Units,
    truncated: &mut bool) {
    if end <= span_start { return; }
    if depth == 0 {
        outside.push(&source[span_start..end]);
        return;
    }
    let length = end - span_start;
    if length >= MAX_PREVIEW {
        *truncated |= recent.len() + length > MAX_PREVIEW;
        *recent = source[end - MAX_PREVIEW..end].to_vec();
    } else {
        recent.extend_from_slice(&source[span_start..end]);
        if recent.len() > MAX_PREVIEW {
            *truncated = true;
            *recent = recent[recent.len() - MAX_PREVIEW..].to_vec();
        }
    }
}

/// `getStreamingThoughtPreview(source)` in an engine whose `Intl.Segmenter` has the tables of `segmenter`.
fn streaming_preview(source: &[u16], segmenter: Option<UnicodeVersion>) -> Result<Option<Preview>> {
    if find(source, OPEN_TAG, 0).is_none() { return Ok(None); }
    let mut depth = 0usize;
    let mut found = false;
    let mut before = Units::new();
    let mut outside: Vec<&[u16]> = Vec::new();
    let mut recent = Units::new();
    let mut truncated = false;
    let mut cursor = 0;
    let mut span_start = 0;
    let open = s(OPEN_TAG);
    let close = s(CLOSE_TAG);
    while cursor < source.len() {
        let Some(next) = crate::utils::find_unit(source, 0x3c, cursor) else { break };
        let opening = source[next..].starts_with(&open);
        let closing = depth > 0 && source[next..].starts_with(&close);
        if opening || closing {
            append_span(source, next, span_start, depth, &mut outside, &mut recent, &mut truncated);
            if opening {
                if depth == 0 {
                    if !found {
                        before = outside.concat();
                        outside.clear();
                    }
                    found = true;
                    recent.clear();
                    truncated = false;
                }
                depth += 1;
            } else {
                depth -= 1;
            }
            cursor = next + if opening { open.len() } else { close.len() };
            span_start = cursor;
        } else {
            let rest = &source[next..];
            if rest.len() < close.len() && (open.starts_with(rest) || (depth > 0 && close.starts_with(rest))) {
                append_span(source, next, span_start, depth, &mut outside, &mut recent, &mut truncated);
                span_start = source.len();
                break;
            }
            cursor = next + 1;
        }
    }
    append_span(source, source.len(), span_start, depth, &mut outside, &mut recent, &mut truncated);
    if !found { return Ok(None); }
    if truncated && !recent.is_empty() {
        let cut = first_cluster(&recent, segmenter)?;
        recent.drain(..cut);
    }
    let trimmed = recent.len() - recent.iter().rev().take_while(|unit| risunest_ir::text::is_whitespace(**unit)).count();
    recent.truncate(trimmed);
    let mut line_start = Some(recent.len());
    for _ in 0..PREVIEW_LINES {
        // `lastIndexOf('\n', lineStart - 1)`, where a negative position searches only index 0.
        let from = line_start.unwrap_or(0);
        line_start = if from == 0 { (recent.first() == Some(&0x0a)).then_some(0) } else {
            recent[..from.min(recent.len())].iter().rposition(|unit| *unit == 0x0a)
        };
        if line_start.is_none() { break; }
    }
    if let Some(start) = line_start {
        recent.drain(..start + 1);
        truncated = true;
    }
    Ok(Some(Preview { before, after: outside.concat(), recent, truncated }))
}

fn escape_preview(value: &[u16], out: &mut Units) {
    for &unit in value {
        match unit {
            0x26 => out.extend(s("&amp;")),
            0x3c => out.extend(s("&lt;")),
            0x3e => out.extend(s("&gt;")),
            0x22 => out.extend(s("&quot;")),
            0x0d => out.extend(s("&#13;")),
            0x0a => out.extend(s("&#10;")),
            unit => out.push(unit),
        }
    }
}

/// `renderStreamingThoughtPreview(preview, label)`.
fn render_preview(preview: &Preview, label: &[u16]) -> Units {
    let mut out = preview.before.clone();
    out.extend(s("\n\n<div class=\"x-risu-streaming-thought-preview\" role=\"note\" data-streaming-thought-preview><div class=\"x-risu-streaming-thought-label\">"));
    escape_preview(label, &mut out);
    out.extend(s(&format!("</div><div class=\"x-risu-streaming-thought-window\" data-truncated=\"{}\"><div class=\"x-risu-streaming-thought-text\">", preview.truncated)));
    if preview.recent.is_empty() { out.push(0x2026) } else { escape_preview(&preview.recent, &mut out) }
    out.extend(s("</div></div></div>\n\n"));
    out.extend_from_slice(&preview.after);
    out
}

/// `parseThoughtsAndTools(data, thoughtMode)`: the streaming preview in `recent` mode, each `<Thoughts>` block as a
/// `<details>`, and each `<tool_call>` as its notice. The preview drops the first grapheme cluster of a truncated
/// window with the `Intl.Segmenter` of `js`.
pub fn parse_thoughts_and_tools(data: &Text, mode: ThoughtMode, labels: &ThoughtLabels, js: &JsEngine, budget: &Budget) -> Result<Text> {
    let meter = Meter::new(budget);
    let mut data: Units = units_of(&data);
    meter.alloc(4 * data.len())?;
    meter.work(data.len())?;
    let cot: Units = labels.cot.units().collect();
    if mode == ThoughtMode::Recent {
        if let Some(preview) = streaming_preview(&data, js.segmenter)? { data = render_preview(&preview, &cot); }
    }
    meter.alloc(2 * data.len())?;
    let open = s(OPEN_TAG);
    let close = s(CLOSE_TAG);
    let mut result = Units::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        meter.work(1)?;
        let Some(start) = find(&data, OPEN_TAG, i) else {
            result.extend_from_slice(&data[i..]);
            break;
        };
        result.extend_from_slice(&data[i..start]);
        i = start;
        let mut j = i + 10;
        let mut depth = 1;
        while depth > 0 {
            match crate::utils::find_unit(&data, 0x3c, j) {
                None => {
                    j = data.len();
                    break;
                }
                Some(at) => j = at,
            }
            if data[j..].starts_with(&open) { depth += 1 } else if data[j..].starts_with(&close) { depth -= 1 }
            j += 1;
        }
        if depth == 0 || mode == ThoughtMode::Collapsed {
            let content_end = if depth == 0 { j - 1 } else { data.len() };
            result.extend(s("<details data-risu-thought"));
            if mode == ThoughtMode::Collapsed { result.extend(s(" data-risu-streaming-thought")); }
            result.extend(s("><summary>"));
            result.extend_from_slice(&cot);
            result.extend(s("</summary>"));
            result.extend_from_slice(&data[(i + 10).min(content_end)..content_end]);
            result.extend(s("</details>"));
            i = if depth == 0 { j + 10 } else { data.len() };
            meter.alloc(2 * (cot.len() + 60))?;
            continue;
        }
        result.push(data[i]);
        i += 1;
    }
    drop(data);
    let text = tool_calls(&result, labels.tool_called, &meter)?;
    meter.text(text.len())?;
    meter.finish()?;
    Ok(Text::from_units(&text, budget)?)
}

/// `.replace(/<tool_call>(.+?)<\/tool_call>/gms, ...)` of `parseThoughtsAndTools`.
fn tool_calls(text: &[u16], label: &Text, meter: &Meter) -> Result<Units> {
    let label: Units = label.units().collect();
    let slot = find(&label, "{{tool}}", 0);
    let mut out = Units::with_capacity(text.len());
    let mut pos = 0;
    let mut index = 0;
    while let Some(start) = find(text, "<tool_call>", index) {
        let content = start + 11;
        let Some(end) = (content < text.len()).then(|| find(text, "</tool_call>", content + 1)).flatten() else { break };
        let call = &text[content..end];
        out.extend_from_slice(&text[pos..start]);
        out.extend(s("<div class=\"x-risu-tool-call\">\u{1f6e0}\u{fe0f} "));
        let mut parts = call.split(|unit| *unit == 0xf100);
        let name = parts.next().and(parts.next()).map(|part| part.to_vec()).unwrap_or_else(|| s("unknown"));
        match slot {
            Some(at) => {
                out.extend_from_slice(&label[..at]);
                substitute(&name, &label, at, at + 8, &mut out);
                out.extend_from_slice(&label[at + 8..]);
            }
            None => out.extend_from_slice(&label),
        }
        out.extend(s("</div>\n\n"));
        meter.alloc(2 * (label.len() + name.len() + 40))?;
        meter.text(out.len())?;
        pos = end + 12;
        index = end + 12;
    }
    out.extend_from_slice(&text[pos..]);
    Ok(out)
}

/// `encodeStyle`: each `<style>…</style>` body as the hex of its UTF-8 (lone surrogates as U+FFFD, as `Buffer.from`
/// writes them) inside `<risu-style>`.
pub fn encode_style(data: &Text, budget: &Budget) -> Result<Text> {
    let meter = Meter::new(budget);
    let text: Units = units_of(&data);
    meter.alloc(2 * text.len())?;
    meter.work(text.len())?;
    let mut out = Units::with_capacity(text.len());
    let mut pos = 0;
    let mut index = 0;
    while let Some(start) = find(&text, "<style>", index) {
        let content = start + 7;
        let Some(end) = (content < text.len()).then(|| find(&text, "</style>", content + 1)).flatten() else { break };
        out.extend_from_slice(&text[pos..start]);
        out.extend(s("<risu-style>"));
        for decoded in char::decode_utf16(text[content..end].iter().copied()) {
            let mut bytes = [0u8; 4];
            for byte in decoded.unwrap_or('\u{fffd}').encode_utf8(&mut bytes).bytes() {
                out.extend(s(&format!("{byte:02x}")));
            }
        }
        out.extend(s("</risu-style>"));
        meter.alloc(2 * 6 * (end - content))?;
        meter.text(out.len())?;
        pos = end + 8;
        index = end + 8;
    }
    out.extend_from_slice(&text[pos..]);
    meter.finish()?;
    Ok(Text::from_units(&out, budget)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use serde_json::Value;
    use std::sync::{atomic::AtomicBool, Arc};

    const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/grapheme.cases.json");
    const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/grapheme.expected.json");
    // The same recording in a Unicode 17 WebView engine (`render-grapheme-webview.capture.mjs`).
    const WEBVIEW_CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/grapheme-webview.cases.json");
    const WEBVIEW_EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/grapheme-webview.expected.json");

    /// The cases and the recorded results by id of the Node recording, after the fixture hash check.
    fn recorded() -> (Vec<Value>, Vec<Value>) { recorded_in(CASES, EXPECTED) }

    fn recorded_in(cases: &[u8], expected: &[u8]) -> (Vec<Value>, Vec<Value>) {
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(cases).iter().map(|byte| format!("{byte:02x}")).collect();
        let cases: Value = serde_json::from_slice(cases).expect("cases");
        let expected: Value = serde_json::from_slice(expected).expect("expected");
        assert_eq!(expected["fixtureSha256"], digest.as_str(), "regenerate the expected file");
        let cases = cases["cases"].as_array().expect("cases").clone();
        let results: Vec<Value> = expected["cases"].as_array().expect("cases").iter().zip(&cases).map(|(entry, case)| {
            assert_eq!(entry["id"], case["id"]);
            entry["result"].clone()
        }).collect();
        (cases, results)
    }

    fn ranges(value: &Value) -> Vec<(u32, u32)> {
        value.as_array().expect("ranges").iter().map(|range| (range[0].as_u64().expect("start") as u32, range[1].as_u64().expect("end") as u32)).collect()
    }

    fn contains(ranges: &[(u32, u32)], code: u32) -> bool {
        let index = ranges.partition_point(|(start, _)| *start <= code);
        index > 0 && code <= ranges[index - 1].1
    }

    /// One code point as JS writes it, a surrogate as a lone unit.
    fn code_units(code: u32) -> Vec<u16> {
        match char::from_u32(code) {
            Some(character) => character.encode_utf16(&mut [0; 2]).to_vec(),
            None => vec![code as u16],
        }
    }

    /// Checks [`first_cluster`] for `version` against a recording: wherever the rules settle the first cluster of a
    /// lead code point and a second one, the recorded segmenter ends it at the same unit. The seconds stand for every
    /// second code point the rules accept: those share their `Grapheme_Cluster_Break` value with one of them and have
    /// no `Indic_Conjunct_Break` or `Extended_Pictographic` value, the only properties UAX #29 reads, and the recording
    /// checks that each such second breaks as its stand-in after a lead of every property class. Returns the leads
    /// declined before an 'a'.
    fn first_clusters_end_where_recorded(cases: &[Value], results: &[Value], version: UnicodeVersion) -> Vec<u32> {
        let seconds: Vec<u32> = cases[0]["seconds"].as_array().expect("seconds").iter().map(|code| code.as_u64().expect("code") as u32).collect();
        let joined = results[0]["joined"].as_array().expect("joined");
        assert_eq!(joined.len(), seconds.len());
        assert_eq!(results[0]["classMismatches"], Value::Array(Vec::new()));
        let (mut settled, mut declined, mut wrong, mut declined_before_a) = (0usize, 0usize, Vec::new(), Vec::new());
        for (&second, joined) in seconds.iter().zip(joined) {
            let joined = ranges(joined);
            let tail = code_units(second);
            for code in 0..=0x10ffffu32 {
                let mut text = code_units(code);
                let lead = text.len();
                text.extend_from_slice(&tail);
                let js = lead + if contains(&joined, code) { tail.len() } else { 0 };
                match first_cluster(&text, Some(version)) {
                    Ok(units) if units == js => settled += 1,
                    Ok(units) => wrong.push((code, second, units, js)),
                    Err(_) => {
                        declined += 1;
                        if second == 0x61 { declined_before_a.push(code); }
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{} pairs end elsewhere (lead, second, rust, js): {:x?}", wrong.len(), &wrong[..wrong.len().min(20)]);
        assert!(settled > 8_000_000 && declined > 0, "{settled} settled, {declined} declined");
        declined_before_a
    }

    #[test]
    fn unicode_16_first_clusters_end_where_the_recorded_segmenter_ends_them() {
        let (cases, results) = recorded();
        first_clusters_end_where_recorded(&cases, &results, UnicodeVersion::Unicode16);
    }

    /// The Unicode 17 rules settle every pair the recorded WebView segmenter ends where they say, and before an 'a' they
    /// decline exactly the leads that segmenter keeps with it and the Hangul leading jamo.
    #[test]
    fn unicode_17_first_clusters_end_where_the_recorded_webview_segmenter_ends_them() {
        let (cases, results) = recorded_in(WEBVIEW_CASES, WEBVIEW_EXPECTED);
        let declined = first_clusters_end_where_recorded(&cases, &results, UnicodeVersion::Unicode17);
        let joined_a = ranges(&results[0]["joined"][3]);
        assert_eq!(cases[0]["seconds"][3], 0x61);
        let expected: Vec<u32> = (0..=0x10ffffu32).filter(|&code| contains(&joined_a, code) || matches!(code, 0x1100..=0x115f | 0xa960..=0xa97c))
            .collect();
        assert_eq!(declined, expected, "the leads declined before an 'a'");
    }

    /// The seconds the rules accept are exactly the ones the recording checks.
    #[test]
    fn the_rules_accept_exactly_the_seconds_the_recording_checks() {
        let (cases, _) = recorded();
        let (webview, _) = recorded_in(WEBVIEW_CASES, WEBVIEW_EXPECTED);
        assert_eq!(cases[0], webview[0]);
        let simple = ranges(&cases[0]["simple"]);
        for version in [UnicodeVersion::Unicode16, UnicodeVersion::Unicode17] {
            let differ: Vec<u32> = (0..=0x10ffffu32).filter(|&code| {
                let mut text = vec![0x61];
                text.extend(code_units(code));
                settled_first_cluster(&text, version).is_ok() != contains(&simple, code)
            }).collect();
            assert!(differ.is_empty(), "{version:?} accepts other seconds than the checked ones: {:x?}", &differ[..differ.len().min(20)]);
        }
    }

    /// `c` has `Grapheme_Extend` in Rust's tables: `escape_debug` escapes it alone but not after another character.
    fn rust_grapheme_extend(c: char) -> bool {
        c.escape_debug().to_string() != c.to_string() && format!("a{c}").escape_debug().to_string() == format!("a{c}")
    }

    /// The probes the `JsEngine` documentation gives read the recorded engine as Unicode 16, and the Unicode 17 one
    /// tests a mark Rust's newer tables extend with and the recorded segmenter does not.
    #[test]
    fn the_engine_probes_read_the_recorded_engine_as_unicode_16() {
        let (cases, results) = recorded();
        let probes = &cases[2];
        assert_eq!(probes["id"], "probes");
        assert_eq!(results[2], serde_json::json!({ "case16": true, "case17": false, "segment16": true, "segment17": false }));
        let code = |name: &str| probes[name].as_u64().expect("code") as u32;
        let doc = include_str!("js_engine.rs");
        for text in [
            format!("'\\u{{{:X}}}'.toUpperCase() === '\\u{{{:X}}}'", code("case16Small"), code("case16Capital")),
            format!("'\\u{{{:X}}}'.toUpperCase() !== '\\u{{{:X}}}'", code("case17"), code("case17")),
            format!("first('\\u{{{:X}}}a') !== '\\u{{{:X}}}a'", code("segment16"), code("segment16")),
            format!("first('a\\u{{{:X}}}') === 'a\\u{{{:X}}}'", code("segment17"), code("segment17")),
        ] {
            assert!(doc.contains(&text), "the JsEngine documentation gives {text}");
        }
        let extend = ranges(&results[1]);
        let mark = char::from_u32(code("segment17")).expect("scalar");
        let candidates: Vec<u32> = (0..=0x10ffffu32).filter_map(char::from_u32)
            .filter(|&c| rust_grapheme_extend(c) && !contains(&extend, c as u32)).map(|c| c as u32).collect();
        assert!(candidates.contains(&(mark as u32)), "U+{:X} is not a newer extending mark; candidates {:x?}", mark as u32,
            &candidates[..candidates.len().min(40)]);
        let mut text = code_units(code("segment16"));
        text.push(0x61);
        assert!(settled_first_cluster(&text, UnicodeVersion::Unicode16).is_err(), "the Unicode 16 rules see the prepending probe");
    }

    /// The probes the `JsEngine` documentation gives read the recorded WebView engine as Unicode 17 for both tables,
    /// and its case tables as older than Unicode 18.
    #[test]
    fn the_engine_probes_read_the_recorded_webview_as_unicode_17() {
        let (cases, results) = recorded_in(WEBVIEW_CASES, WEBVIEW_EXPECTED);
        let probes = &cases[2];
        assert_eq!(probes["id"], "probes");
        assert_eq!(results[2], serde_json::json!({ "case16": true, "case17": true, "case18": false, "segment16": true, "segment17": true }));
        let code = |name: &str| probes[name].as_u64().expect("code") as u32;
        let doc = include_str!("js_engine.rs");
        for text in [
            format!("'\\u{{{:X}}}'.toUpperCase() !== '\\u{{{:X}}}'", code("case18Small"), code("case18Small")),
            format!("'\\u{{{:X}}}'.toLowerCase() !== '\\u{{{:X}}}'", code("case18Capital"), code("case18Capital")),
        ] {
            assert!(doc.contains(&text), "the JsEngine documentation gives {text}");
        }
        let mut text = code_units(code("segment16"));
        text.push(0x61);
        assert!(settled_first_cluster(&text, UnicodeVersion::Unicode17).is_err(), "the Unicode 17 rules see the prepending probe");
    }

    fn budget(max_work: usize) -> Budget {
        let limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22, max_depth: 128,
            max_work };
        Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
    }

    /// One line of 10 000 openers without `}}` costs work linear in its length.
    #[test]
    fn unmatched_inlay_openers_scan_each_unit_once() {
        let line = "{{inlay::".repeat(10_000);
        let budget = budget(8 * line.len());
        let data = Text::from_str(&line, &budget).expect("text");
        let inputs = InlayInputs { sources: &[], hide_all_images: false, first_slot: Some(0), image_reservation: false };
        let result = parse_inlay_assets(&data, &inputs, &budget).expect("linear work");
        assert!(result.text.units().eq(line.encode_utf16()));
    }

    fn image(id: &str, kind: InlayKind, budget: &Budget) -> InlayEntry {
        let mime = if kind == InlayKind::Video { "video/mp4" } else { "image/png" };
        InlayEntry { id: Text::from_str(id, budget).expect("id"),
            source: InlaySource { kind, mime: Text::from_str(mime, budget).expect("mime"), width: Some(640.0), height: Some(480.0) } }
    }

    /// `data.replace(inlay, markup)` for each match in turn, each searching the whole text: the stage as JS runs it.
    fn replace_each(data: &[u16], inputs: &InlayInputs, budget: &Budget) -> (Units, Vec<(u64, Units)>) {
        let meter = Meter::new(budget);
        let mut text = data.to_vec();
        let matches: Vec<Units> = inlay_matches(&text, &meter).expect("matches").into_iter().map(|(start, end)| text[start..end].to_vec()).collect();
        let (mut registrations, mut next_slot) = (Vec::new(), inputs.first_slot);
        for inlay in matches {
            let colon = (0..inlay.len() - 1).find(|&index| inlay[index] == 0x3a && inlay[index + 1] == 0x3a).expect("::");
            let id = &inlay[colon + 2..inlay.len() - 2];
            let Some(entry) = inputs.sources.iter().find(|entry| entry.id.units().eq(id.iter().copied())) else { continue };
            let mut replacement = Units::new();
            match entry.source.kind {
                InlayKind::Signature => continue,
                InlayKind::Image if inputs.hide_all_images => {}
                _ => {
                    let inlayed = starts_with(&inlay, "{{inlayed");
                    if inlayed { replacement.extend(s("<div class=\"risu-inlay-image\">")); }
                    inlay_markup(id, &entry.source, next_slot, inputs.image_reservation, &mut replacement);
                    if let Some(slot) = next_slot {
                        registrations.push((slot, id.to_vec()));
                        next_slot = Some(slot + 1);
                    }
                    if inlayed { replacement.extend(s("</div>\n\n")); }
                }
            }
            let Some(start) = (0..=text.len().saturating_sub(inlay.len())).find(|&index| text[index..].starts_with(&inlay)) else { continue };
            let mut next = text[..start].to_vec();
            substitute(&replacement, &text, start, start + inlay.len(), &mut next);
            next.extend_from_slice(&text[start + inlay.len()..]);
            text = next;
        }
        (text, registrations)
    }

    fn run(data: &[u16], inputs: &InlayInputs, budget: &Budget) -> Result<(Units, Vec<(u64, Units)>)> {
        let result = parse_inlay_assets(&Text::from_units(data, budget)?, inputs, budget)?;
        Ok((result.text.units().collect(), result.registrations.iter().map(|entry| (entry.slot, entry.id.units().collect())).collect()))
    }

    /// Texts built from openers, partial openers, braces, replacement patterns and line breaks, and from the shapes that
    /// switch to the search of the whole text, with a random subset of their ids stored, come out as the replacements
    /// one at a time over the whole text give them.
    #[test]
    fn inlay_replacements_match_replacing_each_over_the_whole_text() {
        const PIECES: [&str; 21] = ["{{inlay::", "{{inlayed::", "{{inlayeddata::", "{{inl", "ay::", "{{", "{", "}}", "}", "a", "b", "k", "$&", "$`",
            "$'", "$$", "\n", " ", "\"", "&", "<"];
        const KINDS: [InlayKind; 5] = [InlayKind::Image, InlayKind::Image, InlayKind::Video, InlayKind::Audio, InlayKind::Signature];
        let budget = budget(1 << 40);
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut below = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        // A match, a match whose id holds an opener, an opener split around a match that an empty replacement removes,
        // and an id whose `$&` copies the match into its markup.
        let shape = |kind: usize, id: &str| match kind {
            0 => format!("{{{{inlay::{id}}}}}"),
            1 => format!("{{{{inlay::{{{{inlay::{id}}}}}"),
            2 => format!("{{{{inl{{{{inlay::{id}}}}}ay::{id}}}}}"),
            _ => format!("{{{{inlay::{id}$&}}}}"),
        };
        for case in 0..20_000 {
            let source: String = (0..1 + below(16))
                .map(|_| if below(4) == 0 { shape(below(4), ["a", "b"][below(2)]) } else { PIECES[below(PIECES.len())].to_string() })
                .collect();
            let data: Units = source.encode_utf16().collect();
            let mut sources = Vec::new();
            for id in find_inlays(&Text::from_units(&data, &budget).expect("text"), &budget).expect("ids") {
                let id = String::from_utf16(&id.units().collect::<Units>()).expect("id");
                for _ in 0..below(3) { sources.push(image(&id, KINDS[below(KINDS.len())], &budget)); }
            }
            let first_slot = if below(4) == 0 { None } else { Some(below(3) as u64) };
            let inputs = InlayInputs { sources: &sources, hide_all_images: below(3) == 0, first_slot, image_reservation: below(2) == 0 };
            let expected = replace_each(&data, &inputs, &budget);
            assert_eq!(run(&data, &inputs, &budget).expect("render"), expected, "case {case}: {source:?}, hidden {}", inputs.hide_all_images);
        }
    }

    /// 300 stored images cost work and bytes within a fixed multiple of the text before and after, and once a skipped
    /// match holding an opener sends the rest through the search of the whole text, bytes still stay within it.
    #[test]
    fn inlay_replacements_cost_work_and_bytes_linear_in_the_text() {
        let wide = budget(1 << 40);
        let sources: Vec<InlayEntry> = (0..300).map(|index| image(&format!("n{index}"), InlayKind::Image, &wide)).collect();
        let inputs = InlayInputs { sources: &sources, hide_all_images: false, first_slot: Some(0), image_reservation: true };
        let blocks: String = (0..300).map(|index| format!("word {{{{inlay::n{index}}}}} ")).collect();
        for (prefix, work_bound) in [("", true), ("{{inlay::{{inlay::gone}}", false)] {
            let data: Units = format!("{prefix}{blocks}").encode_utf16().collect();
            let expected = replace_each(&data, &inputs, &wide);
            let units = data.len() + expected.0.len();
            let max_work = if work_bound { 8 * units } else { 1 << 40 };
            let limits = Limits { max_owned_bytes: 16 * units, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22, max_depth: 128,
                max_work };
            let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits");
            let result = run(&data, &inputs, &budget);
            eprintln!("prefix {prefix:?}: {units} units in and out, peak {} bytes", budget.peak_bytes());
            assert_eq!(result.expect("linear cost"), expected);
        }
    }
}
