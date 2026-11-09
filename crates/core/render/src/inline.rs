//! The inline parser of markdown-it 14.3.2: `lib/rules_inline/state_inline.mjs`, `lib/parser_inline.mjs`, the inline
//! rules of the `default` preset (`linkify` is a no-op with `linkify: false`) and the `ruler2` post-processing.

use crate::block::{References, MAX_NESTING};
use crate::entities::{decode_html, Mode};
use crate::error::{invariant, Result};
use crate::html;
use crate::js_engine::UnicodeVersion;
use crate::links::{parse_link_destination, parse_link_title};
use crate::meter::Meter;
use crate::token::{Kind, Token};
use crate::url::{normalize_link, normalize_link_text, validate_link};
use crate::utils::{
    code, find_unit, from_code_point, is_ascii_alnum, is_ascii_alpha, is_ascii_digit, is_ascii_hex, is_md_ascii_punct,
    is_punct_code, is_space, is_valid_entity_code, is_white_space, normalize_reference, parse_int, s, Units,
};
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct Delimiter {
    marker: u16,
    length: usize,
    token: usize,
    end: isize,
    open: bool,
    close: bool,
}

struct Scanned {
    can_open: bool,
    can_close: bool,
    length: usize,
}

struct InlineState<'s, 'r, 'm, 'b> {
    src: &'s [u16],
    references: &'r References,
    /// The engine's case tables, for reference labels.
    case: Option<UnicodeVersion>,
    meter: &'m Meter<'b>,
    tokens: Vec<Token>,
    pos: usize,
    pos_max: usize,
    level: i32,
    pending: Units,
    pending_level: i32,
    cache: HashMap<usize, usize>,
    /// Every delimiter list: index 0 is the state's own, the rest belong to opening tokens (`tokens_meta`), in push order.
    lists: Vec<Vec<Delimiter>>,
    delimiters: usize,
    previous_delimiters: Vec<usize>,
    backticks: HashMap<usize, usize>,
    backticks_scanned: bool,
}

/// `ParserInline.prototype.parse` (parser_inline.mjs): tokenize, then `balance_pairs`, `strikethrough`,
/// `emphasis` and `fragments_join`.
pub(crate) fn parse(src: &[u16], references: &References, case: Option<UnicodeVersion>, meter: &Meter) -> Result<Vec<Token>> {
    let _depth = meter.enter()?;
    let mut state = InlineState {
        src, references, case, meter, tokens: Vec::new(), pos: 0, pos_max: src.len(), level: 0, pending: Units::new(), pending_level: 0,
        cache: HashMap::new(), lists: vec![Vec::new()], delimiters: 0, previous_delimiters: Vec::new(), backticks: HashMap::new(),
        backticks_scanned: false,
    };
    state.tokenize()?;
    let order: Vec<usize> = std::iter::once(state.delimiters).chain((0..state.lists.len()).filter(|index| *index != state.delimiters)).collect();
    for &list in &order { process_delimiters(&mut state.lists[list]); }
    for &list in &order { strikethrough_post_process(&mut state.tokens, &state.lists[list]); }
    for &list in &order { emphasis_post_process(&mut state.tokens, &state.lists[list]); }
    fragments_join(&mut state.tokens);
    Ok(state.tokens)
}

/// `isTerminatorChar` (rules_inline/text.mjs).
fn is_terminator(unit: u16) -> bool {
    matches!(unit, 0x0a | 0x21 | 0x23 | 0x24 | 0x25 | 0x26 | 0x2a | 0x2b | 0x2d | 0x3a | 0x3c | 0x3d | 0x3e | 0x40 | 0x5b | 0x5c
        | 0x5d | 0x5e | 0x5f | 0x60 | 0x7b | 0x7d | 0x7e)
}

fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

const RULES: usize = 11;

/// Bytes charged per token, per opened delimiter list, per delimiter and per `skipToken` cache entry, as each is
/// created, so a run of markers stops at the budget instead of after it is tokenized.
const TOKEN_BYTES: usize = std::mem::size_of::<Token>();
const LIST_BYTES: usize = std::mem::size_of::<Vec<Delimiter>>() + std::mem::size_of::<usize>();
const DELIMITER_BYTES: usize = std::mem::size_of::<Delimiter>();
const CACHE_ENTRY_BYTES: usize = 4 * std::mem::size_of::<usize>();

impl InlineState<'_, '_, '_, '_> {
    fn push_pending(&mut self) -> Result<()> {
        self.meter.work(1)?;
        self.meter.alloc(TOKEN_BYTES + 2 * self.pending.len())?;
        let mut token = Token::new(Kind::Text, "", 0);
        token.content = std::mem::take(&mut self.pending);
        token.level = self.pending_level;
        self.tokens.push(token);
        Ok(())
    }

    /// `StateInline.prototype.push` (state_inline.mjs); returns the token's index.
    fn push(&mut self, kind: Kind, tag: &'static str, nesting: i8) -> Result<usize> {
        if !self.pending.is_empty() { self.push_pending()?; }
        self.meter.work(1)?;
        self.meter.alloc(TOKEN_BYTES + if nesting > 0 { LIST_BYTES } else { 0 })?;
        let mut token = Token::new(kind, tag, nesting);
        if nesting < 0 {
            self.level -= 1;
            self.delimiters = self.previous_delimiters.pop().unwrap_or(0);
        }
        token.level = self.level;
        if nesting > 0 {
            self.level += 1;
            self.previous_delimiters.push(self.delimiters);
            self.lists.push(Vec::new());
            self.delimiters = self.lists.len() - 1;
        }
        self.pending_level = self.level;
        self.tokens.push(token);
        Ok(self.tokens.len() - 1)
    }

    fn push_content(&mut self, kind: Kind, content: Units) -> Result<()> {
        let index = self.push(kind, "", 0)?;
        self.meter.alloc(2 * content.len())?;
        self.tokens[index].content = content;
        Ok(())
    }

    /// Adds a delimiter for the last pushed token to the current list (`state.delimiters.push`).
    fn push_delimiter(&mut self, marker: u16, length: usize, scanned: &Scanned) -> Result<()> {
        self.meter.alloc(DELIMITER_BYTES)?;
        let token = self.tokens.len() - 1;
        let list = self.delimiters;
        self.lists[list].push(Delimiter { marker, length, token, end: -1, open: scanned.can_open, close: scanned.can_close });
        Ok(())
    }

    /// Charges the attributes a link or image token holds.
    fn charge_attrs(&self, attrs: &[(&'static str, Units)]) -> Result<()> {
        self.meter.alloc(attrs.iter().map(|(_, value)| 2 * value.len() + 24).sum())
    }

    /// `scanDelims` (state_inline.mjs).
    fn scan_delims(&self, start: usize, can_split_word: bool) -> Scanned {
        let src = self.src;
        let max = self.pos_max;
        let marker = src[start];
        let last_char: u32 = if start == 0 {
            0x20
        } else if start == 1 {
            let unit = src[0];
            if (unit & 0xf800) == 0xd800 { 0xfffd } else { unit as u32 }
        } else {
            let unit = src[start - 1];
            if (unit & 0xfc00) == 0xdc00 {
                let high = src[start - 2];
                if (high & 0xfc00) == 0xd800 { 0x10000 + (((high - 0xd800) as u32) << 10) + (unit - 0xdc00) as u32 } else { 0xfffd }
            } else if (unit & 0xfc00) == 0xd800 {
                0xfffd
            } else {
                unit as u32
            }
        };
        let mut pos = start;
        while pos < max && src[pos] == marker { pos += 1; }
        let count = pos - start;
        let mut next_char: u32 = if pos < max { src[pos] as u32 } else { 0x20 };
        if (next_char & 0xfc00) == 0xd800 {
            next_char = match code(src, pos + 1) {
                Some(low) if (low & 0xfc00) == 0xdc00 => 0x10000 + ((next_char - 0xd800) << 10) + (low - 0xdc00) as u32,
                _ => 0xfffd,
            };
        } else if (next_char & 0xfc00) == 0xdc00 {
            next_char = 0xfffd;
        }
        let last_punct = is_md_ascii_punct(last_char) || is_punct_code(last_char);
        let next_punct = is_md_ascii_punct(next_char) || is_punct_code(next_char);
        let last_white = is_white_space(last_char);
        let next_white = is_white_space(next_char);
        let left_flanking = !next_white && (!next_punct || last_white || last_punct);
        let right_flanking = !last_white && (!last_punct || next_white || next_punct);
        Scanned {
            can_open: left_flanking && (can_split_word || !right_flanking || last_punct),
            can_close: right_flanking && (can_split_word || !left_flanking || next_punct),
            length: count,
        }
    }

    fn rule(&mut self, index: usize, silent: bool) -> Result<bool> {
        match index {
            0 => Ok(self.text(silent)),
            1 => self.newline(silent),
            2 => self.escape(silent),
            3 => self.backticks(silent),
            4 => self.strikethrough(silent),
            5 => self.emphasis(silent),
            6 => self.link(silent),
            7 => self.image(silent),
            8 => self.autolink(silent),
            9 => self.html_inline(silent),
            _ => self.entity(silent),
        }
    }

    /// `ParserInline.prototype.tokenize` (parser_inline.mjs).
    fn tokenize(&mut self) -> Result<()> {
        let end = self.pos_max;
        while self.pos < end {
            self.meter.work(1)?;
            let previous = self.pos;
            let mut ok = false;
            if self.level < MAX_NESTING {
                for rule in 0..RULES {
                    ok = self.rule(rule, false)?;
                    if ok {
                        if previous >= self.pos { return Err(invariant()); }
                        break;
                    }
                }
            }
            if ok {
                if self.pos >= end { break; }
                continue;
            }
            self.pending.push(self.src[self.pos]);
            self.pos += 1;
        }
        if !self.pending.is_empty() { self.push_pending()?; }
        Ok(())
    }

    /// `ParserInline.prototype.skipToken` (parser_inline.mjs).
    fn skip_token(&mut self) -> Result<()> {
        self.meter.work(1)?;
        let pos = self.pos;
        if let Some(&cached) = self.cache.get(&pos) {
            self.pos = cached;
            return Ok(());
        }
        let meter = self.meter;
        let _depth = meter.enter()?;
        let mut ok = false;
        if self.level < MAX_NESTING {
            for rule in 0..RULES {
                self.level += 1;
                ok = self.rule(rule, true)?;
                self.level -= 1;
                if ok {
                    if pos >= self.pos { return Err(invariant()); }
                    break;
                }
            }
        } else {
            self.pos = self.pos_max;
        }
        if !ok { self.pos += 1; }
        self.meter.alloc(CACHE_ENTRY_BYTES)?;
        self.cache.insert(pos, self.pos);
        Ok(())
    }

    /// `parseLinkLabel` (helpers/parse_link_label.mjs).
    fn parse_link_label(&mut self, start: usize, disable_nested: bool) -> Result<Option<usize>> {
        let max = self.pos_max;
        let old_pos = self.pos;
        self.pos = start + 1;
        let mut level = 1;
        let mut found = false;
        while self.pos < max {
            let marker = self.src[self.pos];
            if marker == 0x5d {
                level -= 1;
                if level == 0 {
                    found = true;
                    break;
                }
            }
            let previous = self.pos;
            self.skip_token()?;
            if marker == 0x5b {
                if previous == self.pos - 1 {
                    level += 1;
                } else if disable_nested {
                    self.pos = old_pos;
                    return Ok(None);
                }
            }
        }
        let label_end = found.then_some(self.pos);
        self.pos = old_pos;
        Ok(label_end)
    }

    /// rules_inline/text.mjs.
    fn text(&mut self, silent: bool) -> bool {
        let mut pos = self.pos;
        while pos < self.pos_max && !is_terminator(self.src[pos]) { pos += 1; }
        if pos == self.pos { return false; }
        if !silent { self.pending.extend_from_slice(&self.src[self.pos..pos]); }
        self.pos = pos;
        true
    }

    /// rules_inline/newline.mjs.
    fn newline(&mut self, silent: bool) -> Result<bool> {
        let mut pos = self.pos;
        if code(self.src, pos) != Some(0x0a) { return Ok(false); }
        let max = self.pos_max;
        if !silent {
            let length = self.pending.len();
            if length >= 1 && self.pending[length - 1] == 0x20 {
                if length >= 2 && self.pending[length - 2] == 0x20 {
                    let mut whitespace = length - 2;
                    while whitespace >= 1 && self.pending[whitespace - 1] == 0x20 { whitespace -= 1; }
                    self.pending.truncate(whitespace);
                    self.push(Kind::Hardbreak, "br", 0)?;
                } else {
                    self.pending.truncate(length - 1);
                    self.push(Kind::Softbreak, "br", 0)?;
                }
            } else {
                self.push(Kind::Softbreak, "br", 0)?;
            }
        }
        pos += 1;
        while pos < max && is_space(self.src[pos]) { pos += 1; }
        self.pos = pos;
        Ok(true)
    }

    /// rules_inline/escape.mjs.
    fn escape(&mut self, silent: bool) -> Result<bool> {
        let mut pos = self.pos;
        let max = self.pos_max;
        if code(self.src, pos) != Some(0x5c) { return Ok(false); }
        pos += 1;
        if pos >= max { return Ok(false); }
        let first = self.src[pos];
        if first == 0x0a {
            if !silent { self.push(Kind::Hardbreak, "br", 0)?; }
            pos += 1;
            while pos < max && is_space(self.src[pos]) { pos += 1; }
            self.pos = pos;
            return Ok(true);
        }
        if first == 0x20 {
            if !silent { self.push_content(Kind::TextSpecial, vec![0x5c])?; }
            self.pos = pos;
            return Ok(true);
        }
        let mut escaped = vec![first];
        if (0xd800..=0xdbff).contains(&first) && pos + 1 < max {
            let second = self.src[pos + 1];
            if (0xdc00..=0xdfff).contains(&second) {
                escaped.push(second);
                pos += 1;
            }
        }
        if !silent {
            let content = if is_md_ascii_punct(first as u32) { escaped } else { [&[0x5c], &escaped[..]].concat() };
            self.push_content(Kind::TextSpecial, content)?;
        }
        self.pos = pos + 1;
        Ok(true)
    }

    /// rules_inline/backticks.mjs.
    fn backticks(&mut self, silent: bool) -> Result<bool> {
        let mut pos = self.pos;
        if code(self.src, pos) != Some(0x60) { return Ok(false); }
        let start = pos;
        pos += 1;
        let max = self.pos_max;
        while pos < max && self.src[pos] == 0x60 { pos += 1; }
        let opener = pos - start;
        if self.backticks_scanned && self.backticks.get(&opener).copied().unwrap_or(0) <= start {
            if !silent { self.pending.extend_from_slice(&self.src[start..pos]); }
            self.pos += opener;
            return Ok(true);
        }
        let mut match_end = pos;
        while let Some(match_start) = find_unit(self.src, 0x60, match_end) {
            self.meter.work(match_start - match_end + 1)?;
            match_end = match_start + 1;
            while match_end < max && self.src[match_end] == 0x60 { match_end += 1; }
            let closer = match_end - match_start;
            if closer == opener {
                if !silent {
                    let mut content: Units = self.src[pos..match_start].iter().map(|unit| if *unit == 0x0a { 0x20 } else { *unit }).collect();
                    // .replace(/^ (.+) $/, '$1')
                    let length = content.len();
                    if length >= 3 && content[0] == 0x20 && content[length - 1] == 0x20
                        && !content[1..length - 1].iter().any(|unit| is_line_terminator(*unit))
                    {
                        content = content[1..length - 1].to_vec();
                    }
                    let index = self.push(Kind::CodeInline, "code", 0)?;
                    self.meter.alloc(2 * content.len())?;
                    self.tokens[index].content = content;
                }
                self.pos = match_end;
                return Ok(true);
            }
            self.backticks.insert(closer, match_start);
        }
        self.backticks_scanned = true;
        if !silent { self.pending.extend_from_slice(&self.src[start..pos]); }
        self.pos += opener;
        Ok(true)
    }

    /// rules_inline/strikethrough.mjs.
    fn strikethrough(&mut self, silent: bool) -> Result<bool> {
        let start = self.pos;
        if silent || code(self.src, start) != Some(0x7e) { return Ok(false); }
        let scanned = self.scan_delims(start, true);
        let mut length = scanned.length;
        if length < 2 { return Ok(false); }
        if length % 2 == 1 {
            self.push_content(Kind::Text, vec![0x7e])?;
            length -= 1;
        }
        for _ in (0..length).step_by(2) {
            self.push_content(Kind::Text, vec![0x7e, 0x7e])?;
            self.push_delimiter(0x7e, 0, &scanned)?;
        }
        self.pos += scanned.length;
        Ok(true)
    }

    /// rules_inline/emphasis.mjs.
    fn emphasis(&mut self, silent: bool) -> Result<bool> {
        let start = self.pos;
        if silent { return Ok(false); }
        let Some(marker) = code(self.src, start).filter(|unit| *unit == 0x5f || *unit == 0x2a) else { return Ok(false) };
        let scanned = self.scan_delims(start, marker == 0x2a);
        for _ in 0..scanned.length {
            self.push_content(Kind::Text, vec![marker])?;
            self.push_delimiter(marker, scanned.length, &scanned)?;
        }
        self.pos += scanned.length;
        Ok(true)
    }

    fn skip_spaces_and_newlines(&self, mut pos: usize) -> usize {
        while pos < self.pos_max && (is_space(self.src[pos]) || self.src[pos] == 0x0a) { pos += 1; }
        pos
    }

    /// The reference half of `link` and `image`: the target of `[label]`, `[label][]` or `[text][label]`, `None` when
    /// it names no definition.
    fn reference_target(&mut self, mut pos: usize, label_start: usize, label_end: usize) -> Result<Option<(usize, Units, Units)>> {
        let Some(references) = self.references else { return Ok(None) };
        let max = self.pos_max;
        let mut label: &[u16] = &[];
        if pos < max && self.src[pos] == 0x5b {
            let start = pos + 1;
            match self.parse_link_label(pos, false)? {
                Some(end) => {
                    label = &self.src[start..end];
                    pos = end + 1;
                }
                None => pos = label_end + 1,
            }
        } else {
            pos = label_end + 1;
        }
        if label.is_empty() { label = &self.src[label_start..label_end]; }
        let key = normalize_reference(label, self.case, self.meter)?;
        Ok(references.get(&key).map(|reference| (pos, reference.href.clone(), reference.title.clone())))
    }

    /// rules_inline/link.mjs.
    fn link(&mut self, silent: bool) -> Result<bool> {
        if code(self.src, self.pos) != Some(0x5b) { return Ok(false); }
        let old_pos = self.pos;
        let max = self.pos_max;
        let label_start = self.pos + 1;
        let Some(label_end) = self.parse_link_label(self.pos, true)? else { return Ok(false) };
        let mut pos = label_end + 1;
        let mut parse_reference = true;
        let mut href = Units::new();
        let mut title = Units::new();
        if pos < max && self.src[pos] == 0x28 {
            parse_reference = false;
            pos = self.skip_spaces_and_newlines(pos + 1);
            if pos >= max { return Ok(false); }
            let destination = parse_link_destination(self.src, pos, self.pos_max, self.meter)?;
            if destination.ok {
                href = normalize_link(&destination.text, self.meter)?;
                if validate_link(&href) { pos = destination.pos; } else { href.clear(); }
                let start = pos;
                pos = self.skip_spaces_and_newlines(pos);
                let parsed = parse_link_title(self.src, pos, self.pos_max, None, self.meter)?;
                if pos < max && start != pos && parsed.ok {
                    title = parsed.text;
                    pos = self.skip_spaces_and_newlines(parsed.pos);
                }
            }
            if pos >= max || self.src[pos] != 0x29 { parse_reference = true; }
            pos += 1;
        }
        if parse_reference {
            if self.references.is_none() { return Ok(false); }
            let Some((after, target, target_title)) = self.reference_target(pos, label_start, label_end)? else {
                self.pos = old_pos;
                return Ok(false);
            };
            pos = after;
            href = target;
            title = target_title;
        }
        if !silent {
            self.pos = label_start;
            self.pos_max = label_end;
            let index = self.push(Kind::LinkOpen, "a", 1)?;
            let mut attrs = vec![("href", href)];
            if !title.is_empty() { attrs.push(("title", title)); }
            self.charge_attrs(&attrs)?;
            self.tokens[index].attrs = attrs;
            let meter = self.meter;
            let depth = meter.enter()?;
            self.tokenize()?;
            drop(depth);
            self.push(Kind::LinkClose, "a", -1)?;
        }
        self.pos = pos;
        self.pos_max = max;
        Ok(true)
    }

    /// rules_inline/image.mjs.
    fn image(&mut self, silent: bool) -> Result<bool> {
        let old_pos = self.pos;
        let max = self.pos_max;
        if code(self.src, self.pos) != Some(0x21) || code(self.src, self.pos + 1) != Some(0x5b) { return Ok(false); }
        let label_start = self.pos + 2;
        let Some(label_end) = self.parse_link_label(self.pos + 1, false)? else { return Ok(false) };
        let mut pos = label_end + 1;
        let mut href = Units::new();
        let title;
        if pos < max && self.src[pos] == 0x28 {
            pos = self.skip_spaces_and_newlines(pos + 1);
            if pos >= max { return Ok(false); }
            let destination = parse_link_destination(self.src, pos, self.pos_max, self.meter)?;
            if destination.ok {
                href = normalize_link(&destination.text, self.meter)?;
                if validate_link(&href) { pos = destination.pos; } else { href.clear(); }
            }
            let start = pos;
            pos = self.skip_spaces_and_newlines(pos);
            let parsed = parse_link_title(self.src, pos, self.pos_max, None, self.meter)?;
            if pos < max && start != pos && parsed.ok {
                title = parsed.text;
                pos = self.skip_spaces_and_newlines(parsed.pos);
            } else {
                title = Units::new();
            }
            if pos >= max || self.src[pos] != 0x29 {
                self.pos = old_pos;
                return Ok(false);
            }
            pos += 1;
        } else {
            if self.references.is_none() { return Ok(false); }
            let Some((after, target, target_title)) = self.reference_target(pos, label_start, label_end)? else {
                self.pos = old_pos;
                return Ok(false);
            };
            pos = after;
            href = target;
            title = target_title;
        }
        if !silent {
            let content = self.src[label_start..label_end].to_vec();
            let children = parse(&content, self.references, self.case, self.meter)?;
            let index = self.push(Kind::Image, "img", 0)?;
            let mut attrs = vec![("src", href), ("alt", Units::new())];
            if !title.is_empty() { attrs.push(("title", title)); }
            self.charge_attrs(&attrs)?;
            self.meter.alloc(2 * content.len())?;
            let token = &mut self.tokens[index];
            token.attrs = attrs;
            token.children = children;
            token.content = content;
        }
        self.pos = pos;
        self.pos_max = max;
        Ok(true)
    }

    /// rules_inline/autolink.mjs.
    fn autolink(&mut self, silent: bool) -> Result<bool> {
        let mut pos = self.pos;
        if code(self.src, pos) != Some(0x3c) { return Ok(false); }
        let start = self.pos;
        let max = self.pos_max;
        loop {
            pos += 1;
            if pos >= max { return Ok(false); }
            match self.src[pos] {
                0x3c => return Ok(false),
                0x3e => break,
                _ => {}
            }
        }
        let url = &self.src[start + 1..pos];
        let full = if is_autolink(url) {
            url.to_vec()
        } else if is_email(url) {
            [&s("mailto:")[..], url].concat()
        } else {
            return Ok(false);
        };
        let full = normalize_link(&full, self.meter)?;
        if !validate_link(&full) { return Ok(false); }
        if !silent {
            let text = normalize_link_text(url, self.meter)?;
            let open = self.push(Kind::LinkOpen, "a", 1)?;
            let attrs = vec![("href", full)];
            self.charge_attrs(&attrs)?;
            self.tokens[open].attrs = attrs;
            self.tokens[open].auto = true;
            self.push_content(Kind::Text, text)?;
            let close = self.push(Kind::LinkClose, "a", -1)?;
            self.tokens[close].auto = true;
        }
        self.pos += url.len() + 2;
        Ok(true)
    }

    /// rules_inline/html_inline.mjs.
    fn html_inline(&mut self, silent: bool) -> Result<bool> {
        let max = self.pos_max;
        let pos = self.pos;
        if code(self.src, pos) != Some(0x3c) || pos + 2 >= max { return Ok(false); }
        let next = self.src[pos + 1];
        if !matches!(next, 0x21 | 0x3f | 0x2f) && !is_ascii_alpha(next) { return Ok(false); }
        let Some(length) = html::html_tag(&self.src[pos..]) else { return Ok(false) };
        self.meter.work(length)?;
        if !silent { self.push_content(Kind::HtmlInline, self.src[pos..pos + length].to_vec())?; }
        self.pos += length;
        Ok(true)
    }

    /// rules_inline/entity.mjs.
    fn entity(&mut self, silent: bool) -> Result<bool> {
        let pos = self.pos;
        let max = self.pos_max;
        if code(self.src, pos) != Some(0x26) || pos + 1 >= max { return Ok(false); }
        let rest = &self.src[pos..];
        if rest[1] == 0x23 {
            // DIGITAL_RE /^&#((?:x[a-f0-9]{1,6}|[0-9]{1,7}));/i
            let hex = rest.get(2).is_some_and(|unit| (*unit | 0x20) == 0x78);
            let (from, limit) = if hex { (3, 6) } else { (2, 7) };
            let run = rest.get(from..).map_or(0, |tail| tail.iter().take_while(|unit| if hex { is_ascii_hex(**unit) } else { is_ascii_digit(**unit) }).count());
            if (1..=limit).contains(&run) && code(rest, from + run) == Some(0x3b) {
                if !silent {
                    let value = parse_int(&rest[from..from + run], if hex { 16 } else { 10 });
                    let mut content = Units::new();
                    from_code_point(if is_valid_entity_code(value) { value as u32 } else { 0xfffd }, &mut content);
                    self.push_content(Kind::TextSpecial, content)?;
                }
                self.pos += from + run + 1;
                return Ok(true);
            }
        } else if is_ascii_alpha(rest[1]) {
            // NAMED_RE /^&([a-z][a-z0-9]{1,31});/i
            let run = rest[2..].iter().take_while(|unit| is_ascii_alnum(**unit)).count();
            if (1..=31).contains(&run) && code(rest, 2 + run) == Some(0x3b) {
                let matched = &rest[..3 + run];
                let decoded = decode_html(matched, Mode::Strict);
                if decoded != matched {
                    if !silent { self.push_content(Kind::TextSpecial, decoded)?; }
                    self.pos += matched.len();
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

/// AUTOLINK_RE /^([a-zA-Z][a-zA-Z0-9+.-]{1,31}):([^<>\x00-\x20]*)$/
fn is_autolink(url: &[u16]) -> bool {
    if !url.first().is_some_and(|unit| is_ascii_alpha(*unit)) { return false; }
    let run = url[1..].iter().take_while(|unit| is_ascii_alnum(**unit) || matches!(**unit, 0x2b | 0x2e | 0x2d)).count();
    (1..=31).contains(&run) && code(url, 1 + run) == Some(0x3a)
        && url[2 + run..].iter().all(|unit| *unit > 0x20 && *unit != 0x3c && *unit != 0x3e)
}

/// EMAIL_RE (autolink.mjs).
fn is_email(url: &[u16]) -> bool {
    let Some(at) = url.iter().position(|unit| *unit == 0x40) else { return false };
    let local = |unit: u16| is_ascii_alnum(unit) || b".!#$%&'*+/=?^_`{|}~-".contains(&(unit as u8)) && unit < 0x80;
    if at == 0 || !url[..at].iter().all(|unit| local(*unit)) { return false; }
    url[at + 1..].split(|unit| *unit == 0x2e).all(|label| {
        !label.is_empty() && label.len() <= 63 && is_ascii_alnum(label[0]) && is_ascii_alnum(label[label.len() - 1])
            && label.iter().all(|unit| is_ascii_alnum(*unit) || *unit == 0x2d)
    })
}

/// `processDelimiters` (rules_inline/balance_pairs.mjs).
fn process_delimiters(delimiters: &mut [Delimiter]) {
    let max = delimiters.len();
    if max == 0 { return; }
    let mut openers_bottom: HashMap<u16, [isize; 6]> = HashMap::new();
    let mut header = 0usize;
    let mut last_token: isize = -2;
    let mut jumps: Vec<isize> = Vec::with_capacity(max);
    for closer_index in 0..max {
        jumps.push(0);
        let closer = delimiters[closer_index];
        if delimiters[header].marker != closer.marker || last_token != closer.token as isize - 1 { header = closer_index; }
        last_token = closer.token as isize;
        if !closer.close { continue; }
        let slot = usize::from(closer.open) * 3 + closer.length % 3;
        let min_opener = openers_bottom.entry(closer.marker).or_insert([-1; 6])[slot];
        let mut opener_index = header as isize - jumps[header] - 1;
        let mut new_min_opener = opener_index;
        while opener_index > min_opener {
            let opener = delimiters[opener_index as usize];
            if opener.marker == closer.marker && opener.open && opener.end < 0 {
                let odd = (opener.close || closer.open) && (opener.length + closer.length) % 3 == 0
                    && (opener.length % 3 != 0 || closer.length % 3 != 0);
                if !odd {
                    let last_jump = if opener_index > 0 && !delimiters[opener_index as usize - 1].open { jumps[opener_index as usize - 1] + 1 } else { 0 };
                    jumps[closer_index] = closer_index as isize - opener_index + last_jump;
                    jumps[opener_index as usize] = last_jump;
                    delimiters[closer_index].open = false;
                    delimiters[opener_index as usize].end = closer_index as isize;
                    delimiters[opener_index as usize].close = false;
                    new_min_opener = -1;
                    last_token = -2;
                    break;
                }
            }
            opener_index -= jumps[opener_index as usize] + 1;
        }
        if new_min_opener != -1 {
            let slot = usize::from(delimiters[closer_index].open) * 3 + delimiters[closer_index].length % 3;
            openers_bottom.entry(closer.marker).or_insert([-1; 6])[slot] = new_min_opener;
        }
    }
}

/// `postProcess` of rules_inline/strikethrough.mjs.
fn strikethrough_post_process(tokens: &mut [Token], delimiters: &[Delimiter]) {
    let mut lone_markers = Vec::new();
    for start in delimiters {
        if start.marker != 0x7e || start.end == -1 { continue; }
        let end = delimiters[start.end as usize];
        let open = &mut tokens[start.token];
        open.kind = Kind::Generic;
        open.tag = "s";
        open.nesting = 1;
        open.content.clear();
        let close = &mut tokens[end.token];
        close.kind = Kind::SClose;
        close.tag = "s";
        close.nesting = -1;
        close.content.clear();
        let before = &tokens[end.token - 1];
        if before.kind == Kind::Text && before.content == [0x7e] { lone_markers.push(end.token - 1); }
    }
    while let Some(index) = lone_markers.pop() {
        let mut swap = index + 1;
        while swap < tokens.len() && tokens[swap].kind == Kind::SClose { swap += 1; }
        swap -= 1;
        if index != swap { tokens.swap(index, swap); }
    }
}

/// `postProcess` of rules_inline/emphasis.mjs.
fn emphasis_post_process(tokens: &mut [Token], delimiters: &[Delimiter]) {
    let mut index = delimiters.len() as isize - 1;
    while index >= 0 {
        let start = delimiters[index as usize];
        if (start.marker != 0x5f && start.marker != 0x2a) || start.end == -1 {
            index -= 1;
            continue;
        }
        let end = delimiters[start.end as usize];
        let strong = index > 0 && {
            let previous = delimiters[index as usize - 1];
            previous.end == start.end + 1 && previous.marker == start.marker && previous.token + 1 == start.token
                && delimiters[start.end as usize + 1].token == end.token + 1
        };
        let tag = if strong { "strong" } else { "em" };
        for (token, nesting) in [(start.token, 1), (end.token, -1)] {
            let token = &mut tokens[token];
            token.kind = Kind::Generic;
            token.tag = tag;
            token.nesting = nesting;
            token.content.clear();
        }
        if strong {
            tokens[delimiters[index as usize - 1].token].content.clear();
            tokens[delimiters[start.end as usize + 1].token].content.clear();
            index -= 1;
        }
        index -= 1;
    }
}

/// rules_inline/fragments_join.mjs: recomputes levels and joins adjacent text tokens.
fn fragments_join(tokens: &mut Vec<Token>) {
    let mut level = 0;
    let max = tokens.len();
    let mut last = 0;
    for current in 0..max {
        if tokens[current].nesting < 0 { level -= 1; }
        tokens[current].level = level;
        if tokens[current].nesting > 0 { level += 1; }
        if tokens[current].kind == Kind::Text && current + 1 < max && tokens[current + 1].kind == Kind::Text {
            let mut joined = std::mem::take(&mut tokens[current].content);
            joined.extend_from_slice(&tokens[current + 1].content);
            tokens[current + 1].content = joined;
        } else {
            if current != last { tokens.swap(last, current); }
            last += 1;
        }
    }
    tokens.truncate(last);
}
