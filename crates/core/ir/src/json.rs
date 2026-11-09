use crate::budget::{Budget, Error, Growth, Meter, OwnedString, OwnedVec};
use crate::text::{Text, TextBuilder, TextData};
use crate::value::Value;
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};
use std::mem::size_of;

#[cfg(test)]
#[path = "json_reference.rs"]
mod reference;
#[cfg(test)]
#[path = "json_differential.rs"]
mod differential;

/// `JSON.parse`. `work` and `json-values` charges are leased from `budget` in blocks of 64Ki and 1Ki, so
/// cancellation is observed at least once per block rather than at every item.
pub fn parse_json(input: &str, budget: &Budget) -> Result<Value, Error> {
    admit(input.len(), budget)?;
    parse_bytes(input.as_bytes(), Some(input), budget)
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
    parse_bytes(bytes.as_slice(), None, budget)
}

// The input charge and size limit every entry point applies to its UTF-8 or WTF-8 length.
fn admit(length: usize, budget: &Budget) -> Result<(), Error> {
    budget.work(length)?;
    if length > budget.0.limits.max_json_bytes { return Err(Error::Limit("json-bytes")); }
    Ok(())
}

// `input` is UTF-8 with `text` the same bytes as a `str`, or WTF-8 (an isolated surrogate as its 3-byte sequence)
// with no `text`.
fn parse_bytes(input: &[u8], text: Option<&str>, budget: &Budget) -> Result<Value, Error> {
    let mut parser = Parser { input, text, position: 0, budget, meter: Meter::new(budget), items: Vec::new(), members: Vec::new() };
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

fn sequence_length(lead: u8) -> usize { match lead { 0..=0x7f => 1, 0x80..=0xdf => 2, 0xe0..=0xef => 3, _ => 4 } }

fn hex4(bytes: &[u8]) -> Option<u16> {
    bytes.get(..4)?.iter().try_fold(0u16, |value, &byte| Some((value << 4) | (byte as char).to_digit(16)? as u16))
}

// The unit of a two-byte escape `\x`.
fn short_escape(byte: u8) -> Option<u8> {
    Some(match byte { b'"' => 34, b'\\' => 92, b'/' => 47, b'b' => 8, b'f' => 12, b'n' => 10, b'r' => 13, b't' => 9, _ => return None })
}

const CHUNK: usize = 32;
fn chunk(input: &[u8], at: usize) -> Option<&[u8; CHUNK]> { input.get(at..at + CHUNK)?.try_into().ok() }

// The end of the run of string bytes from `at` that need no attention (no quote, backslash, control or, in WTF-8, 0xED
// lead), with its continuation bytes and 4-byte leads. Whole chunks are tested branch-free so they vectorize.
fn plain_run(input: &[u8], mut at: usize, wtf8: bool) -> (usize, usize, usize) {
    let special = |byte: u8| (byte == b'"') | (byte == b'\\') | (byte < 0x20) | (wtf8 & (byte == 0xed));
    let (mut continuations, mut fours) = (0usize, 0usize);
    while let Some(bytes) = chunk(input, at) {
        let (mut found, mut continuing, mut four) = (0u8, 0u8, 0u8);
        for &byte in bytes {
            found |= special(byte) as u8;
            continuing += ((byte & 0xc0) == 0x80) as u8;
            four += (byte >= 0xf0) as u8;
        }
        if found != 0 { break; }
        continuations += continuing as usize;
        fours += four as usize;
        at += CHUNK;
    }
    while let Some(&byte) = input.get(at) {
        if special(byte) { break; }
        continuations += ((byte & 0xc0) == 0x80) as usize;
        fours += (byte >= 0xf0) as usize;
        at += 1;
    }
    (at, continuations, fours)
}

// The offset of the first backslash in `bytes`, or its length.
fn next_escape(bytes: &[u8]) -> usize {
    let mut at = 0;
    while let Some(block) = chunk(bytes, at) {
        if block.iter().fold(0u8, |found, &byte| found | (byte == b'\\') as u8) != 0 { break; }
        at += CHUNK;
    }
    at + bytes[at..].iter().position(|&byte| byte == b'\\').unwrap_or(bytes.len() - at)
}

// What the unit-by-unit string path builds and charges for one string with no isolated surrogate: its UTF-8 length and
// units, its loop steps (one per character or escape, and one for the closing quote), and the growth of its
// `OwnedString` from empty (the copy work each growth charges, and the last two capacities).
#[derive(Default)]
struct Scan { end: usize, bytes: usize, units: usize, steps: usize, growth: usize, previous: usize, capacity: usize, escaped: bool }
impl Scan {
    // `OwnedString::extend` with one character of `length` bytes.
    fn append(&mut self, length: usize) {
        let needed = self.bytes + length;
        if needed > self.capacity {
            self.growth += self.bytes;
            self.previous = self.capacity;
            self.capacity = needed.checked_next_power_of_two().unwrap_or(needed);
        }
        self.bytes = needed;
    }
    // The characters of `run` appended one by one; only the characters that grow the string are visited, the one
    // holding byte `capacity` each time.
    fn append_run(&mut self, run: &[u8]) {
        let start = self.bytes;
        let end = start + run.len();
        while self.capacity < end {
            let mut at = self.capacity - start;
            while (run[at] & 0xc0) == 0x80 { at -= 1; }
            self.bytes = start + at;
            self.append(sequence_length(run[at]));
        }
        self.bytes = end;
    }
    fn work(&self) -> Option<usize> { self.steps.checked_add(self.units)?.checked_add(self.bytes)?.checked_add(self.growth) }
}

// The powers of ten binary64 holds exactly.
const EXACT_POWERS: [f64; 23] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18,
    1e19, 1e20, 1e21, 1e22];

// The value of a number token the grammar accepted, when its decimal significand is at most 2^53 and its power of ten at
// most 22 in magnitude: both are then exact binary64 values, so one correctly rounded multiplication or division gives
// the correctly rounded value `str::parse` returns (Clinger's fast path). `None` for every other token.
fn exact_number(token: &[u8]) -> Option<f64> {
    let (negative, body) = match token { [b'-', rest @ ..] => (true, rest), _ => (false, token) };
    let (mut significand, mut digits, mut fraction, mut at) = (0u64, 0usize, 0i32, 0usize);
    let mut point = false;
    while let Some(&byte) = body.get(at) {
        match byte {
            b'0'..=b'9' => {
                digits += 1;
                if digits > 19 { return None; }
                significand = significand * 10 + (byte - b'0') as u64;
                fraction += i32::from(point);
            }
            b'.' => point = true,
            _ => break,
        }
        at += 1;
    }
    let mut exponent = 0i32;
    if matches!(body.get(at), Some(b'e' | b'E')) {
        at += 1;
        let below_one = body.get(at) == Some(&b'-');
        if matches!(body.get(at), Some(b'+' | b'-')) { at += 1; }
        for &byte in &body[at..] {
            exponent = exponent * 10 + (byte - b'0') as i32;
            if exponent > 1000 { return None; }
        }
        if below_one { exponent = -exponent; }
    }
    if significand > 1 << 53 { return None; }
    let value = match exponent - fraction {
        power @ 0..=22 => significand as f64 * EXACT_POWERS[power as usize],
        power @ -22..=-1 => significand as f64 / EXACT_POWERS[-power as usize],
        _ => return None,
    };
    Some(if negative { -value } else { value })
}

// The index of `key` among `entries`, charging each comparison before it is made: in one charge when their sum fits,
// otherwise one by one so that a limit stops at the same comparison.
fn find_key(meter: &mut Meter, entries: &[(Text, Value)], key: &Text) -> Result<Option<usize>, Error> {
    let cost = |name: &Text| name.len_units().min(key.len_units()).saturating_add(1);
    let mut total = 0usize;
    let mut found = None;
    for (index, (name, _)) in entries.iter().enumerate() {
        total = total.saturating_add(cost(name));
        if name.equals(key) { found = Some(index); break; }
    }
    if meter.covers(total)? { meter.work(total)?; return Ok(found); }
    for (name, _) in &entries[..found.map_or(entries.len(), |index| index + 1)] { meter.work(cost(name))?; }
    Ok(found)
}

// What an object being parsed knows about its keys so that each new key costs a constant number of steps however many
// members precede it: how many leading members have array-index keys, and from `INDEXED` members on, the hashes of its
// UTF-8 keys and a count of its keys by unit length. A UTF-16 key holds an isolated surrogate, so it never equals a
// UTF-8 key and is always looked up by comparison.
#[derive(Default)]
struct Keys { indices: usize, index: Option<Box<KeyIndex>> }
const INDEXED: usize = 32;
// Unit lengths up to `SHORT` are counted in Fenwick trees; longer ones are listed.
const SHORT: usize = 255;
struct KeyIndex { hashes: HashSet<u64, BuildHasherDefault<Prehashed>>, counts: [usize; SHORT + 1], sums: [usize; SHORT + 1], long: Vec<usize> }

#[derive(Default)]
struct Prehashed(u64);
impl Hasher for Prehashed {
    fn finish(&self) -> u64 { self.0 }
    fn write(&mut self, bytes: &[u8]) { for &byte in bytes { self.0 = (self.0.rotate_left(8) ^ byte as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15); } }
    fn write_u64(&mut self, value: u64) { self.0 = value; }
}

fn key_hash(key: &str) -> u64 {
    let mix = |hash: u64, word: u64| (hash.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    let bytes = key.as_bytes();
    let mut hash = mix(0, bytes.len() as u64);
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks { hash = mix(hash, u64::from_le_bytes(chunk.try_into().expect("8 bytes"))); }
    let mut last = [0u8; 8];
    last[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
    mix(hash, u64::from_le_bytes(last))
}

impl KeyIndex {
    fn new(entries: &[(Text, Value)]) -> Box<Self> {
        let mut index = Box::new(Self { hashes: HashSet::default(), counts: [0; SHORT + 1], sums: [0; SHORT + 1], long: Vec::new() });
        for (key, _) in entries { index.insert(key); }
        index
    }
    fn insert(&mut self, key: &Text) {
        if let Some(value) = key.as_unicode() { self.hashes.insert(key_hash(value)); }
        let length = key.len_units();
        if length > SHORT { return self.long.push(length); }
        // Fenwick position `length + 1` over lengths 0..=SHORT.
        let mut at = length + 1;
        while at <= SHORT + 1 {
            self.counts[at - 1] += 1;
            self.sums[at - 1] += length;
            at += at & at.wrapping_neg();
        }
    }
    // The count and length sum of the short keys of at most `length` units.
    fn short_at_most(&self, length: usize) -> (usize, usize) {
        let (mut count, mut sum) = (0, 0);
        let mut at = length.min(SHORT) + 1;
        while at > 0 {
            count += self.counts[at - 1];
            sum += self.sums[at - 1];
            at &= at - 1;
        }
        (count, sum)
    }
    // What `find_key` charges for comparing a key of `length` units with all `members` keys: the sum over them of the
    // shorter length plus one.
    fn cost(&self, length: usize, members: usize) -> usize {
        let (short, short_sum) = self.short_at_most(length);
        let (all_short, _) = self.short_at_most(SHORT);
        let long = self.long.iter().map(|&other| other.min(length)).fold(0usize, usize::saturating_add);
        short_sum.saturating_add(length.saturating_mul(all_short - short)).saturating_add(long).saturating_add(members)
    }
}

impl Keys {
    // `find_key` over `entries`, the object's members so far.
    fn find(&mut self, meter: &mut Meter, entries: &[(Text, Value)], key: &Text) -> Result<Option<usize>, Error> {
        if self.index.is_none() && entries.len() >= INDEXED { self.index = Some(KeyIndex::new(entries)); }
        let (Some(index), Some(value)) = (&self.index, key.as_unicode()) else { return find_key(meter, entries, key) };
        if index.hashes.contains(&key_hash(value)) { return find_key(meter, entries, key); }
        // No member holds this key, so every comparison is made.
        let total = index.cost(key.len_units(), entries.len());
        if meter.covers(total)? { return meter.work(total).map(|()| None); }
        for (name, _) in entries { meter.work(name.len_units().min(key.len_units()).saturating_add(1))?; }
        Ok(None)
    }
    // Where a new `key` goes among `entries`, charging the steps the scan of the leading array-index keys takes.
    fn position(&mut self, meter: &mut Meter, entries: &[(Text, Value)], key: &Text) -> Result<usize, Error> {
        let length = entries.len();
        let Some(index) = key.array_index_key() else { return Ok(length) };
        // The scan charges a step for each smaller index key and for the larger index or other key it stops at, if any.
        let position = entries[..self.indices].partition_point(|(name, _)| name.array_index_key().is_some_and(|old| old < index));
        let scanned = position + usize::from(position < length);
        if meter.covers(scanned + (length - position))? { meter.work(scanned + (length - position))?; }
        else {
            for _ in 0..scanned { meter.work(1)?; }
            meter.work(length - position)?;
        }
        self.indices += 1;
        Ok(position)
    }
    fn insert(&mut self, key: &Text) { if let Some(index) = &mut self.index { index.insert(key); } }
}

// Charges go through `meter`, which keeps each one's outcome and the budget's counters as direct charges would leave
// them; only `unit_string` charges the budget directly. The elements of every open array and object wait on `items`
// and `members`, each container's above its parent's, and move into a vector of their final capacity when it closes,
// while `Growth` charges each container as pushing onto its own vector would.
struct Parser<'a> {
    input: &'a [u8], text: Option<&'a str>, position: usize, budget: &'a Budget, meter: Meter<'a>,
    items: Vec<Value>, members: Vec<(Text, Value)>,
}
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
        self.meter.value()?;
        if depth > self.budget.0.limits.max_depth { return Err(Error::Limit("json-depth")); }
        self.space()?;
        match self.peek() {
            Some(b'"') => Ok(Value::Text(self.string()?)),
            Some(b'[') => {
                self.position += 1;
                let mut growth = Growth::new(self.budget, size_of::<Value>())?;
                let base = self.items.len();
                self.space()?;
                if !self.eat(b']') {
                    loop {
                        let value = self.value(depth + 1)?;
                        growth.push(self.items.len() - base)?;
                        self.items.push(value);
                        self.space()?;
                        if self.eat(b']') { break; }
                        if !self.eat(b',') { return Err(self.syntax()); }
                    }
                }
                Ok(Value::Array(growth.finish(self.items.drain(base..))?))
            }
            Some(b'{') => {
                self.position += 1;
                let mut growth = Growth::new(self.budget, size_of::<(Text, Value)>())?;
                let mut keys = Keys::default();
                let base = self.members.len();
                self.space()?;
                if !self.eat(b'}') {
                    loop {
                        self.space()?;
                        let key = self.string()?;
                        self.space()?;
                        if !self.eat(b':') { return Err(self.syntax()); }
                        let value = self.value(depth + 1)?;
                        if let Some(index) = keys.find(&mut self.meter, &self.members[base..], &key)? { self.members[base + index].1 = value; }
                        else {
                            let length = self.members.len() - base;
                            let position = keys.position(&mut self.meter, &self.members[base..], &key)?;
                            growth.push(length)?;
                            keys.insert(&key);
                            self.members.push((key, value));
                            self.members[base + position..].rotate_right(1);
                        }
                        self.space()?;
                        if self.eat(b'}') { break; }
                        if !self.eat(b',') { return Err(self.syntax()); }
                    }
                }
                Ok(Value::Object(growth.finish(self.members.drain(base..))?))
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
        if self.peek() == Some(b'"') {
            if let Some(text) = self.whole_string()? { return Ok(text); }
        }
        self.meter.release_work();
        self.unit_string()
    }
    // The string at `position` built in one allocation with the charges `unit_string` would make, in total, when it
    // holds no isolated surrogate and none of those charges would fail; `None` changes nothing.
    fn whole_string(&mut self) -> Result<Option<Text>, Error> {
        let Some(scan) = self.scan(self.position + 1)? else { return Ok(None) };
        let Some(work) = scan.work() else { return Ok(None) };
        if scan.units > self.budget.0.limits.max_text_units || !self.meter.covers(work)? { return Ok(None); }
        let data = if scan.bytes == 0 { OwnedString::new(self.budget)? } else {
            let Some(charge) = self.budget.grown(scan.previous, scan.capacity)? else { return Ok(None) };
            let mut data = String::new();
            if data.try_reserve_exact(scan.capacity).is_err() || data.capacity() != scan.capacity { return Ok(None); }
            self.fill(&mut data, self.position + 1, scan.end, scan.escaped)?;
            debug_assert_eq!(data.len(), scan.bytes);
            OwnedString::charged(data, charge)
        };
        self.meter.work(work)?;
        self.position = scan.end + 1;
        Ok(Some(Text { data: TextData::Utf8(data), units: scan.units }))
    }
    // The string whose content starts at `start`, or `None` for one `whole_string` leaves to `unit_string`: an isolated
    // surrogate, an invalid escape, a control character or no closing quote.
    fn scan(&self, start: usize) -> Result<Option<Scan>, Error> {
        let input = self.input;
        let wtf8 = self.text.is_none();
        let mut scan = Scan::default();
        let mut at = start;
        let mut checked = start;
        loop {
            let (end, continuations, fours) = plain_run(input, at, wtf8);
            if end != at {
                let characters = end - at - continuations;
                scan.steps += characters;
                scan.units += characters + fours;
                scan.append_run(&input[at..end]);
                at = end;
            }
            if at - checked >= 1 << 16 { self.budget.check()?; checked = at; }
            match input.get(at) {
                Some(b'"') => {
                    scan.steps += 1;
                    scan.end = at;
                    return Ok(Some(scan));
                }
                Some(b'\\') => {
                    scan.escaped = true;
                    match input.get(at + 1) {
                        Some(b'u') => match hex4(&input[at + 2..]) {
                            // A high surrogate pairs with an escaped low one that follows it at once.
                            Some(0xd800..=0xdbff) => {
                                let low = input.get(at + 6..at + 8).filter(|escape| *escape == b"\\u").and_then(|_| hex4(&input[at + 8..]));
                                if !low.is_some_and(|low| (0xdc00..=0xdfff).contains(&low)) { return Ok(None); }
                                scan.steps += 2;
                                scan.units += 2;
                                scan.append(4);
                                at += 12;
                            }
                            Some(0xdc00..=0xdfff) | None => return Ok(None),
                            Some(unit) => {
                                scan.steps += 1;
                                scan.units += 1;
                                scan.append(char::from_u32(unit as u32).map_or(3, char::len_utf8));
                                at += 6;
                            }
                        },
                        Some(&escape) if short_escape(escape).is_some() => {
                            scan.steps += 1;
                            scan.units += 1;
                            scan.append(1);
                            at += 2;
                        }
                        _ => return Ok(None),
                    }
                }
                // A 0xED lead in WTF-8: U+D000 to U+D7FF is an ordinary character, a surrogate is not.
                Some(0xed) if input.get(at + 1).is_some_and(|byte| *byte < 0xa0) => {
                    scan.steps += 1;
                    scan.units += 1;
                    scan.append(3);
                    at += 3;
                }
                _ => return Ok(None),
            }
        }
    }
    // Unescapes the string bytes `start..end`, which `scan` accepted, into `data`.
    fn fill(&self, data: &mut String, start: usize, end: usize, escaped: bool) -> Result<(), Error> {
        let run = |from: usize, to: usize| match self.text {
            Some(text) => text.get(from..to),
            None => std::str::from_utf8(&self.input[from..to]).ok(),
        }.ok_or(Error::Invariant("json-string-run"));
        if !escaped { data.push_str(run(start, end)?); return Ok(()); }
        let invalid = || Error::Invariant("json-string-escape");
        let mut at = start;
        loop {
            let next = at + next_escape(&self.input[at..end]);
            data.push_str(run(at, next)?);
            if next == end { return Ok(()); }
            let escape = self.input[next + 1];
            if escape != b'u' {
                data.push(short_escape(escape).ok_or_else(invalid)? as char);
                at = next + 2;
                continue;
            }
            let unit = hex4(&self.input[next + 2..]).ok_or_else(invalid)? as u32;
            let (code, length) = match unit {
                0xd800..=0xdbff => (0x10000 + ((unit - 0xd800) << 10) + (hex4(&self.input[next + 8..]).ok_or_else(invalid)? as u32 - 0xdc00), 12),
                _ => (unit, 6),
            };
            data.push(char::from_u32(code).ok_or_else(invalid)?);
            at = next + length;
        }
    }
    fn unit_string(&mut self) -> Result<Text, Error> {
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
        self.meter.work(self.position - start)?;
        if let Some(value) = exact_number(&self.input[start..self.position]) { return Ok(Value::Number(value)); }
        let digits = std::str::from_utf8(&self.input[start..self.position]).map_err(|_| self.syntax())?;
        let value = digits.parse::<f64>().map_err(|_| self.syntax())?;
        Ok(Value::Number(value))
    }
}

pub use self::parse_json as parse;
pub use self::parse_json_text as parse_text;

/// `JSON.stringify(value)`: `None` where JS returns `undefined`. `work` charges are leased in blocks of 64Ki, so
/// cancellation is observed at least once per block rather than at every item.
pub fn stringify(value: &Value, budget: &Budget) -> Result<Option<Text>, Error> {
    if matches!(value, Value::Undefined) { budget.work(1)?; return Ok(None); }
    let mut writer = Writer { data: OwnedString::new(budget)?, units: 0, budget, meter: Meter::new(budget) };
    writer.value(value, 0)?;
    // Isolated surrogates leave as escapes, so the output is always Unicode text.
    Ok(Some(Text { data: TextData::Utf8(writer.data), units: writer.units }))
}

// The array-index part of the `Value::Object` order invariant.
fn js_key_order(entries: &[(Text, Value)]) -> bool {
    let (mut previous, mut named) = (None, false);
    for (key, _) in entries {
        match key.array_index_key() {
            Some(index) if named || previous.is_some_and(|previous| previous >= index) => return false,
            Some(index) => previous = Some(index),
            None => named = true,
        }
    }
    true
}

// `work` charges go through `meter`, except the copy a growth of `data` charges.
struct Writer<'a> { data: OwnedString, units: usize, budget: &'a Budget, meter: Meter<'a> }
impl Writer<'_> {
    // `units` is the UTF-16 length of `value`, which every caller already knows.
    fn push(&mut self, value: &str, units: usize) -> Result<(), Error> {
        self.units = self.units.checked_add(units).filter(|total| *total <= self.budget.0.limits.max_text_units)
            .ok_or(Error::Limit("text-units"))?;
        // `OwnedString::extend`, whose growth charges `work` directly.
        self.meter.work(value.len())?;
        if self.data.data.capacity() - self.data.data.len() < value.len() { self.meter.release_work(); }
        self.data.reserve(value.len())?;
        self.data.data.push_str(value);
        Ok(())
    }
    fn ascii(&mut self, value: &str) -> Result<(), Error> { self.push(value, value.len()) }
    fn value(&mut self, value: &Value, depth: usize) -> Result<(), Error> {
        self.meter.work(1)?;
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
                    self.meter.work(1)?;
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
            // Each run up to the next character to escape leaves in one push, as character by character would.
            let mut start = 0;
            loop {
                self.budget.check()?;
                let (end, continuations, fours) = plain_run(value.as_bytes(), start, false);
                self.push(&value[start..end], end - start - continuations + fours)?;
                let Some(&byte) = value.as_bytes().get(end) else { break };
                self.escape(byte as u16)?;
                start = end + 1;
            }
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
