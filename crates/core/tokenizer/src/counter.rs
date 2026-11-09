//! The `TokenCounter` stage and `tokenizeChat` accounting.

pub mod account;
pub mod ban;
pub mod encode;
pub mod google;

use crate::id::{model, TokenizerId};
use crate::tiktoken::{Encoding, Refusal};
use google::{Answer, GoogleCloud};
use risunest_ir::chat::{Multimodal, PromptMessage};
use risunest_ir::stage::{HostReply, HostRequest, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{Budget, Text, Value};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

/// The `ChatTokenizer` settings (`src/ts/tokenizer.ts`) that turn text counts into message counts.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatAccounting {
    /// `chatAdditionalTokens`, added once per message and once per counted multimodal entry.
    pub additional_tokens: u32,
    /// `useName === 'name'`: a nonempty `name` costs its tokens plus one.
    pub use_name: bool,
    /// `supportsInlayImage()`: the model takes image input.
    pub inlay_images: bool,
    /// `gptVisionQuality === 'low'`.
    pub low_vision_quality: bool,
}

/// Entries the TS `encode` cache keeps (`MAX_CACHE_SIZE`).
pub const PLUGIN_MEMO_ENTRIES: usize = 1500;

/// Bounds on the plugin tokenizer memo, which outlives any one count's `Budget`. The TS cache keeps any text; this
/// memo does not keep a text longer than `max_text_units`, and drops its least recently used entries to stay within
/// `entries` and `max_total_units`, so such a text reaches the host again where TS would reuse it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginMemoLimits {
    pub entries: usize,
    /// The longest text kept, in UTF-16 units.
    pub max_text_units: usize,
    /// UTF-16 units kept across all entries; an [`encode::Encoder`] memo also counts each kept ID as one unit.
    pub max_total_units: usize,
}
impl Default for PluginMemoLimits {
    fn default() -> Self { Self { entries: PLUGIN_MEMO_ENTRIES, max_text_units: 1 << 16, max_total_units: 1 << 22 } }
}

/// Counts like `tokenize`, `tokenizeChat` and `tokenizeChats` under one tokenizer. Built-in encodings and registered
/// models finish in one call; the plugin tokenizer yields one `HostRequest::PluginTokenizer` per text in TS order
/// (each message's content, then its name) and takes `HostReply::Tokens` back, and Google Cloud yields one
/// `HostRequest::Http` per text its memo does not answer and takes `HostReply::Http` back. `tokenizeChat` is counted as
/// the product calls it, without `countThoughts`. A tokenizer this crate does not run is `Unsupported` with
/// [`TokenizerId::decline`], and one whose [`TokenizerId::model`] is not registered ([`crate::id::model::register`]) is
/// `Unsupported("tokenizer-model")`, both before any host request.
pub struct Counter {
    id: TokenizerId,
    accounting: ChatAccounting,
    memo: Option<Memo<usize>>,
    google: Option<GoogleCloud>,
    pending: Option<Pending>,
}

// Each text with the tokens TS adds after it, and the tokens no tokenizer decides.
type Plan<'a> = (Vec<(&'a Text, f64)>, f64);

struct Pending {
    // Texts still to count, each with the tokens TS adds after it; the front one is with the host once requested.
    texts: VecDeque<(Text, f64)>,
    total: f64,
    requested: bool,
}

// The TS `encode` LRU, kept for plugin tokenizer results only. Each entry records the units it holds.
pub(crate) struct Memo<V> { entries: HashMap<Vec<u16>, (V, u64, usize)>, clock: u64, units: usize, limits: PluginMemoLimits }
impl<V: Clone> Memo<V> {
    pub(crate) fn new(limits: PluginMemoLimits) -> Self { Self { entries: HashMap::new(), clock: 0, units: 0, limits } }
    pub(crate) fn keeps(&self, units: usize) -> bool {
        self.limits.entries != 0 && units <= self.limits.max_text_units && units <= self.limits.max_total_units
    }
    pub(crate) fn get(&mut self, key: &[u16]) -> Option<V> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|entry| { entry.1 = clock; entry.0.clone() })
    }
    // `cost` is the units the entry holds, at least the key's length.
    pub(crate) fn set(&mut self, key: Vec<u16>, value: V, cost: usize) {
        self.clock += 1;
        if let Some(previous) = self.entries.remove(&key) { self.units -= previous.2; }
        if !self.keeps(key.len()) || cost > self.limits.max_total_units { return; }
        while self.entries.len() >= self.limits.entries || self.units + cost > self.limits.max_total_units {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, entry)| entry.1).map(|(key, _)| key.clone()) else { break };
            if let Some(entry) = self.entries.remove(&oldest) { self.units -= entry.2; }
        }
        self.units += cost;
        self.entries.insert(key, (value, self.clock, cost));
    }
}

/// The IDs of a plugin tokenizer reply: `tokenizerFunc(data) ?? [0]`, so a callback that returns nothing counts one.
pub(crate) fn plugin_ids(reply: HostReply) -> Result<Vec<f64>, StageError> {
    match reply {
        HostReply::Tokens(ids) => Ok(ids),
        HostReply::Value(Value::Undefined | Value::Null) => Ok(vec![0.0]),
        HostReply::Cancelled => Err(StageError::Cancelled),
        HostReply::Unsupported(name) => Err(StageError::Unsupported(name)),
        HostReply::Failed { message } => Err(StageError::Host(message.units().collect())),
        _ => Err(StageError::Invalid(Cow::Borrowed("plugin-tokenizer-reply"))),
    }
}

/// `Unsupported` for a tokenizer this crate does not run.
pub(crate) fn declined(id: TokenizerId) -> StageError {
    StageError::Unsupported(Cow::Borrowed(id.decline().unwrap_or("tokenizer")))
}

impl Counter {
    pub fn new(id: TokenizerId, accounting: ChatAccounting) -> Self {
        Self { id, accounting, memo: None, google: None, pending: None }
    }
    /// What the Google Cloud tokenizer reads besides the text. A Google Cloud counter without it is
    /// `Invalid("google-cloud-settings")`; other tokenizers ignore it.
    pub fn with_google_cloud(mut self, google: GoogleCloud) -> Self {
        self.google = Some(google);
        self
    }
    /// Reuses plugin tokenizer results as TS does under `useTokenizerCaching`. TS keeps its cache for the whole
    /// session, so host requests match TS only for an operation that starts with an empty TS cache; otherwise compare
    /// them as the set of distinct texts.
    pub fn with_plugin_memo(self) -> Self { self.with_plugin_memo_limits(PluginMemoLimits::default()) }
    /// [`Counter::with_plugin_memo`] under other bounds.
    pub fn with_plugin_memo_limits(mut self, limits: PluginMemoLimits) -> Self {
        self.memo = Some(Memo::new(limits));
        self
    }
    pub fn id(&self) -> TokenizerId { self.id }

    // `tokenizeMultiModal`, including its dimension quirks: only a square image is clamped to 768.
    fn multimodal(&self, data: &Multimodal) -> f64 {
        let additional = self.accounting.additional_tokens as f64;
        if !self.accounting.inlay_images { return additional; }
        if self.accounting.low_vision_quality { return 87.0; }
        let mut height = data.height.unwrap_or(0.0);
        let mut width = data.width.unwrap_or(0.0);
        if height == width {
            if height > 768.0 { height = 768.0; width = 768.0; }
        } else if height > width {
            if width > 768.0 { width = 768.0; height *= 768.0 / width; }
        } else if height > 768.0 {
            height = 768.0;
            width *= 768.0 / height;
        }
        additional + (width / 512.0).ceil() * (height / 512.0).ceil() * 2.0 + 85.0
    }

    // The texts one input encodes, in TS order, and the tokens that do not depend on the tokenizer.
    fn plan<'a>(&self, input: TokenInput<'a>, budget: &Budget) -> Result<Plan<'a>, StageError> {
        let mut texts = Vec::new();
        let mut fixed = 0.0;
        let messages = match input {
            TokenInput::Text(text) => { texts.push((text, 0.0)); return Ok((texts, fixed)); }
            TokenInput::Message(message) => std::slice::from_ref(message),
            TokenInput::Messages(messages) => messages,
        };
        for message in messages {
            budget.work(1)?;
            self.message(message, &mut texts, &mut fixed);
        }
        Ok((texts, fixed))
    }
    fn message<'a>(&self, message: &'a PromptMessage, texts: &mut Vec<(&'a Text, f64)>, fixed: &mut f64) {
        texts.push((&message.content, 0.0));
        *fixed += self.accounting.additional_tokens as f64;
        if let Some(name) = message.name.as_ref().filter(|name| self.accounting.use_name && name.len_units() != 0) {
            texts.push((name, 1.0));
        }
        for multimodal in message.multimodals.iter().flatten() { *fixed += self.multimodal(multimodal); }
    }

    // Ends the pending count unless it waits for the host, so a failed count leaves the counter free.
    fn settle(&mut self, step: Result<Step<u32>, StageError>) -> Result<Step<u32>, StageError> {
        if !matches!(step, Ok(Step::NeedsHost(_))) { self.pending = None; }
        step
    }

    fn advance(&mut self, budget: &Budget) -> Result<Step<u32>, StageError> {
        let pending = self.pending.as_mut().ok_or(StageError::Invalid(Cow::Borrowed("token-counter-resume")))?;
        while let Some((text, extra)) = pending.texts.front() {
            budget.work(1)?;
            if let Some(google) = self.google.as_ref().filter(|_| self.id == TokenizerId::GoogleCloud) {
                if let Some(total) = google.cached(text, budget)? {
                    pending.total += total.length()? as f64 + *extra;
                    pending.texts.pop_front();
                    continue;
                }
                pending.requested = true;
                return google.request(text, budget).map(Step::NeedsHost);
            }
            if let Some(memo) = self.memo.as_mut().filter(|memo| memo.keeps(text.len_units())) {
                budget.work(text.len_units())?;
                if let Some(count) = memo.get(&text.units().collect::<Vec<_>>()) {
                    pending.total += count as f64 + *extra;
                    pending.texts.pop_front();
                    continue;
                }
            }
            pending.requested = true;
            return Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: text.copy(budget)? }));
        }
        finish(pending.total).map(Step::Done)
    }

    fn reply(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<u32>, StageError> {
        let pending = self.pending.as_mut().expect("pending count");
        pending.requested = false;
        if let Some(google) = self.google.as_ref().filter(|_| self.id == TokenizerId::GoogleCloud) {
            let (text, extra) = pending.texts.pop_front().expect("requested text");
            let count = match google.answer(&text, reply, budget)? {
                Answer::Api(total) => total.length()? as f64,
                Answer::Gemma => model::get(TokenizerId::Gemma)?.encode(&text, budget)?.len() as f64,
            };
            pending.total += count + extra;
            return self.advance(budget);
        }
        let ids = plugin_ids(reply)?;
        budget.work(ids.len())?;
        let (text, extra) = pending.texts.pop_front().expect("requested text");
        pending.total += ids.len() as f64 + extra;
        if let Some(memo) = self.memo.as_mut() {
            let key: Vec<u16> = text.units().collect();
            let cost = key.len();
            memo.set(key, ids.len(), cost);
        }
        self.advance(budget)
    }
}

fn finish(total: f64) -> Result<u32, StageError> {
    if total.is_finite() && total >= 0.0 && total <= u32::MAX as f64 && total.fract() == 0.0 { Ok(total as u32) }
    else { Err(StageError::Invalid(Cow::Borrowed("token-count"))) }
}

// The string the WASM encoder receives: an isolated surrogate arrives as U+FFFD.
pub(crate) fn encoder_input<'a>(text: &'a Text, budget: &Budget) -> Result<Cow<'a, str>, StageError> {
    budget.work(text.len_units())?;
    let input = match text.as_unicode() {
        Some(source) => {
            budget.work(source.len())?;
            Cow::Borrowed(source)
        }
        None => {
            // A UTF-16 unit takes at most 3 UTF-8 bytes.
            let _charge = budget.reserve_owned(text.len_units().saturating_mul(3))?;
            let lossy = char::decode_utf16(text.units()).map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)).collect::<String>();
            budget.work(lossy.len())?;
            Cow::Owned(lossy)
        }
    };
    budget.work(input.len())?;
    Ok(input)
}

pub(crate) fn refused(refusal: Refusal) -> StageError {
    match refusal {
        Refusal::DisallowedSpecial(_) => StageError::Invalid(Cow::Borrowed("tokenizer-special-token")),
        Refusal::Declined(name) => StageError::Unsupported(Cow::Borrowed(name)),
    }
}

fn count_builtin(encoding: Encoding, text: &Text, budget: &Budget) -> Result<usize, StageError> {
    encoding.count(&encoder_input(text, budget)?).map_err(refused)
}

impl TokenCounter for Counter {
    fn decline(&self) -> Option<&'static str> { self.id.decline() }

    fn count(&mut self, input: TokenInput<'_>, budget: &Budget) -> Result<Step<u32>, StageError> {
        if self.pending.is_some() { return Err(StageError::Invalid(Cow::Borrowed("token-counter-busy"))); }
        let (texts, fixed) = self.plan(input, budget)?;
        if let Some(encoding) = self.id.encoding() {
            let mut total = fixed;
            for (text, extra) in texts { total += count_builtin(encoding, text, budget)? as f64 + extra; }
            return finish(total).map(Step::Done);
        }
        let model = model::admit(self.id)?;
        if let Some(model) = model.filter(|_| self.id != TokenizerId::GoogleCloud) {
            let mut total = fixed;
            for (text, extra) in texts { total += model.encode(text, budget)?.len() as f64 + extra; }
            return finish(total).map(Step::Done);
        }
        if self.id == TokenizerId::PluginMissing {
            let total = texts.iter().fold(fixed, |total, (_, extra)| total + 1.0 + extra);
            return finish(total).map(Step::Done);
        }
        if self.id == TokenizerId::GoogleCloud && self.google.is_none() {
            return Err(StageError::Invalid(Cow::Borrowed("google-cloud-settings")));
        }
        if !matches!(self.id, TokenizerId::Plugin | TokenizerId::GoogleCloud) { return Err(declined(self.id)); }
        let mut queue = VecDeque::new();
        for (text, extra) in texts { queue.push_back((text.copy(budget)?, extra)); }
        self.pending = Some(Pending { texts: queue, total: fixed, requested: false });
        let step = self.advance(budget);
        self.settle(step)
    }

    fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<u32>, StageError> {
        if !self.pending.as_ref().is_some_and(|pending| pending.requested) {
            return Err(StageError::Invalid(Cow::Borrowed("token-counter-resume")));
        }
        let step = self.reply(reply, budget);
        self.settle(step)
    }
}
