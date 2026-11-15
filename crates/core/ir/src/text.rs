use crate::budget::{Budget, Error, OwnedString, OwnedVec};

#[derive(Debug)]
pub(crate) enum TextData { Utf8(OwnedString), Utf16(OwnedVec<u16>) }
#[derive(Debug)]
pub struct Text { pub(crate) data: TextData, pub(crate) units: usize }

// `remaining` counts the units `EncodeUtf16` has yet to yield, which its own size hint only bounds.
#[derive(Clone)]
enum UnitIter<'a> {
    Utf8 { iter: std::str::EncodeUtf16<'a>, remaining: usize },
    Utf16(std::iter::Copied<std::slice::Iter<'a, u16>>),
}

/// The UTF-16 code units of a [`Text`].
#[derive(Clone)]
pub struct Units<'a>(UnitIter<'a>);
impl Iterator for Units<'_> {
    type Item = u16;
    #[inline]
    fn next(&mut self) -> Option<u16> {
        match &mut self.0 {
            UnitIter::Utf8 { iter, remaining } => { let unit = iter.next()?; *remaining -= 1; Some(unit) }
            UnitIter::Utf16(iter) => iter.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) { (self.len(), Some(self.len())) }
    fn count(self) -> usize { self.len() }
    fn fold<B, F: FnMut(B, u16) -> B>(self, init: B, f: F) -> B {
        match self.0 { UnitIter::Utf8 { iter, .. } => iter.fold(init, f), UnitIter::Utf16(iter) => iter.fold(init, f) }
    }
}
impl ExactSizeIterator for Units<'_> {
    fn len(&self) -> usize { match &self.0 { UnitIter::Utf8 { remaining, .. } => *remaining, UnitIter::Utf16(iter) => iter.len() } }
}

impl Text {
    pub fn from_str(value: &str, budget: &Budget) -> Result<Self, Error> {
        budget.work(value.len())?;
        let units = value.encode_utf16().count();
        if units > budget.0.limits.max_text_units { return Err(Error::Limit("text-units")); }
        let mut data = OwnedString::new(budget)?;
        data.extend(value)?;
        Ok(Self { data: TextData::Utf8(data), units })
    }
    /// Stores well-formed units as UTF-8 and units with an isolated surrogate as they are, as [`TextBuilder`] would,
    /// charging `work` once for the length.
    pub fn from_units(value: &[u16], budget: &Budget) -> Result<Self, Error> {
        if value.len() > budget.0.limits.max_text_units { return Err(Error::Limit("text-units")); }
        budget.work(value.len())?;
        // The UTF-8 length if the units are well formed: a byte per unit, one more from U+0080 and one more from U+0800
        // outside the surrogates, so a pair's two units make its four bytes. Branch-free, so it vectorizes.
        let (mut bytes, mut surrogates) = (0usize, 0usize);
        for chunk in value.chunks(4096) {
            budget.check()?;
            for &unit in chunk {
                let surrogate = (0xd800..=0xdfff).contains(&unit);
                bytes += 1 + usize::from(unit >= 0x80) + usize::from(unit >= 0x800 && !surrogate);
                surrogates += usize::from(surrogate);
            }
        }
        if surrogates != 0 && !paired(value, budget)? {
            let mut data = OwnedVec::new(budget)?;
            data.reserve(value.len())?;
            data.data.extend_from_slice(value);
            return Ok(Self { data: TextData::Utf16(data), units: value.len() });
        }
        let mut data = OwnedString::new(budget)?;
        data.reserve(bytes)?;
        let capacity = data.data.capacity();
        if bytes == value.len() {
            let mut buffer = [0u8; 4096];
            for chunk in value.chunks(buffer.len()) {
                let ascii = &mut buffer[..chunk.len()];
                for (byte, unit) in ascii.iter_mut().zip(chunk) { *byte = *unit as u8; }
                data.data.push_str(std::str::from_utf8(ascii).expect("ASCII units"));
            }
        } else {
            data.data.extend(char::decode_utf16(value.iter().copied()).map(|value| value.expect("well-formed UTF-16")));
        }
        debug_assert_eq!(data.data.capacity(), capacity);
        Ok(Self { data: TextData::Utf8(data), units: value.len() })
    }
    pub fn units(&self) -> Units<'_> {
        match &self.data {
            TextData::Utf8(value) => Units(UnitIter::Utf8 { iter: value.data.encode_utf16(), remaining: self.units }),
            TextData::Utf16(value) => Units(UnitIter::Utf16(value.as_slice().iter().copied())),
        }
    }
    pub fn len_units(&self) -> usize { self.units }
    pub(crate) fn array_index_key(&self) -> Option<u32> {
        if self.units == 0 || self.units > 10 { return None; }
        // Digits are one byte and one unit each, so UTF-8 storage is read as bytes.
        if let Some(value) = self.as_unicode() {
            let bytes = value.as_bytes();
            if bytes[0] == b'0' { return (bytes.len() == 1).then_some(0); }
            let mut total = 0u32;
            for &byte in bytes {
                if !byte.is_ascii_digit() { return None; }
                total = total.checked_mul(10)?.checked_add((byte - b'0') as u32)?;
            }
            return (total < u32::MAX).then_some(total);
        }
        let mut units = self.units();
        let first = units.next()?;
        if first == 48 { return (self.units == 1).then_some(0); }
        if !(49..=57).contains(&first) { return None; }
        let mut value = (first - 48) as u32;
        for unit in units {
            if !(48..=57).contains(&unit) { return None; }
            value = value.checked_mul(10)?.checked_add((unit - 48) as u32)?;
        }
        (value < u32::MAX).then_some(value)
    }
    /// Fast path only; `None` is always a valid answer, callers must fall back to `units()`.
    pub fn as_unicode(&self) -> Option<&str> {
        match &self.data { TextData::Utf8(value) => Some(value.data.as_str()), _ => None }
    }
    pub fn equals(&self, other: &Self) -> bool {
        if self.units != other.units { return false; }
        // UTF-8 encodes each unit sequence one way, so two UTF-8 texts hold equal units exactly when their bytes are equal.
        if let (Some(left), Some(right)) = (self.as_unicode(), other.as_unicode()) { return left == right; }
        self.units().eq(other.units())
    }
    pub fn equals_ascii(&self, other: &[u8]) -> bool { self.units == other.len() && self.units().eq(other.iter().map(|v| *v as u16)) }
    pub fn copy(&self, budget: &Budget) -> Result<Self, Error> {
        match &self.data {
            TextData::Utf8(value) => Self::from_str(value.data.as_str(), budget),
            TextData::Utf16(value) => Self::from_units(value.as_slice(), budget),
        }
    }
    pub fn unicode_copy(&self, budget: &Budget) -> Result<Self, Error> {
        let value = self.copy(budget)?;
        if value.as_unicode().is_some() { Ok(value) } else { Err(Error::IsolatedUtf16) }
    }
    pub fn append(&mut self, other: &Self) -> Result<(), Error> {
        let units = self.units.checked_add(other.units).ok_or(Error::Limit("text-units"))?;
        let budget = match &self.data { TextData::Utf8(value) => value.budget(), TextData::Utf16(value) => value.budget() }.clone();
        if units > budget.0.limits.max_text_units { return Err(Error::Limit("text-units")); }
        if let (TextData::Utf8(left), TextData::Utf8(right)) = (&mut self.data, &other.data) {
            left.extend(right.data.as_str())?;
            self.units = units;
            return Ok(());
        }
        let mut next = TextBuilder::new(&budget)?;
        for unit in self.units().chain(other.units()) { next.unit(unit)?; }
        *self = next.finish()?;
        Ok(())
    }
    pub fn utf8_accounting_len(&self, budget: &Budget) -> Result<usize, Error> {
        budget.work(1)?;
        if let Some(value) = self.as_unicode() { return Ok(value.len()); }
        budget.work(self.len_units())?;
        let mut total = 0usize;
        for (index, value) in char::decode_utf16(self.units()).enumerate() {
            if index & 4095 == 0 { budget.check()?; }
            total = total.checked_add(value.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8()).ok_or(Error::Limit("output-bytes"))?;
        }
        Ok(total)
    }
}

pub struct TextBuilder {
    utf8: Option<OwnedString>,
    utf16: Option<OwnedVec<u16>>,
    pending: Option<u16>,
    units: usize,
    budget: Budget,
}
impl TextBuilder {
    pub fn new(budget: &Budget) -> Result<Self, Error> {
        Ok(Self { utf8: Some(OwnedString::new(budget)?), utf16: None, pending: None, units: 0, budget: budget.clone() })
    }
    fn upgrade(&mut self) -> Result<(), Error> {
        if self.utf16.is_some() { return Ok(()); }
        let mut next = OwnedVec::new(&self.budget)?;
        if let Some(value) = &self.utf8 {
            self.budget.work(value.data.len())?;
            for unit in value.data.encode_utf16() { next.push(unit)?; }
        }
        self.utf8 = None;
        self.utf16 = Some(next);
        Ok(())
    }
    fn scalar(&mut self, value: char) -> Result<(), Error> {
        let mut bytes = [0; 4];
        self.utf8.as_mut().expect("scalar builder").extend(value.encode_utf8(&mut bytes))
    }
    pub fn unit(&mut self, value: u16) -> Result<(), Error> {
        self.budget.work(1)?;
        self.units = self.units.checked_add(1).filter(|v| *v <= self.budget.0.limits.max_text_units).ok_or(Error::Limit("text-units"))?;
        if let Some(units) = self.utf16.as_mut() { return units.push(value); }
        if let Some(high) = self.pending.take() {
            if (0xdc00..=0xdfff).contains(&value) {
                return self.scalar(char::from_u32(0x10000 + (((high as u32) - 0xd800) << 10) + value as u32 - 0xdc00).expect("surrogate pair"));
            }
            self.upgrade()?;
            let units = self.utf16.as_mut().expect("UTF-16 builder");
            units.push(high)?;
            return units.push(value);
        }
        if (0xd800..=0xdbff).contains(&value) { self.pending = Some(value); return Ok(()); }
        if (0xdc00..=0xdfff).contains(&value) {
            self.upgrade()?;
            return self.utf16.as_mut().expect("UTF-16 builder").push(value);
        }
        self.scalar(char::from_u32(value as u32).expect("non-surrogate unit"))
    }
    pub fn character(&mut self, value: char) -> Result<(), Error> {
        let mut units = [0; 2];
        for unit in value.encode_utf16(&mut units) { self.unit(*unit)?; }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Text, Error> {
        if let Some(high) = self.pending.take() { self.upgrade()?; self.utf16.as_mut().expect("UTF-16 builder").push(high)?; }
        let data = if let Some(value) = self.utf16 { TextData::Utf16(value) } else { TextData::Utf8(self.utf8.expect("text builder")) };
        Ok(Text { data, units: self.units })
    }
}

fn js_space(value: u16) -> bool {
    matches!(value, 9..=13 | 32 | 160 | 5760 | 8192..=8202 | 8232 | 8233 | 8239 | 8287 | 12288 | 65279)
}
/// Whether every surrogate in `value` is half of a pair: a unit is a high surrogate exactly when the next is a low one,
/// the first unit is no low one and the last no high one. Branch-free per block, so it vectorizes.
fn paired(value: &[u16], budget: &Budget) -> Result<bool, Error> {
    let high = |unit: u16| (unit & 0xfc00) == 0xd800;
    let low = |unit: u16| (unit & 0xfc00) == 0xdc00;
    let (Some(&first), Some(&last)) = (value.first(), value.last()) else { return Ok(true) };
    if low(first) || high(last) { return Ok(false); }
    for (units, next) in value[..value.len() - 1].chunks(4096).zip(value[1..].chunks(4096)) {
        budget.check()?;
        if units.iter().zip(next).fold(false, |broken, (&unit, &following)| broken | (high(unit) != low(following))) {
            return Ok(false);
        }
    }
    Ok(true)
}
impl Text {
    pub fn prefix(&self, units: usize, budget: &Budget) -> Result<Self, Error> {
        let mut builder = TextBuilder::new(budget)?;
        for unit in self.units().take(units) { builder.unit(unit)?; }
        builder.finish()
    }
    pub fn trim(&self, budget: &Budget) -> Result<Self, Error> { self.trim_edges(true, true, budget) }
    fn trim_edges(&self, leading: bool, trailing: bool, budget: &Budget) -> Result<Self, Error> {
        match &self.data {
            TextData::Utf8(value) => {
                let text = value.data.as_str();
                budget.work(text.len())?;
                let mut first = 0;
                if leading {
                    for (index, (byte, character)) in text.char_indices().enumerate() {
                        if index & 4095 == 0 { budget.check()?; }
                        if (character as u32) > 0xffff || !js_space(character as u16) { break; }
                        first = byte + character.len_utf8();
                    }
                }
                let mut last = text.len();
                if trailing {
                    for (index, (byte, character)) in text[first..].char_indices().rev().enumerate() {
                        if index & 4095 == 0 { budget.check()?; }
                        if (character as u32) > 0xffff || !js_space(character as u16) { break; }
                        last = first + byte;
                    }
                }
                Self::from_str(&text[first..last], budget)
            }
            TextData::Utf16(value) => {
                let values = value.as_slice();
                budget.work(values.len())?;
                let mut first = 0;
                if leading {
                    for (index, unit) in values.iter().enumerate() {
                        if index & 4095 == 0 { budget.check()?; }
                        if !js_space(*unit) { break; }
                        first = index + 1;
                    }
                }
                let mut last = values.len();
                if trailing {
                    for (index, (at, unit)) in values[first..].iter().enumerate().rev().enumerate() {
                        if index & 4095 == 0 { budget.check()?; }
                        if !js_space(*unit) { break; }
                        last = first + at;
                    }
                }
                Self::from_units(&values[first..last], budget)
            }
        }
    }
    pub fn index_of(&self, needle: &Self, budget: &Budget) -> Result<Option<usize>, Error> {
        budget.work(1)?;
        if needle.len_units() == 0 { return Ok(Some(0)); }
        if needle.len_units() > self.len_units() { return Ok(None); }
        if let (Some(text), Some(needle)) = (self.as_unicode(), needle.as_unicode()) {
            if needle.len() > text.len() { return Ok(None); }
            let work = text.len().checked_mul(2).and_then(|length| length.checked_add(needle.len())).ok_or(Error::Limit("work"))?;
            budget.work(work)?;
            return Ok(text.find(needle).map(|byte| text[..byte].encode_utf16().count()));
        }
        budget.work(self.len_units().checked_add(needle.len_units()).ok_or(Error::Limit("work"))?)?;
        let mut text = OwnedVec::new(budget)?;
        for unit in self.units() { text.push(unit)?; }
        let mut pattern = OwnedVec::new(budget)?;
        for unit in needle.units() { pattern.push(unit)?; }
        for (index, part) in text.as_slice().windows(pattern.data.len()).enumerate() {
            budget.work(pattern.data.len())?;
            if part == pattern.as_slice() { return Ok(Some(index)); }
        }
        Ok(None)
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Self) -> bool { self.equals(other) }
}
impl Eq for Text {}

/// ECMAScript `WhiteSpace` and `LineTerminator`, the set `String.prototype.trim` removes.
pub fn is_whitespace(unit: u16) -> bool { js_space(unit) }

impl Text {
    /// Copies the UTF-16 units `start..end`, clamped to the text; an isolated surrogate at either edge is kept.
    pub fn slice(&self, start: usize, end: usize, budget: &Budget) -> Result<Self, Error> {
        let end = end.min(self.units);
        let start = start.min(end);
        budget.work(start)?;
        let mut builder = TextBuilder::new(budget)?;
        for unit in self.units().skip(start).take(end - start) { builder.unit(unit)?; }
        builder.finish()
    }
    /// `String.prototype.trimStart`.
    pub fn trim_start(&self, budget: &Budget) -> Result<Self, Error> { self.trim_edges(true, false, budget) }
    /// `String.prototype.trimEnd`.
    pub fn trim_end(&self, budget: &Budget) -> Result<Self, Error> { self.trim_edges(false, true, budget) }
}

/// The code points, as inclusive ranges, whose `toLocaleLowerCase`, `toLocaleUpperCase` or Final_Sigma context class
/// (case-ignorable, cased or neither) in a Unicode 16 JS engine (Node 24.12, ICU 77.1) differs from Rust's tables,
/// which are Unicode 17. Every other code point maps and classes alike, and both engines map a string one code point
/// at a time with only Final_Sigma reading the context, so text without these maps alike.
const CASE_DIFFERS: &[(u32, u32)] = &[
    (0x295, 0x295), (0x1acf, 0x1add), (0x1ae0, 0x1aeb), (0xa7ce, 0xa7cf), (0xa7d2, 0xa7d5), (0xa7f1, 0xa7f1),
    (0x10ec5, 0x10ec5), (0x10efa, 0x10efb), (0x11b60, 0x11b60), (0x11b62, 0x11b64), (0x11b66, 0x11b66),
    (0x11dd9, 0x11dd9), (0x16ea0, 0x16eb8), (0x16ebb, 0x16ed3), (0x16ff2, 0x16ff3), (0x1e6e3, 0x1e6e3),
    (0x1e6e6, 0x1e6e6), (0x1e6ee, 0x1e6ef), (0x1e6f5, 0x1e6f5), (0x1e6ff, 0x1e6ff),
];
/// Whether the text contains a code point whose Unicode 16 case mapping or context class differs.
pub fn unicode16_case_differs(s: &[u16]) -> bool {
    char::decode_utf16(s.iter().copied()).filter_map(Result::ok).any(|c| {
        let code = c as u32;
        CASE_DIFFERS.binary_search_by(|&(start, end)| if end < code { std::cmp::Ordering::Less } else if start > code {
            std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }).is_ok()
    })
}
