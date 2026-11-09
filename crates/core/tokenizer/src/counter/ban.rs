//! `strongBan` (`src/ts/tokenizer.ts`): the logit bias that bans a word together with the tokens it forms next to
//! punctuation.
//!
//! TS caches the result in `localStorage` under [`cache_key`], a key that names neither the tokenizer nor the bias it
//! started from, so a bias computed under one tokenizer is returned under every other. The native form keeps the
//! same entry in the owner's device-local store (per device, never synced): the owner reads it into
//! [`StrongBan::start`] and writes [`BanResult::store`] back.

use super::encode::Encoder;
use risunest_ir::stage::{HostReply, StageError, Step};
use risunest_ir::{json, num, Budget, Text, TextBuilder, Value};
use std::borrow::Cow;

/// `banChars`, iterated by code point.
pub const BAN_CHARACTERS: &str = " !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~\u{201c}\u{201d}\u{2018}\u{2019}\u{ab}\u{bb}\u{300c}\u{300d}\u{2026}\u{2013}\u{2015}\u{203b}";

const PREFIX: &str = "strongBan_";
// `charAlt`: the text, trimmed, upper and lower case, and with its first unit in upper and lower case.
const ALTERNATES: usize = 6;

/// The `localStorage` key `strongBan` reads and writes: `'strongBan_' + data`.
pub fn cache_key(data: &Text, budget: &Budget) -> Result<Text, StageError> {
    let mut key = Text::from_str(PREFIX, budget)?;
    key.append(data)?;
    Ok(key)
}

/// The tables of the JS engine's `toLocaleUpperCase` and `toLocaleLowerCase`, from the host's probes of its Unicode
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseTables {
    /// Rust's own tables: an engine whose case mapping is the Unicode version of Rust's.
    Native,
    /// Unicode 16: Rust's tables except where `risunest_ir::text::unicode16_case_differs` finds a difference.
    Unicode16,
    /// Tables the port does not reproduce, or an engine that was not probed: only ASCII maps alike.
    Foreign,
}

/// What one `strongBan` call returns and stores.
#[derive(Debug)]
pub struct BanResult {
    /// The parsed cached bias, or the given bias with every banned ID set to -100.
    pub bias: Value,
    /// `JSON.stringify(bias)`, which TS stores under [`cache_key`]; `None` when it returned the cached bias.
    pub store: Option<Text>,
}

struct Run {
    characters: Vec<Text>,
    alternates: Vec<Text>,
    bias: Value,
    // The next text to encode, as an index into the sequence `texts` describes.
    next: usize,
    // `unbanChars`: the first ID of each ban character, `None` where it encodes to nothing.
    unban: Vec<Option<f64>>,
}

impl Run {
    fn len(&self) -> usize { self.characters.len() * (2 + 2 * self.alternates.len()) }
    // The texts `runJavaScript` encodes in order: each ban character, then for each one the character itself and,
    // for each alternate, the alternate before and after it.
    fn text(&self, index: usize, budget: &Budget) -> Result<Text, StageError> {
        let count = self.characters.len();
        if index < count { return Ok(self.characters[index].copy(budget)?); }
        let per = 1 + 2 * self.alternates.len();
        let (character, offset) = ((index - count) / per, (index - count) % per);
        let character = &self.characters[character];
        if offset == 0 { return Ok(character.copy(budget)?); }
        let alternate = &self.alternates[(offset - 1) / 2];
        let (mut text, last) = if offset % 2 == 1 { (alternate.copy(budget)?, character) } else { (character.copy(budget)?, alternate) };
        text.append(last)?;
        Ok(text)
    }
    fn record(&mut self, ids: Vec<f64>, budget: &Budget) -> Result<(), StageError> {
        let first = ids.first().copied();
        let index = self.next;
        self.next += 1;
        if index < self.characters.len() {
            self.unban.push(first);
            return Ok(());
        }
        // `encoded.length > 0 && !unbanChars.includes(encoded[0])`, where includes compares by SameValueZero.
        let Some(id) = first else { return Ok(()) };
        budget.work(self.unban.len())?;
        if self.unban.iter().flatten().any(|unban| *unban == id || (unban.is_nan() && id.is_nan())) { return Ok(()); }
        self.bias.define(num::number_to_text(id, budget)?, Value::Number(-100.0), budget)?;
        Ok(())
    }
}

/// One `strongBan(data, bias)` call over an [`Encoder`] for the active tokenizer. With a plugin tokenizer every text
/// reaches the host in TS order, 630 of them unless the encoder's memo answers repeats.
pub struct StrongBan {
    encoder: Encoder,
    run: Option<Run>,
}

impl StrongBan {
    pub fn new(encoder: Encoder) -> Self { Self { encoder, run: None } }
    pub fn into_encoder(self) -> Encoder { self.encoder }

    /// `cached` is `localStorage.getItem(cache_key(data))`; TS returns its parsed value whenever it is nonempty.
    /// `tailored_case` says the default locale tailors case mapping (`tr`, `az`, `lt`), which `toLocaleUpperCase`
    /// follows and this stage declines as `case-locale`; a `data` the engine's `case_tables` could map differently from
    /// Rust's is declined as `case-table`. A `bias` that is not an object is declined as `strong-ban-bias`: TS writes the
    /// bans into an array as indices and, on a primitive, throws at the first ban.
    pub fn start(&mut self, data: &Text, bias: Value, cached: Option<&Text>, tailored_case: bool, case_tables: CaseTables,
        budget: &Budget) -> Result<Step<BanResult>, StageError> {
        if self.run.is_some() || self.encoder.is_pending() { return Err(StageError::Invalid(Cow::Borrowed("strong-ban-busy"))); }
        if let Some(cached) = cached.filter(|cached| cached.len_units() != 0) {
            budget.work(cached.len_units())?;
            let source = String::from_utf16(&cached.units().collect::<Vec<_>>())
                .map_err(|_| StageError::Unsupported(Cow::Borrowed("strong-ban-cache-utf16")))?;
            return Ok(Step::Done(BanResult { bias: json::parse(&source, budget)?, store: None }));
        }
        // `data[0].toLocaleUpperCase()` throws a TypeError on the empty string.
        if data.len_units() == 0 { return Err(StageError::Invalid(Cow::Borrowed("strong-ban-text"))); }
        if tailored_case { return Err(StageError::Unsupported(Cow::Borrowed("case-locale"))); }
        if !matches!(bias, Value::Object(_)) { return Err(StageError::Unsupported(Cow::Borrowed("strong-ban-bias"))); }
        let units: Vec<u16> = data.units().collect();
        budget.work(units.len().saturating_mul(ALTERNATES))?;
        let differs = match case_tables {
            CaseTables::Native => false,
            CaseTables::Unicode16 => risunest_ir::text::unicode16_case_differs(&units),
            CaseTables::Foreign => units.iter().any(|unit| *unit >= 0x80),
        };
        if differs { return Err(StageError::Unsupported(Cow::Borrowed("case-table"))); }
        let (first, rest) = (&units[..1], &units[1..]);
        let joined = |head: Vec<u16>| { let mut head = head; head.extend_from_slice(rest); head };
        let alternates = vec![
            data.copy(budget)?,
            data.trim(budget)?,
            Text::from_units(&map_case(&units, true), budget)?,
            Text::from_units(&map_case(&units, false), budget)?,
            Text::from_units(&joined(map_case(first, true)), budget)?,
            Text::from_units(&joined(map_case(first, false)), budget)?,
        ];
        let mut characters = Vec::new();
        for character in BAN_CHARACTERS.chars() {
            let mut builder = TextBuilder::new(budget)?;
            builder.character(character)?;
            characters.push(builder.finish()?);
        }
        self.run = Some(Run { characters, alternates, bias, next: 0, unban: Vec::new() });
        let step = self.advance(budget);
        self.settle(step)
    }

    pub fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<BanResult>, StageError> {
        if self.run.is_none() || !self.encoder.is_pending() { return Err(StageError::Invalid(Cow::Borrowed("strong-ban-resume"))); }
        let step = match self.encoder.resume(reply, budget) {
            Ok(Step::Done(ids)) => self.run.as_mut().expect("strong ban run").record(ids, budget).and_then(|()| self.advance(budget)),
            Ok(Step::NeedsHost(request)) => Ok(Step::NeedsHost(request)),
            Err(error) => Err(error),
        };
        self.settle(step)
    }

    fn settle(&mut self, step: Result<Step<BanResult>, StageError>) -> Result<Step<BanResult>, StageError> {
        if !matches!(step, Ok(Step::NeedsHost(_))) { self.run = None; }
        step
    }

    fn advance(&mut self, budget: &Budget) -> Result<Step<BanResult>, StageError> {
        let run = self.run.as_mut().expect("strong ban run");
        while run.next < run.len() {
            let text = run.text(run.next, budget)?;
            match self.encoder.encode(&text, budget)? {
                Step::Done(ids) => run.record(ids, budget)?,
                Step::NeedsHost(request) => return Ok(Step::NeedsHost(request)),
            }
        }
        let run = self.run.take().expect("strong ban run");
        let store = json::stringify(&run.bias, budget)?.ok_or(StageError::Invalid(Cow::Borrowed("strong-ban-store")))?;
        Ok(Step::Done(BanResult { bias: run.bias, store: Some(store) }))
    }
}

// `toUpperCase` or `toLowerCase` under the root locale: each well-formed run maps with full Unicode case mapping,
// including the final sigma rule, and an isolated surrogate stays as it is.
fn map_case(units: &[u16], upper: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(units.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<u16>| {
        if run.is_empty() { return; }
        let mapped = if upper { run.to_uppercase() } else { run.to_lowercase() };
        out.extend(mapped.encode_utf16());
        run.clear();
    };
    for unit in char::decode_utf16(units.iter().copied()) {
        match unit {
            Ok(character) => run.push(character),
            Err(error) => {
                flush(&mut run, &mut out);
                out.push(error.unpaired_surrogate());
            }
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::TokenizerId;
    use risunest_ir::stage::HostRequest;
    use risunest_ir::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    /// The case alternates follow the engine's tables only where Rust reproduces them; a cached entry needs none.
    #[test]
    fn case_alternates_decline_where_the_engine_tables_differ() {
        let limits = Limits { max_owned_bytes: 16 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 64, max_work: 1 << 32 };
        let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
        let start = |data: &str, cached: Option<&str>, tables| {
            let data = Text::from_str(data, &budget).unwrap();
            let cached = cached.map(|cached| Text::from_str(cached, &budget).unwrap());
            let bias = json::parse("{}", &budget).unwrap();
            StrongBan::new(Encoder::new(TokenizerId::Plugin)).start(&data, bias, cached.as_ref(), false, tables, &budget)
        };
        let declined = |data: &str, tables| start(data, None, tables).err() == Some(StageError::Unsupported(Cow::Borrowed("case-table")));
        let encodes = |data: &str, tables| matches!(start(data, None, tables), Ok(Step::NeedsHost(HostRequest::PluginTokenizer { .. })));
        assert!(encodes("caf\u{e9}", CaseTables::Native) && encodes("caf\u{e9}", CaseTables::Unicode16) && encodes("cafe", CaseTables::Foreign));
        assert!(declined("caf\u{e9}", CaseTables::Foreign));
        assert!(encodes("\u{a7d3}", CaseTables::Native) && declined("\u{a7d3}", CaseTables::Unicode16));
        assert!(matches!(start("caf\u{e9}", Some("{}"), CaseTables::Foreign), Ok(Step::Done(BanResult { store: None, .. }))));
    }
}
