//! The block parser of markdown-it 14.3.2: `lib/rules_block/state_block.mjs`, `lib/parser_block.mjs` and the block
//! rules the `default` preset enables, minus `code` (`md.disable(['code'])`).

use crate::error::{invariant, Result};
use crate::html;
use crate::js_engine::UnicodeVersion;
use crate::links::{parse_link_destination, parse_link_title};
use crate::meter::Meter;
use crate::token::{Kind, Token};
use crate::url::{normalize_link, validate_link};
use crate::utils::{ascii_trim, code, is_space, is_space_at, js_trim, normalize_reference, s, Units};
use std::collections::HashMap;

/// `maxNesting` of the default preset.
pub(crate) const MAX_NESTING: i32 = 100;

pub(crate) struct Reference {
    pub(crate) href: Units,
    pub(crate) title: Units,
}

/// `env.references`: `None` until the first reference definition, as `typeof state.env.references === 'undefined'`.
pub(crate) type References = Option<HashMap<Units, Reference>>;

/// `ParserBlock.prototype.parse` (parser_block.mjs) for a nonempty source: the block tokens and `env.references`.
pub(crate) fn parse(src: &[u16], case: Option<UnicodeVersion>, meter: &Meter) -> Result<(Vec<Token>, References)> {
    let mut state = BlockState::new(src, case, meter)?;
    let (start, end) = (state.line, state.line_max);
    state.tokenize(start, end)?;
    Ok((state.tokens, state.references))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent { Root, Paragraph, Blockquote, List, Reference, Table }

#[derive(Clone, Copy)]
enum Rule { Table, Fence, Blockquote, Hr, List, Reference, HtmlBlock, Heading, Lheading, Paragraph }

/// `ruler.getRules('')` with `code` disabled (parser_block.mjs).
const RULES: [Rule; 10] = [
    Rule::Table, Rule::Fence, Rule::Blockquote, Rule::Hr, Rule::List, Rule::Reference, Rule::HtmlBlock, Rule::Heading,
    Rule::Lheading, Rule::Paragraph,
];
/// `getRules('paragraph')`, which equals `getRules('reference')`.
const PARAGRAPH_CHAIN: [Rule; 7] = [Rule::Table, Rule::Fence, Rule::Blockquote, Rule::Hr, Rule::List, Rule::HtmlBlock, Rule::Heading];
const BLOCKQUOTE_CHAIN: [Rule; 6] = [Rule::Fence, Rule::Blockquote, Rule::Hr, Rule::List, Rule::HtmlBlock, Rule::Heading];
const LIST_CHAIN: [Rule; 3] = [Rule::Fence, Rule::Blockquote, Rule::Hr];

struct BlockState<'s, 'm, 'b> {
    src: &'s [u16],
    /// The engine's case tables, for reference labels.
    case: Option<UnicodeVersion>,
    meter: &'m Meter<'b>,
    references: References,
    tokens: Vec<Token>,
    b_marks: Vec<usize>,
    e_marks: Vec<usize>,
    t_shift: Vec<usize>,
    s_count: Vec<isize>,
    bs_count: Vec<isize>,
    blk_indent: isize,
    line: usize,
    line_max: usize,
    tight: bool,
    list_indent: isize,
    parent: Parent,
    level: i32,
    /// Tokens already charged to the meter.
    charged: usize,
}

impl<'s, 'm, 'b> BlockState<'s, 'm, 'b> {
    /// `StateBlock` (state_block.mjs).
    fn new(src: &'s [u16], case: Option<UnicodeVersion>, meter: &'m Meter<'b>) -> Result<Self> {
        meter.work(src.len())?;
        let mut state = Self {
            src, case, meter, references: None, tokens: Vec::new(), b_marks: Vec::new(), e_marks: Vec::new(), t_shift: Vec::new(),
            s_count: Vec::new(), bs_count: Vec::new(), blk_indent: 0, line: 0, line_max: 0, tight: false, list_indent: -1,
            parent: Parent::Root, level: 0, charged: 0,
        };
        let (mut start, mut indent, mut offset, mut indent_found) = (0usize, 0usize, 0isize, false);
        let len = src.len();
        let mut pos = 0;
        while pos < len {
            let unit = src[pos];
            if !indent_found {
                if is_space(unit) {
                    indent += 1;
                    if unit == 0x09 { offset += 4 - offset % 4; } else { offset += 1; }
                    pos += 1;
                    continue;
                }
                indent_found = true;
            }
            if unit == 0x0a || pos == len - 1 {
                if unit != 0x0a { pos += 1; }
                state.b_marks.push(start);
                state.e_marks.push(pos);
                state.t_shift.push(indent);
                state.s_count.push(offset);
                state.bs_count.push(0);
                indent_found = false;
                indent = 0;
                offset = 0;
                start = pos + 1;
            }
            pos += 1;
        }
        state.b_marks.push(len);
        state.e_marks.push(len);
        state.t_shift.push(0);
        state.s_count.push(0);
        state.bs_count.push(0);
        state.line_max = state.b_marks.len() - 1;
        meter.alloc(state.b_marks.len() * 40)?;
        Ok(state)
    }

    fn push(&mut self, kind: Kind, tag: &'static str, nesting: i8) -> &mut Token {
        let mut token = Token::new(kind, tag, nesting);
        token.block = true;
        if nesting < 0 { self.level -= 1; }
        token.level = self.level;
        if nesting > 0 { self.level += 1; }
        self.tokens.push(token);
        self.tokens.last_mut().expect("pushed")
    }

    fn is_empty(&self, line: usize) -> bool { self.b_marks[line] + self.t_shift[line] >= self.e_marks[line] }

    fn skip_empty_lines(&self, mut from: usize) -> usize {
        while from < self.line_max {
            if self.b_marks[from] + self.t_shift[from] < self.e_marks[from] { break; }
            from += 1;
        }
        from
    }

    fn skip_spaces(&self, mut pos: usize) -> usize {
        while pos < self.src.len() && is_space(self.src[pos]) { pos += 1; }
        pos
    }

    fn skip_spaces_back(&self, mut pos: usize, min: usize) -> usize {
        if pos <= min { return pos; }
        while pos > min {
            pos -= 1;
            if !is_space(self.src[pos]) { return pos + 1; }
        }
        pos
    }

    fn skip_chars(&self, mut pos: usize, unit: u16) -> usize {
        while pos < self.src.len() && self.src[pos] == unit { pos += 1; }
        pos
    }

    fn skip_chars_back(&self, mut pos: usize, unit: u16, min: usize) -> usize {
        if pos <= min { return pos; }
        while pos > min {
            pos -= 1;
            if self.src[pos] != unit { return pos + 1; }
        }
        pos
    }

    /// `getLines` (state_block.mjs).
    fn get_lines(&self, begin: usize, end: usize, indent: isize, keep_last_lf: bool) -> Result<Units> {
        let mut out = Units::new();
        if begin >= end { return Ok(out); }
        for line in begin..end {
            let mut line_indent: isize = 0;
            let line_start = self.b_marks[line];
            let mut first = line_start;
            let last = if line + 1 < end || keep_last_lf { self.e_marks[line] + 1 } else { self.e_marks[line] };
            while first < last && line_indent < indent {
                match code(self.src, first) {
                    Some(unit) if is_space(unit) => {
                        if unit == 0x09 { line_indent += 4 - (line_indent + self.bs_count[line]) % 4; } else { line_indent += 1; }
                    }
                    _ if first - line_start < self.t_shift[line] => line_indent += 1,
                    _ => break,
                }
                first += 1;
            }
            if line_indent > indent {
                out.extend(std::iter::repeat_n(0x20, (line_indent - indent) as usize));
            }
            let len = self.src.len();
            out.extend_from_slice(&self.src[first.min(len)..last.min(len)]);
        }
        self.meter.work(out.len())?;
        self.meter.text(out.len())?;
        Ok(out)
    }

    /// Charges the tokens pushed since the last charge.
    fn charge_tokens(&mut self) -> Result<()> {
        let weight: usize = self.tokens[self.charged..].iter().map(Token::weight).sum();
        self.charged = self.tokens.len();
        self.meter.alloc(weight)
    }

    fn line_text(&self, line: usize) -> &'s [u16] {
        let src: &'s [u16] = self.src;
        &src[self.b_marks[line] + self.t_shift[line]..self.e_marks[line]]
    }

    fn run(&mut self, rule: Rule, start: usize, end: usize, silent: bool) -> Result<bool> {
        self.meter.work(1)?;
        match rule {
            Rule::Table => self.table(start, end, silent),
            Rule::Fence => self.fence(start, end, silent),
            Rule::Blockquote => self.blockquote(start, end, silent),
            Rule::Hr => self.hr(start, silent),
            Rule::List => self.list(start, end, silent),
            Rule::Reference => self.reference(start, silent),
            Rule::HtmlBlock => self.html_block(start, end, silent),
            Rule::Heading => self.heading(start, silent),
            Rule::Lheading => self.lheading(start, end),
            Rule::Paragraph => self.paragraph(start, end),
        }
    }

    fn terminates(&mut self, chain: &[Rule], line: usize, end: usize) -> Result<bool> {
        for rule in chain {
            if self.run(*rule, line, end, true)? { return Ok(true); }
        }
        Ok(false)
    }

    /// `ParserBlock.prototype.tokenize` (parser_block.mjs).
    fn tokenize(&mut self, start: usize, end: usize) -> Result<()> {
        let meter = self.meter;
        let _depth = meter.enter()?;
        let mut line = start;
        let mut has_empty_lines = false;
        while line < end {
            line = self.skip_empty_lines(line);
            self.line = line;
            if line >= end { break; }
            if self.s_count[line] < self.blk_indent { break; }
            if self.level >= MAX_NESTING {
                self.line = end;
                break;
            }
            let previous = self.line;
            let mut ok = false;
            for rule in RULES {
                ok = self.run(rule, line, end, false)?;
                if ok {
                    if previous >= self.line { return Err(invariant()); }
                    break;
                }
            }
            if !ok { return Err(invariant()); }
            self.charge_tokens()?;
            self.tight = !has_empty_lines;
            if self.is_empty(self.line - 1) { has_empty_lines = true; }
            line = self.line;
            if line < end && self.is_empty(line) {
                has_empty_lines = true;
                line += 1;
                self.line = line;
            }
        }
        Ok(())
    }

    /// rules_block/table.mjs.
    fn table(&mut self, start: usize, end: usize, silent: bool) -> Result<bool> {
        if start + 2 > end { return Ok(false); }
        let mut next = start + 1;
        if self.s_count[next] < self.blk_indent || self.s_count[next] - self.blk_indent >= 4 { return Ok(false); }
        let mut pos = self.b_marks[next] + self.t_shift[next];
        if pos >= self.e_marks[next] { return Ok(false); }
        let delimiter = |unit: u16| matches!(unit, 0x7c | 0x2d | 0x3a);
        let first = self.src[pos];
        pos += 1;
        if !delimiter(first) || pos >= self.e_marks[next] { return Ok(false); }
        let second = self.src[pos];
        pos += 1;
        if !delimiter(second) && !is_space(second) { return Ok(false); }
        if first == 0x2d && is_space(second) { return Ok(false); }
        while pos < self.e_marks[next] {
            let unit = self.src[pos];
            if !delimiter(unit) && !is_space(unit) { return Ok(false); }
            pos += 1;
        }
        let line_text = self.line_text(start + 1);
        let columns: Vec<&[u16]> = line_text.split(|unit| *unit == 0x7c).collect();
        let mut aligns: Vec<&'static str> = Vec::new();
        for (index, column) in columns.iter().enumerate() {
            let text = js_trim(column);
            if text.is_empty() {
                if index == 0 || index == columns.len() - 1 { continue; }
                return Ok(false);
            }
            // /^:?-+:?$/
            let lead = usize::from(text[0] == 0x3a);
            let dashes = text[lead..].iter().take_while(|unit| **unit == 0x2d).count();
            let tail = lead + dashes + usize::from(text.get(lead + dashes) == Some(&0x3a));
            if dashes == 0 || tail != text.len() { return Ok(false); }
            aligns.push(if text[text.len() - 1] == 0x3a {
                if text[0] == 0x3a { "center" } else { "right" }
            } else if text[0] == 0x3a { "left" } else { "" });
        }
        let header = js_trim(self.line_text(start));
        if !header.contains(&0x7c) { return Ok(false); }
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        let mut columns = escaped_split(header);
        trim_outer_empty(&mut columns);
        let column_count = columns.len();
        if column_count == 0 || column_count != aligns.len() { return Ok(false); }
        if silent { return Ok(true); }
        let old_parent = self.parent;
        self.parent = Parent::Table;
        let style = |align: &str| (!align.is_empty()).then(|| vec![("style", s(&format!("text-align:{align}")))]).unwrap_or_default();
        self.push(Kind::Generic, "table", 1);
        self.push(Kind::Generic, "thead", 1);
        self.push(Kind::Generic, "tr", 1);
        for (index, column) in columns.iter().enumerate() {
            self.push(Kind::Generic, "th", 1).attrs = style(aligns[index]);
            self.push(Kind::Inline, "", 0).content = js_trim(column).to_vec();
            self.push(Kind::Generic, "th", -1);
        }
        self.push(Kind::Generic, "tr", -1);
        self.push(Kind::Generic, "thead", -1);
        let mut tbody = false;
        let mut autocompleted: isize = 0;
        next = start + 2;
        while next < end {
            if self.s_count[next] < self.blk_indent { break; }
            if self.terminates(&BLOCKQUOTE_CHAIN, next, end)? { break; }
            let line_text = js_trim(self.line_text(next));
            if line_text.is_empty() { break; }
            if self.s_count[next] - self.blk_indent >= 4 { break; }
            let mut columns = escaped_split(line_text);
            trim_outer_empty(&mut columns);
            autocompleted += column_count as isize - columns.len() as isize;
            if autocompleted > 0x10000 { break; }
            if next == start + 2 {
                self.push(Kind::Generic, "tbody", 1);
                tbody = true;
            }
            self.push(Kind::Generic, "tr", 1);
            for (index, align) in aligns.iter().enumerate().take(column_count) {
                self.push(Kind::Generic, "td", 1).attrs = style(align);
                let content = columns.get(index).map(|column| js_trim(column).to_vec()).unwrap_or_default();
                self.push(Kind::Inline, "", 0).content = content;
                self.push(Kind::Generic, "td", -1);
            }
            self.push(Kind::Generic, "tr", -1);
            self.meter.work(column_count)?;
            self.charge_tokens()?;
            next += 1;
        }
        if tbody { self.push(Kind::Generic, "tbody", -1); }
        self.push(Kind::Generic, "table", -1);
        self.parent = old_parent;
        self.line = next;
        Ok(true)
    }

    /// rules_block/fence.mjs.
    fn fence(&mut self, start: usize, end: usize, silent: bool) -> Result<bool> {
        let mut pos = self.b_marks[start] + self.t_shift[start];
        let mut max = self.e_marks[start];
        if self.s_count[start] - self.blk_indent >= 4 || pos + 3 > max { return Ok(false); }
        let marker = self.src[pos];
        if marker != 0x7e && marker != 0x60 { return Ok(false); }
        let mut mem = pos;
        pos = self.skip_chars(pos, marker);
        let len = pos - mem;
        if len < 3 { return Ok(false); }
        let params = &self.src[pos..max];
        if marker == 0x60 && params.contains(&0x60) { return Ok(false); }
        if silent { return Ok(true); }
        let params = params.to_vec();
        let mut next = start;
        let mut have_end_marker = false;
        loop {
            next += 1;
            if next >= end { break; }
            pos = self.b_marks[next] + self.t_shift[next];
            mem = pos;
            max = self.e_marks[next];
            if pos < max && self.s_count[next] < self.blk_indent { break; }
            if code(self.src, pos) != Some(marker) { continue; }
            if self.s_count[next] - self.blk_indent >= 4 { continue; }
            pos = self.skip_chars(pos, marker);
            if pos - mem < len { continue; }
            pos = self.skip_spaces(pos);
            if pos < max { continue; }
            have_end_marker = true;
            break;
        }
        let indent = self.s_count[start];
        self.line = next + usize::from(have_end_marker);
        let content = self.get_lines(start + 1, next, indent, true)?;
        let token = self.push(Kind::Fence, "code", 0);
        token.info = params;
        token.content = content;
        Ok(true)
    }

    /// rules_block/blockquote.mjs.
    fn blockquote(&mut self, start: usize, end: usize, silent: bool) -> Result<bool> {
        let mut pos = self.b_marks[start] + self.t_shift[start];
        let old_line_max = self.line_max;
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        if code(self.src, pos) != Some(0x3e) { return Ok(false); }
        if silent { return Ok(true); }
        let (mut old_b_marks, mut old_bs_count, mut old_s_count, mut old_t_shift) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let old_parent = self.parent;
        self.parent = Parent::Blockquote;
        let mut last_line_empty = false;
        let mut next = start;
        while next < end {
            let is_outdented = self.s_count[next] < self.blk_indent;
            pos = self.b_marks[next] + self.t_shift[next];
            let max = self.e_marks[next];
            if pos >= max { break; }
            let first = self.src[pos];
            pos += 1;
            if first == 0x3e && !is_outdented {
                let mut initial = self.s_count[next] + 1;
                let (space_after_marker, adjust_tab);
                match code(self.src, pos) {
                    Some(0x20) => {
                        pos += 1;
                        initial += 1;
                        adjust_tab = false;
                        space_after_marker = true;
                    }
                    Some(0x09) => {
                        space_after_marker = true;
                        if (self.bs_count[next] + initial) % 4 == 3 {
                            pos += 1;
                            initial += 1;
                            adjust_tab = false;
                        } else {
                            adjust_tab = true;
                        }
                    }
                    _ => {
                        space_after_marker = false;
                        adjust_tab = false;
                    }
                }
                let mut offset = initial;
                old_b_marks.push(self.b_marks[next]);
                self.b_marks[next] = pos;
                while pos < max {
                    let unit = self.src[pos];
                    if !is_space(unit) { break; }
                    if unit == 0x09 {
                        offset += 4 - (offset + self.bs_count[next] + isize::from(adjust_tab)) % 4;
                    } else {
                        offset += 1;
                    }
                    pos += 1;
                }
                last_line_empty = pos >= max;
                old_bs_count.push(self.bs_count[next]);
                self.bs_count[next] = self.s_count[next] + 1 + isize::from(space_after_marker);
                old_s_count.push(self.s_count[next]);
                self.s_count[next] = offset - initial;
                old_t_shift.push(self.t_shift[next]);
                self.t_shift[next] = pos - self.b_marks[next];
                next += 1;
                continue;
            }
            if last_line_empty { break; }
            if self.terminates(&BLOCKQUOTE_CHAIN, next, end)? {
                self.line_max = next;
                if self.blk_indent != 0 {
                    old_b_marks.push(self.b_marks[next]);
                    old_bs_count.push(self.bs_count[next]);
                    old_t_shift.push(self.t_shift[next]);
                    old_s_count.push(self.s_count[next]);
                    self.s_count[next] -= self.blk_indent;
                }
                break;
            }
            old_b_marks.push(self.b_marks[next]);
            old_bs_count.push(self.bs_count[next]);
            old_t_shift.push(self.t_shift[next]);
            old_s_count.push(self.s_count[next]);
            self.s_count[next] = -1;
            next += 1;
        }
        let old_indent = self.blk_indent;
        self.blk_indent = 0;
        self.push(Kind::Generic, "blockquote", 1);
        self.tokenize(start, next)?;
        self.push(Kind::Generic, "blockquote", -1);
        self.line_max = old_line_max;
        self.parent = old_parent;
        for index in 0..old_t_shift.len() {
            self.b_marks[index + start] = old_b_marks[index];
            self.t_shift[index + start] = old_t_shift[index];
            self.s_count[index + start] = old_s_count[index];
            self.bs_count[index + start] = old_bs_count[index];
        }
        self.blk_indent = old_indent;
        Ok(true)
    }

    /// rules_block/hr.mjs.
    fn hr(&mut self, start: usize, silent: bool) -> Result<bool> {
        let max = self.e_marks[start];
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        let mut pos = self.b_marks[start] + self.t_shift[start];
        let Some(marker) = code(self.src, pos).filter(|unit| matches!(unit, 0x2a | 0x2d | 0x5f)) else { return Ok(false) };
        pos += 1;
        let mut count = 1;
        while pos < max {
            let unit = self.src[pos];
            pos += 1;
            if unit != marker && !is_space(unit) { return Ok(false); }
            if unit == marker { count += 1; }
        }
        if count < 3 { return Ok(false); }
        if silent { return Ok(true); }
        self.line = start + 1;
        self.push(Kind::Generic, "hr", 0);
        Ok(true)
    }

    /// `skipBulletListMarker` (list.mjs).
    fn skip_bullet_marker(&self, line: usize) -> Option<usize> {
        let max = self.e_marks[line];
        let pos = self.b_marks[line] + self.t_shift[line];
        if !code(self.src, pos).is_some_and(|unit| matches!(unit, 0x2a | 0x2d | 0x2b)) { return None; }
        let pos = pos + 1;
        if pos < max && !is_space(self.src[pos]) { return None; }
        Some(pos)
    }

    /// `skipOrderedListMarker` (list.mjs).
    fn skip_ordered_marker(&self, line: usize) -> Option<usize> {
        let start = self.b_marks[line] + self.t_shift[line];
        let max = self.e_marks[line];
        let mut pos = start;
        if pos + 1 >= max { return None; }
        let unit = self.src[pos];
        pos += 1;
        if !(0x30..=0x39).contains(&unit) { return None; }
        loop {
            if pos >= max { return None; }
            let unit = self.src[pos];
            pos += 1;
            if (0x30..=0x39).contains(&unit) {
                if pos - start >= 10 { return None; }
                continue;
            }
            if unit == 0x29 || unit == 0x2e { break; }
            return None;
        }
        if pos < max && !is_space(self.src[pos]) { return None; }
        Some(pos)
    }

    /// `markTightParagraphs` (list.mjs).
    fn mark_tight_paragraphs(&mut self, index: usize) {
        let level = self.level + 2;
        let mut at = index + 2;
        let limit = self.tokens.len().saturating_sub(2);
        while at < limit {
            if self.tokens[at].level == level && self.tokens[at].kind == Kind::ParagraphOpen {
                self.tokens[at + 2].hidden = true;
                self.tokens[at].hidden = true;
                at += 2;
            }
            at += 1;
        }
    }

    /// rules_block/list.mjs.
    fn list(&mut self, start: usize, end: usize, silent: bool) -> Result<bool> {
        let mut next = start;
        let mut tight = true;
        if self.s_count[next] - self.blk_indent >= 4 { return Ok(false); }
        if self.list_indent >= 0 && self.s_count[next] - self.list_indent >= 4 && self.s_count[next] < self.blk_indent {
            return Ok(false);
        }
        let terminating_paragraph = silent && self.parent == Parent::Paragraph && self.s_count[next] >= self.blk_indent;
        let mut marker_value = 0u64;
        let (ordered, mut after_marker) = if let Some(after) = self.skip_ordered_marker(next) {
            let marker_start = self.b_marks[next] + self.t_shift[next];
            marker_value = self.src[marker_start..after - 1].iter().fold(0u64, |value, unit| value * 10 + (unit - 0x30) as u64);
            if terminating_paragraph && marker_value != 1 { return Ok(false); }
            (true, after)
        } else if let Some(after) = self.skip_bullet_marker(next) {
            (false, after)
        } else {
            return Ok(false);
        };
        if terminating_paragraph && self.skip_spaces(after_marker) >= self.e_marks[next] { return Ok(false); }
        if silent { return Ok(true); }
        let marker = self.src[after_marker - 1];
        let list_token = self.tokens.len();
        if ordered {
            let token = self.push(Kind::Generic, "ol", 1);
            if marker_value != 1 { token.attrs = vec![("start", s(&marker_value.to_string()))]; }
        } else {
            self.push(Kind::Generic, "ul", 1);
        }
        let mut previous_empty_end = false;
        let old_parent = self.parent;
        self.parent = Parent::List;
        while next < end {
            let mut pos = after_marker;
            let max = self.e_marks[next];
            let initial = self.s_count[next] + after_marker as isize - (self.b_marks[next] + self.t_shift[next]) as isize;
            let mut offset = initial;
            while pos < max {
                match self.src[pos] {
                    0x09 => offset += 4 - (offset + self.bs_count[next]) % 4,
                    0x20 => offset += 1,
                    _ => break,
                }
                pos += 1;
            }
            let content_start = pos;
            let mut indent_after_marker = if content_start >= max { 1 } else { offset - initial };
            if indent_after_marker > 4 { indent_after_marker = 1; }
            let indent = initial + indent_after_marker;
            self.push(Kind::Generic, "li", 1);
            let old_tight = self.tight;
            let old_t_shift = self.t_shift[next];
            let old_s_count = self.s_count[next];
            let old_list_indent = self.list_indent;
            self.list_indent = self.blk_indent;
            self.blk_indent = indent;
            self.tight = true;
            self.t_shift[next] = content_start - self.b_marks[next];
            self.s_count[next] = offset;
            if content_start >= max && self.is_empty(next + 1) {
                self.line = (self.line + 2).min(end);
            } else {
                self.tokenize(next, end)?;
            }
            if !self.tight || previous_empty_end { tight = false; }
            previous_empty_end = self.line - next > 1 && self.is_empty(self.line - 1);
            self.blk_indent = self.list_indent;
            self.list_indent = old_list_indent;
            self.t_shift[next] = old_t_shift;
            self.s_count[next] = old_s_count;
            self.tight = old_tight;
            self.push(Kind::Generic, "li", -1);
            next = self.line;
            if next >= end { break; }
            if self.s_count[next] < self.blk_indent { break; }
            if self.s_count[next] - self.blk_indent >= 4 { break; }
            if self.terminates(&LIST_CHAIN, next, end)? { break; }
            let after = if ordered { self.skip_ordered_marker(next) } else { self.skip_bullet_marker(next) };
            let Some(after) = after else { break };
            after_marker = after;
            if marker != self.src[after_marker - 1] { break; }
        }
        self.push(Kind::Generic, if ordered { "ol" } else { "ul" }, -1);
        self.line = next;
        self.parent = old_parent;
        if tight { self.mark_tight_paragraphs(list_token); }
        Ok(true)
    }

    /// `getNextLine` inside rules_block/reference.mjs.
    fn reference_next_line(&mut self, next: usize) -> Result<Option<&'s [u16]>> {
        let end = self.line_max;
        if next >= end || self.is_empty(next) { return Ok(None); }
        let continuation = self.s_count[next] - self.blk_indent > 3 || self.s_count[next] < 0;
        if !continuation {
            let old_parent = self.parent;
            self.parent = Parent::Reference;
            let terminate = self.terminates(&PARAGRAPH_CHAIN, next, end);
            self.parent = old_parent;
            if terminate? { return Ok(None); }
        }
        let src: &'s [u16] = self.src;
        let from = self.b_marks[next] + self.t_shift[next];
        Ok(Some(&src[from..(self.e_marks[next] + 1).min(src.len())]))
    }

    /// rules_block/reference.mjs.
    fn reference(&mut self, start: usize, silent: bool) -> Result<bool> {
        let mut pos = self.b_marks[start] + self.t_shift[start];
        let max = self.e_marks[start];
        let mut next = start + 1;
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        if code(self.src, pos) != Some(0x5b) { return Ok(false); }
        let mut text: Units = self.src[pos..(max + 1).min(self.src.len())].to_vec();
        let mut max = text.len();
        let mut label_end = None;
        pos = 1;
        while pos < max {
            let unit = text[pos];
            if unit == 0x5b { return Ok(false); }
            if unit == 0x5d {
                label_end = Some(pos);
                break;
            }
            if unit == 0x0a {
                if let Some(line) = self.reference_next_line(next)? {
                    text.extend_from_slice(line);
                    max = text.len();
                    next += 1;
                }
            } else if unit == 0x5c {
                pos += 1;
                if pos < max && text[pos] == 0x0a {
                    if let Some(line) = self.reference_next_line(next)? {
                        text.extend_from_slice(line);
                        max = text.len();
                        next += 1;
                    }
                }
            }
            pos += 1;
        }
        let Some(label_end) = label_end else { return Ok(false) };
        if code(&text, label_end + 1) != Some(0x3a) { return Ok(false); }
        pos = label_end + 2;
        while pos < max {
            let unit = text[pos];
            if unit == 0x0a {
                if let Some(line) = self.reference_next_line(next)? {
                    text.extend_from_slice(line);
                    max = text.len();
                    next += 1;
                }
            } else if !is_space(unit) {
                break;
            }
            pos += 1;
        }
        let destination = parse_link_destination(&text, pos, max, self.meter)?;
        if !destination.ok { return Ok(false); }
        let href = normalize_link(&destination.text, self.meter)?;
        if !validate_link(&href) { return Ok(false); }
        pos = destination.pos;
        let destination_end = pos;
        let destination_end_line = next;
        let title_start = pos;
        while pos < max {
            let unit = text[pos];
            if unit == 0x0a {
                if let Some(line) = self.reference_next_line(next)? {
                    text.extend_from_slice(line);
                    max = text.len();
                    next += 1;
                }
            } else if !is_space(unit) {
                break;
            }
            pos += 1;
        }
        let mut title_result = parse_link_title(&text, pos, max, None, self.meter)?;
        while title_result.can_continue {
            let Some(line) = self.reference_next_line(next)? else { break };
            text.extend_from_slice(line);
            pos = max;
            max = text.len();
            next += 1;
            title_result = parse_link_title(&text, pos, max, Some(title_result), self.meter)?;
        }
        let mut title;
        if pos < max && title_start != pos && title_result.ok {
            title = title_result.text;
            pos = title_result.pos;
        } else {
            title = Units::new();
            pos = destination_end;
            next = destination_end_line;
        }
        while pos < max && is_space(text[pos]) { pos += 1; }
        if pos < max && text[pos] != 0x0a && !title.is_empty() {
            title.clear();
            pos = destination_end;
            next = destination_end_line;
            while pos < max && is_space(text[pos]) { pos += 1; }
        }
        if pos < max && text[pos] != 0x0a { return Ok(false); }
        let label = normalize_reference(&text[1..label_end], self.case, self.meter)?;
        if label.is_empty() { return Ok(false); }
        if silent { return Ok(true); }
        self.references.get_or_insert_with(HashMap::new).entry(label).or_insert(Reference { href, title });
        self.line = next;
        Ok(true)
    }

    /// rules_block/html_block.mjs.
    fn html_block(&mut self, start: usize, end: usize, silent: bool) -> Result<bool> {
        let pos = self.b_marks[start] + self.t_shift[start];
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        if code(self.src, pos) != Some(0x3c) { return Ok(false); }
        let line_text = self.line_text(start);
        self.meter.work(line_text.len())?;
        let Some(kind) = html::block_start(line_text) else { return Ok(false) };
        if silent { return Ok(html::block_interrupts(kind)); }
        let mut next = start + 1;
        if !html::block_end(kind, line_text) {
            while next < end {
                if self.s_count[next] < self.blk_indent && (html::block_ends_on_blank(kind) || !self.is_empty(next)) { break; }
                let line_text = self.line_text(next);
                self.meter.work(line_text.len())?;
                if html::block_end(kind, line_text) {
                    if !line_text.is_empty() { next += 1; }
                    break;
                }
                next += 1;
            }
        }
        self.line = next;
        let content = self.get_lines(start, next, self.blk_indent, true)?;
        self.push(Kind::HtmlBlock, "", 0).content = content;
        Ok(true)
    }

    /// rules_block/heading.mjs.
    fn heading(&mut self, start: usize, silent: bool) -> Result<bool> {
        let mut pos = self.b_marks[start] + self.t_shift[start];
        let mut max = self.e_marks[start];
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        let mut unit = code(self.src, pos);
        if unit != Some(0x23) || pos >= max { return Ok(false); }
        let mut level = 1;
        pos += 1;
        unit = code(self.src, pos);
        while unit == Some(0x23) && pos < max && level <= 6 {
            level += 1;
            pos += 1;
            unit = code(self.src, pos);
        }
        if level > 6 || (pos < max && !unit.is_some_and(is_space)) { return Ok(false); }
        if silent { return Ok(true); }
        max = self.skip_spaces_back(max, pos);
        let tmp = self.skip_chars_back(max, 0x23, pos);
        if tmp > pos && is_space_at(self.src, tmp - 1) { max = tmp; }
        self.line = start + 1;
        const TAGS: [&str; 7] = ["", "h1", "h2", "h3", "h4", "h5", "h6"];
        self.push(Kind::Generic, TAGS[level], 1);
        let content = ascii_trim(&self.src[pos..max.max(pos)]).to_vec();
        self.push(Kind::Inline, "", 0).content = content;
        self.push(Kind::Generic, TAGS[level], -1);
        Ok(true)
    }

    /// rules_block/lheading.mjs.
    fn lheading(&mut self, start: usize, end: usize) -> Result<bool> {
        if self.s_count[start] - self.blk_indent >= 4 { return Ok(false); }
        let old_parent = self.parent;
        self.parent = Parent::Paragraph;
        let mut level = 0;
        let mut next = start + 1;
        while next < end && !self.is_empty(next) {
            if self.s_count[next] - self.blk_indent > 3 {
                next += 1;
                continue;
            }
            if self.s_count[next] >= self.blk_indent {
                let mut pos = self.b_marks[next] + self.t_shift[next];
                let max = self.e_marks[next];
                if pos < max {
                    let marker = self.src[pos];
                    if marker == 0x2d || marker == 0x3d {
                        pos = self.skip_chars(pos, marker);
                        pos = self.skip_spaces(pos);
                        if pos >= max {
                            level = if marker == 0x3d { 1 } else { 2 };
                            break;
                        }
                    }
                }
            }
            if self.s_count[next] < 0 {
                next += 1;
                continue;
            }
            if self.terminates(&PARAGRAPH_CHAIN, next, end)? { break; }
            next += 1;
        }
        if level == 0 {
            self.parent = old_parent;
            return Ok(false);
        }
        let content = ascii_trim(&self.get_lines(start, next, self.blk_indent, false)?).to_vec();
        self.line = next + 1;
        let tag = if level == 1 { "h1" } else { "h2" };
        self.push(Kind::Generic, tag, 1);
        self.push(Kind::Inline, "", 0).content = content;
        self.push(Kind::Generic, tag, -1);
        self.parent = old_parent;
        Ok(true)
    }

    /// rules_block/paragraph.mjs.
    fn paragraph(&mut self, start: usize, end: usize) -> Result<bool> {
        let old_parent = self.parent;
        let mut next = start + 1;
        self.parent = Parent::Paragraph;
        while next < end && !self.is_empty(next) {
            if self.s_count[next] - self.blk_indent > 3 || self.s_count[next] < 0 {
                next += 1;
                continue;
            }
            if self.terminates(&PARAGRAPH_CHAIN, next, end)? { break; }
            next += 1;
        }
        let content = ascii_trim(&self.get_lines(start, next, self.blk_indent, false)?).to_vec();
        self.line = next;
        self.push(Kind::ParagraphOpen, "p", 1);
        self.push(Kind::Inline, "", 0).content = content;
        self.push(Kind::Generic, "p", -1);
        self.parent = old_parent;
        Ok(true)
    }
}

/// `escapedSplit` (table.mjs).
fn escaped_split(src: &[u16]) -> Vec<Units> {
    let mut result = Vec::new();
    let mut current = Units::new();
    let (mut escaped, mut last) = (false, 0usize);
    for (pos, &unit) in src.iter().enumerate() {
        if unit == 0x7c {
            if !escaped {
                current.extend_from_slice(&src[last..pos]);
                result.push(std::mem::take(&mut current));
                last = pos + 1;
            } else {
                current.extend_from_slice(&src[last..pos - 1]);
                last = pos;
            }
        }
        escaped = unit == 0x5c;
    }
    current.extend_from_slice(&src[last..]);
    result.push(current);
    result
}

fn trim_outer_empty(columns: &mut Vec<Units>) {
    if columns.first().is_some_and(|column| column.is_empty()) { columns.remove(0); }
    if columns.last().is_some_and(|column| column.is_empty()) { columns.pop(); }
}
