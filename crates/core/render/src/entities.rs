//! `decodeHTML` and `decodeHTMLStrict` of entities 4.5.0 (`lib/esm/decode.js`): `decodeWithTrie` and the
//! `EntityDecoder` states it drives, over the generated `htmlDecodeTree`. The attribute mode is not used by markdown-it.

use crate::tables::HTML_DECODE_TREE as TREE;
use crate::utils::{find_unit, from_code_point, is_ascii_digit, is_ascii_hex, parse_int, Units};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// `DecodingMode.Legacy` (`decodeHTML`): entities may end without a semicolon.
    Legacy,
    /// `DecodingMode.Strict` (`decodeHTMLStrict`): only semicolon-terminated entities.
    Strict,
}

const VALUE_LENGTH: u16 = 0xc000;
const BRANCH_LENGTH: u16 = 0x3f80;
const JUMP_TABLE: u16 = 0x7f;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State { EntityStart, NumericStart, NumericDecimal, NumericHex, NamedEntity }

struct Decoder {
    mode: Mode,
    state: State,
    consumed: usize,
    /// A tree index in the named state, the code point in the numeric states.
    result: u64,
    tree_index: isize,
    excess: usize,
}

fn tree(index: isize) -> Option<u16> { usize::try_from(index).ok().and_then(|index| TREE.get(index).copied()) }

/// `determineBranch` (decode.js).
fn determine_branch(current: u16, node: isize, unit: u16) -> isize {
    let branches = ((current & BRANCH_LENGTH) >> 7) as isize;
    let jump = (current & JUMP_TABLE) as isize;
    if branches == 0 { return if jump != 0 && unit as isize == jump { node } else { -1 }; }
    if jump != 0 {
        let value = unit as isize - jump;
        return if value < 0 || value >= branches { -1 } else { tree(node + value).map_or(-1, |next| next as isize - 1) };
    }
    let (mut low, mut high) = (node, node + branches - 1);
    while low <= high {
        let middle = (low + high) / 2;
        let Some(value) = tree(middle) else { return -1 };
        match value.cmp(&unit) {
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle - 1,
            std::cmp::Ordering::Equal => return tree(middle + branches).map_or(-1, |next| next as isize),
        }
    }
    -1
}

/// `replaceCodePoint` (decode_codepoint.js).
fn replace_code_point(code: u64) -> u32 {
    if (0xd800..=0xdfff).contains(&code) || code > 0x10ffff { return 0xfffd; }
    match code {
        0 => 65533, 128 => 8364, 130 => 8218, 131 => 402, 132 => 8222, 133 => 8230, 134 => 8224, 135 => 8225,
        136 => 710, 137 => 8240, 138 => 352, 139 => 8249, 140 => 338, 142 => 381, 145 => 8216, 146 => 8217,
        147 => 8220, 148 => 8221, 149 => 8226, 150 => 8211, 151 => 8212, 152 => 732, 153 => 8482, 154 => 353,
        155 => 8250, 156 => 339, 158 => 382, 159 => 376,
        other => other as u32,
    }
}

impl Decoder {
    fn new(mode: Mode) -> Self { Self { mode, state: State::EntityStart, consumed: 1, result: 0, tree_index: 0, excess: 1 } }

    /// `write(str, offset)`: the consumed length, 0 for no entity, -1 when the input ends inside one.
    fn write(&mut self, src: &[u16], offset: usize, out: &mut Units) -> isize {
        if src.get(offset) == Some(&0x23) {
            self.state = State::NumericStart;
            self.consumed += 1;
            return self.numeric_start(src, offset + 1, out);
        }
        self.state = State::NamedEntity;
        self.named_entity(src, offset, out)
    }

    fn numeric_start(&mut self, src: &[u16], offset: usize, out: &mut Units) -> isize {
        let Some(&unit) = src.get(offset) else { return -1 };
        if (unit | 0x20) == 0x78 {
            self.state = State::NumericHex;
            self.consumed += 1;
            return self.numeric(src, offset + 1, 16, out);
        }
        self.state = State::NumericDecimal;
        self.numeric(src, offset, 10, out)
    }

    fn add_to_numeric_result(&mut self, digits: &[u16], base: u64) {
        if digits.is_empty() { return; }
        let scale = (0..digits.len()).fold(1u64, |value, _| value.saturating_mul(base));
        self.result = self.result.saturating_mul(scale).saturating_add(parse_int(digits, base));
        self.consumed += digits.len();
    }

    /// `stateNumericHex` and `stateNumericDecimal`.
    fn numeric(&mut self, src: &[u16], offset: usize, base: u64, out: &mut Units) -> isize {
        let start = offset;
        let digit = |unit: u16| if base == 16 { is_ascii_hex(unit) } else { is_ascii_digit(unit) };
        let end = start + src[start.min(src.len())..].iter().take_while(|unit| digit(**unit)).count();
        self.add_to_numeric_result(&src[start.min(src.len())..end.min(src.len())], base);
        match src.get(end) {
            Some(&unit) => self.emit_numeric(Some(unit), if base == 16 { 3 } else { 2 }, out),
            None => -1,
        }
    }

    /// `emitNumericEntity` (decode.js).
    fn emit_numeric(&mut self, last: Option<u16>, expected: usize, out: &mut Units) -> isize {
        if self.consumed <= expected { return 0; }
        if last == Some(0x3b) {
            self.consumed += 1;
        } else if self.mode == Mode::Strict {
            return 0;
        }
        from_code_point(replace_code_point(self.result), out);
        self.consumed as isize
    }

    /// `stateNamedEntity` (decode.js).
    fn named_entity(&mut self, src: &[u16], mut offset: usize, out: &mut Units) -> isize {
        let mut current = tree(self.tree_index).unwrap_or(0);
        let mut value_length = ((current & VALUE_LENGTH) >> 14) as isize;
        while offset < src.len() {
            let unit = src[offset];
            self.tree_index = determine_branch(current, self.tree_index + value_length.max(1), unit);
            if self.tree_index < 0 {
                return if self.result == 0 { 0 } else { self.emit_not_terminated(out) };
            }
            current = tree(self.tree_index).unwrap_or(0);
            value_length = ((current & VALUE_LENGTH) >> 14) as isize;
            if value_length != 0 {
                if unit == 0x3b {
                    return self.emit_named(self.tree_index, value_length, self.consumed + self.excess, out);
                }
                if self.mode != Mode::Strict {
                    self.result = self.tree_index as u64;
                    self.consumed += self.excess;
                    self.excess = 0;
                }
            }
            offset += 1;
            self.excess += 1;
        }
        -1
    }

    fn emit_not_terminated(&mut self, out: &mut Units) -> isize {
        let result = self.result as isize;
        let value_length = ((tree(result).unwrap_or(0) & VALUE_LENGTH) >> 14) as isize;
        self.emit_named(result, value_length, self.consumed, out)
    }

    /// `emitNamedEntityData` (decode.js): each value unit goes through `fromCodePoint` on its own.
    fn emit_named(&self, result: isize, value_length: isize, consumed: usize, out: &mut Units) -> isize {
        let first = if value_length == 1 { tree(result).unwrap_or(0) & !VALUE_LENGTH } else { tree(result + 1).unwrap_or(0) };
        out.push(first);
        if value_length == 3 { out.push(tree(result + 2).unwrap_or(0)); }
        consumed as isize
    }

    /// `end()` (decode.js).
    fn end(&mut self, out: &mut Units) -> usize {
        match self.state {
            State::NamedEntity => if self.result != 0 { self.emit_not_terminated(out) as usize } else { 0 },
            State::NumericDecimal => self.emit_numeric(None, 2, out).max(0) as usize,
            State::NumericHex => self.emit_numeric(None, 3, out).max(0) as usize,
            State::NumericStart | State::EntityStart => 0,
        }
    }
}

/// `decodeWithTrie` (decode.js) over the HTML tree.
pub(crate) fn decode_html(src: &[u16], mode: Mode) -> Units {
    let mut out = Vec::with_capacity(src.len());
    let (mut last, mut offset) = (0usize, 0usize);
    while let Some(at) = find_unit(src, 0x26, offset) {
        out.extend_from_slice(&src[last..at]);
        let mut decoder = Decoder::new(mode);
        let length = decoder.write(src, at + 1, &mut out);
        if length < 0 {
            last = at + decoder.end(&mut out);
            break;
        }
        last = at + length as usize;
        offset = if length == 0 { last + 1 } else { last };
    }
    out.extend_from_slice(&src[last.min(src.len())..]);
    out
}
