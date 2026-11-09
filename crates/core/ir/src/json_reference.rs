//! The parser and writer `json.rs` shipped before their fast paths, kept unchanged as the reference the differential
//! test compares the current ones with: results, errors and every budget counter. Only the `Text` helpers whose bodies
//! changed are restated here in their original form.

use crate::budget::{Budget, Error, OwnedString, OwnedVec};
use crate::text::{Text, TextBuilder, TextData};
use crate::value::Value;

fn equals(left: &Text, right: &Text) -> bool { left.len_units() == right.len_units() && left.units().eq(right.units()) }
fn array_index_key(key: &Text) -> Option<u32> {
    if key.len_units() == 0 || key.len_units() > 10 { return None; }
    let mut units = key.units();
    let first = units.next()?;
    if first == 48 { return (key.len_units() == 1).then_some(0); }
    if !(49..=57).contains(&first) { return None; }
    let mut value = (first - 48) as u32;
    for unit in units {
        if !(48..=57).contains(&unit) { return None; }
        value = value.checked_mul(10)?.checked_add((unit - 48) as u32)?;
    }
    (value < u32::MAX).then_some(value)
}

pub fn parse_json(input: &str, budget: &Budget) -> Result<Value, Error> {
    admit(input.len(), budget)?;
    parse_bytes(input.as_bytes(), budget)
}

/// `JSON.parse` over a JS string, which may hold isolated surrogates. It is [`parse_json`] over the text's WTF-8 bytes
/// (UTF-8 with an isolated surrogate as its 3-byte sequence): the same charges, errors and key order, with
/// `json-bytes` counting those bytes and `Error::Syntax { byte }` an offset into them. Well-formed text is parsed in
/// place; other text is first copied to WTF-8 in a buffer charged as owned bytes.
pub fn parse_json_text(input: &Text, budget: &Budget) -> Result<Value, Error> {
    if let Some(value) = input.as_unicode() { return parse_json(value, budget); }
    let mut length = 0usize;
    for (index, unit) in char::decode_utf16(input.units()).enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        length += unit.map_or(3, char::len_utf8);
    }
    admit(length, budget)?;
    let mut bytes = OwnedVec::new(budget)?;
    bytes.reserve(length)?;
    for (index, unit) in char::decode_utf16(input.units()).enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        let mut buffer = [0; 4];
        match unit {
            Ok(character) => bytes.data.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes()),
            Err(isolated) => {
                let unit = isolated.unpaired_surrogate();
                bytes.data.extend_from_slice(&[0xe0 | (unit >> 12) as u8, 0x80 | ((unit >> 6) & 0x3f) as u8, 0x80 | (unit & 0x3f) as u8]);
            }
        }
    }
    parse_bytes(bytes.as_slice(), budget)
}

// The input charge and size limit every entry point applies to its UTF-8 or WTF-8 length.
fn admit(length: usize, budget: &Budget) -> Result<(), Error> {
    budget.work(length)?;
    if length > budget.0.limits.max_json_bytes { return Err(Error::Limit("json-bytes")); }
    Ok(())
}

// `input` is UTF-8, or WTF-8 holding an isolated surrogate as its 3-byte sequence.
fn parse_bytes(input: &[u8], budget: &Budget) -> Result<Value, Error> {
    let mut parser = Parser { input, position: 0, budget };
    let value = parser.value(0)?;
    parser.space()?;
    if parser.position != input.len() { return Err(parser.syntax()); }
    Ok(value)
}

// The scalar or surrogate code point that starts `bytes` and its byte length.
fn decode(bytes: &[u8]) -> Option<(u32, usize)> {
    let lead = *bytes.first()? as u32;
    let (length, initial) = match lead { 0..=0x7f => return Some((lead, 1)), 0xc0..=0xdf => (2, lead & 0x1f),
        0xe0..=0xef => (3, lead & 0x0f), _ => (4, lead & 0x07) };
    let tail = bytes.get(1..length)?;
    Some((tail.iter().fold(initial, |code, byte| (code << 6) | (*byte as u32 & 0x3f)), length))
}

struct Parser<'a> { input: &'a [u8], position: usize, budget: &'a Budget }
impl Parser<'_> {
    fn syntax(&self) -> Error { Error::Syntax { byte: self.position } }
    fn peek(&self) -> Option<u8> { self.input.get(self.position).copied() }
    fn space(&mut self) -> Result<(), Error> {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            if self.position & 4095 == 0 { self.budget.check()?; }
            self.position += 1;
        }
        Ok(())
    }
    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) { self.position += 1; true } else { false }
    }
    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.budget.value()?;
        if depth > self.budget.0.limits.max_depth { return Err(Error::Limit("json-depth")); }
        self.space()?;
        match self.peek() {
            Some(b'"') => Ok(Value::Text(self.string()?)),
            Some(b'[') => {
                self.position += 1;
                let mut values = OwnedVec::new(self.budget)?;
                self.space()?;
                if self.eat(b']') { return Ok(Value::Array(values)); }
                loop {
                    values.push(self.value(depth + 1)?)?;
                    self.space()?;
                    if self.eat(b']') { break; }
                    if !self.eat(b',') { return Err(self.syntax()); }
                }
                Ok(Value::Array(values))
            }
            Some(b'{') => {
                self.position += 1;
                let mut values: OwnedVec<(Text, Value)> = OwnedVec::new(self.budget)?;
                self.space()?;
                if self.eat(b'}') { return Ok(Value::Object(values)); }
                loop {
                    self.space()?;
                    let key = self.string()?;
                    self.space()?;
                    if !self.eat(b':') { return Err(self.syntax()); }
                    let value = self.value(depth + 1)?;
                    let mut existing = None;
                    for (index, (name, _)) in values.as_slice().iter().enumerate() {
                        self.budget.work(name.len_units().min(key.len_units()).saturating_add(1))?;
                        if equals(name, &key) { existing = Some(index); break; }
                    }
                    if let Some(index) = existing { values.as_mut_slice()[index].1 = value; }
                    else {
                        let mut position = values.data.len();
                        if let Some(index) = array_index_key(&key) {
                            for (at, (name, _)) in values.as_slice().iter().enumerate() {
                                self.budget.work(1)?;
                                if array_index_key(name).is_none_or(|old| old > index) { position = at; break; }
                            }
                        }
                        self.budget.work(values.data.len() - position)?;
                        values.push((key, value))?;
                        values.data[position..].rotate_right(1);
                    }
                    self.space()?;
                    if self.eat(b'}') { break; }
                    if !self.eat(b',') { return Err(self.syntax()); }
                }
                Ok(Value::Object(values))
            }
            Some(b'n') if self.literal("null") => Ok(Value::Null),
            Some(b't') if self.literal("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.literal("false") => Ok(Value::Bool(false)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.syntax()),
        }
    }
    fn literal(&mut self, expected: &str) -> bool {
        if self.input[self.position..].starts_with(expected.as_bytes()) { self.position += expected.len(); true } else { false }
    }
    fn string(&mut self) -> Result<Text, Error> {
        if !self.eat(b'"') { return Err(self.syntax()); }
        let mut output = TextBuilder::new(self.budget)?;
        loop {
            self.budget.work(1)?;
            let byte = self.peek().ok_or_else(|| self.syntax())?;
            if byte == b'"' { self.position += 1; return output.finish(); }
            if byte < 32 { return Err(self.syntax()); }
            if byte == b'\\' {
                self.position += 1;
                let escape = self.peek().ok_or_else(|| self.syntax())?;
                self.position += 1;
                let unit = match escape {
                    b'"' => 34, b'\\' => 92, b'/' => 47, b'b' => 8, b'f' => 12, b'n' => 10, b'r' => 13, b't' => 9,
                    b'u' => {
                        let mut value = 0u16;
                        for _ in 0..4 {
                            let digit = match self.peek() { Some(b'0'..=b'9') => self.peek().unwrap() - b'0',
                                Some(b'a'..=b'f') => self.peek().unwrap() - b'a' + 10,
                                Some(b'A'..=b'F') => self.peek().unwrap() - b'A' + 10, _ => return Err(self.syntax()) };
                            value = (value << 4) | digit as u16;
                            self.position += 1;
                        }
                        value
                    }
                    _ => return Err(self.syntax()),
                };
                output.unit(unit)?;
            } else {
                let (code, length) = decode(&self.input[self.position..]).ok_or_else(|| self.syntax())?;
                self.position += length;
                // A surrogate code point exists only in WTF-8 input; the builder pairs it as JS concatenation would.
                match char::from_u32(code) {
                    Some(character) => output.character(character)?,
                    None if (0xd800..=0xdfff).contains(&code) => output.unit(code as u16)?,
                    None => return Err(self.syntax()),
                }
            }
        }
    }
    fn number(&mut self) -> Result<Value, Error> {
        let start = self.position;
        self.eat(b'-');
        if !self.eat(b'0') {
            if !matches!(self.peek(), Some(b'1'..=b'9')) { return Err(self.syntax()); }
            while matches!(self.peek(), Some(b'0'..=b'9')) { if self.position & 4095 == 0 { self.budget.check()?; } self.position += 1; }
        }
        if self.eat(b'.') {
            let first = self.position;
            while matches!(self.peek(), Some(b'0'..=b'9')) { if self.position & 4095 == 0 { self.budget.check()?; } self.position += 1; }
            if first == self.position { return Err(self.syntax()); }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) { self.position += 1; }
            let first = self.position;
            while matches!(self.peek(), Some(b'0'..=b'9')) { if self.position & 4095 == 0 { self.budget.check()?; } self.position += 1; }
            if first == self.position { return Err(self.syntax()); }
        }
        self.budget.work(self.position - start)?;
        let digits = std::str::from_utf8(&self.input[start..self.position]).map_err(|_| self.syntax())?;
        let value = digits.parse::<f64>().map_err(|_| self.syntax())?;
        Ok(Value::Number(value))
    }
}

/// `JSON.stringify(value)`: `None` where JS returns `undefined`.
pub fn stringify(value: &Value, budget: &Budget) -> Result<Option<Text>, Error> {
    if matches!(value, Value::Undefined) { budget.work(1)?; return Ok(None); }
    let mut writer = Writer { data: OwnedString::new(budget)?, units: 0, budget };
    writer.value(value, 0)?;
    // Isolated surrogates leave as escapes, so the output is always Unicode text.
    Ok(Some(Text { data: TextData::Utf8(writer.data), units: writer.units }))
}

// The array-index part of the `Value::Object` order invariant.
fn js_key_order(entries: &[(Text, Value)]) -> bool {
    let (mut previous, mut named) = (None, false);
    for (key, _) in entries {
        match array_index_key(key) {
            Some(index) if named || previous.is_some_and(|previous| previous >= index) => return false,
            Some(index) => previous = Some(index),
            None => named = true,
        }
    }
    true
}

struct Writer<'a> { data: OwnedString, units: usize, budget: &'a Budget }
impl Writer<'_> {
    // `units` is the UTF-16 length of `value`, which every caller already knows.
    fn push(&mut self, value: &str, units: usize) -> Result<(), Error> {
        self.units = self.units.checked_add(units).filter(|total| *total <= self.budget.0.limits.max_text_units)
            .ok_or(Error::Limit("text-units"))?;
        self.data.extend(value)
    }
    fn ascii(&mut self, value: &str) -> Result<(), Error> { self.push(value, value.len()) }
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), Error> {
        self.budget.work(1)?;
        if depth > self.budget.0.limits.max_depth { return Err(Error::Limit("json-depth")); }
        match value {
            // An undefined array element serializes as null; object members holding it are skipped below.
            Value::Undefined | Value::Null => self.ascii("null"),
            Value::Bool(value) => self.ascii(if *value { "true" } else { "false" }),
            Value::Number(value) if !value.is_finite() => self.ascii("null"),
            Value::Number(value) if *value == 0.0 => self.ascii("0"),
            Value::Number(value) => self.ascii(ryu_js::Buffer::new().format_finite(*value)),
            Value::Text(value) => self.quote(value),
            Value::Array(values) => {
                self.ascii("[")?;
                for (index, value) in values.as_slice().iter().enumerate() {
                    if index != 0 { self.ascii(",")?; }
                    self.value(value, depth + 1)?;
                }
                self.ascii("]")
            }
            Value::Object(values) => {
                debug_assert!(js_key_order(values.as_slice()), "Value::Object keys are not in JS property order");
                self.ascii("{")?;
                let mut first = true;
                for (key, value) in values.as_slice() {
                    self.budget.work(1)?;
                    if matches!(value, Value::Undefined) { continue; }
                    if !first { self.ascii(",")?; }
                    first = false;
                    self.quote(key)?;
                    self.ascii(":")?;
                    self.value(value, depth + 1)?;
                }
                self.ascii("}")
            }
        }
    }
    // QuoteJSONString: short escapes, `\u00xx` for other controls and `\uxxxx` for isolated surrogates.
    fn quote(&mut self, text: &Text) -> Result<(), Error> {
        self.ascii("\"")?;
        if let Some(value) = text.as_unicode() {
            let mut start = 0;
            let mut units = 0;
            for (count, (index, character)) in value.char_indices().enumerate() {
                if count & 4095 == 0 { self.budget.check()?; }
                if (character as u32) < 0x20 || character == '"' || character == '\\' {
                    self.push(&value[start..index], units)?;
                    self.escape(character as u16)?;
                    start = index + 1;
                    units = 0;
                } else {
                    units += character.len_utf16();
                }
            }
            self.push(&value[start..], units)?;
        } else {
            for (index, unit) in char::decode_utf16(text.units()).enumerate() {
                if index & 4095 == 0 { self.budget.check()?; }
                match unit {
                    Ok(character) if (character as u32) < 0x20 || character == '"' || character == '\\' => self.escape(character as u16)?,
                    Ok(character) => {
                        let mut bytes = [0; 4];
                        self.push(character.encode_utf8(&mut bytes), character.len_utf16())?;
                    }
                    Err(isolated) => self.escape(isolated.unpaired_surrogate())?,
                }
            }
        }
        self.ascii("\"")
    }
    fn escape(&mut self, unit: u16) -> Result<(), Error> {
        let short = match unit { 8 => "\\b", 9 => "\\t", 10 => "\\n", 12 => "\\f", 13 => "\\r", 34 => "\\\"", 92 => "\\\\", _ => "" };
        if !short.is_empty() { return self.ascii(short); }
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = *b"\\u0000";
        for (at, shift) in [12, 8, 4, 0].into_iter().enumerate() { bytes[2 + at] = HEX[((unit >> shift) & 15) as usize]; }
        self.ascii(std::str::from_utf8(&bytes).map_err(|_| Error::Invariant("json-escape"))?)
    }
}
