//! `tokenizeNum`: the token IDs `encode` returns for one text.

use super::google::{Answer, GoogleCloud, Total};
use super::{declined, encoder_input, plugin_ids, refused, Memo, PluginMemoLimits};
use crate::id::{model, TokenizerId};
use risunest_ir::stage::{HostReply, HostRequest, StageError, Step};
use risunest_ir::{Budget, Text};
use std::borrow::Cow;

/// Token IDs as JS numbers, one text at a time. Built-in encodings and registered models finish in one call; the
/// plugin tokenizer yields a `HostRequest::PluginTokenizer` and takes the IDs back in `resume`, and Google Cloud, unless
/// its memo answers, yields a `HostRequest::Http`; HTTP 200 returns zero-filled IDs of the API length, other statuses use Gemma.
pub struct Encoder {
    id: TokenizerId,
    memo: Option<Memo<Vec<f64>>>,
    google: Option<GoogleCloud>,
    // The text with the host.
    pending: Option<Text>,
}

impl Encoder {
    pub fn new(id: TokenizerId) -> Self { Self { id, memo: None, google: None, pending: None } }
    /// See [`super::Counter::with_google_cloud`].
    pub fn with_google_cloud(mut self, google: GoogleCloud) -> Self {
        self.google = Some(google);
        self
    }
    /// Reuses plugin tokenizer results like the TS `encode` cache under `useTokenizerCaching`; see
    /// [`super::Counter::with_plugin_memo`]. A [`super::Counter`] keeps its own memo, so TS, whose one cache serves
    /// both, can ask the host less often when one operation both counts and encodes the same text.
    pub fn with_plugin_memo(self) -> Self { self.with_plugin_memo_limits(PluginMemoLimits::default()) }
    pub fn with_plugin_memo_limits(mut self, limits: PluginMemoLimits) -> Self {
        self.memo = Some(Memo::new(limits));
        self
    }
    pub fn id(&self) -> TokenizerId { self.id }
    pub fn is_pending(&self) -> bool { self.pending.is_some() }

    pub fn encode(&mut self, text: &Text, budget: &Budget) -> Result<Step<Vec<f64>>, StageError> {
        if self.pending.is_some() { return Err(StageError::Invalid(Cow::Borrowed("token-encoder-busy"))); }
        budget.work(1)?;
        if let Some(encoding) = self.id.encoding() {
            let ids = encoding.encode(&encoder_input(text, budget)?).map_err(refused)?;
            budget.work(ids.len())?;
            return Ok(Step::Done(ids.into_iter().map(f64::from).collect()));
        }
        let model = model::admit(self.id)?;
        if let Some(model) = model.filter(|_| self.id != TokenizerId::GoogleCloud) {
            return Ok(Step::Done(model.encode(text, budget)?.into_iter().map(f64::from).collect()));
        }
        match self.id {
            TokenizerId::PluginMissing => return Ok(Step::Done(vec![0.0])),
            TokenizerId::Plugin => {}
            TokenizerId::GoogleCloud => {
                let google = self.google.as_ref().ok_or(StageError::Invalid(Cow::Borrowed("google-cloud-settings")))?;
                if let Some(total) = google.cached(text, budget)? { return zeros(total, budget).map(Step::Done); }
                let request = google.request(text, budget)?;
                self.pending = Some(text.copy(budget)?);
                return Ok(Step::NeedsHost(request));
            }
            id => return Err(declined(id)),
        }
        if let Some(memo) = self.memo.as_mut().filter(|memo| memo.keeps(text.len_units())) {
            budget.work(text.len_units())?;
            if let Some(ids) = memo.get(&text.units().collect::<Vec<_>>()) {
                budget.work(ids.len())?;
                return Ok(Step::Done(ids));
            }
        }
        self.pending = Some(text.copy(budget)?);
        Ok(Step::NeedsHost(HostRequest::PluginTokenizer { text: text.copy(budget)? }))
    }

    pub fn resume(&mut self, reply: HostReply, budget: &Budget) -> Result<Step<Vec<f64>>, StageError> {
        let text = self.pending.take().ok_or(StageError::Invalid(Cow::Borrowed("token-encoder-resume")))?;
        if let Some(google) = self.google.as_ref().filter(|_| self.id == TokenizerId::GoogleCloud) {
            return match google.answer(&text, reply, budget)? {
                Answer::Api(total) => zeros(total, budget).map(Step::Done),
                Answer::Gemma => Ok(Step::Done(model::get(TokenizerId::Gemma)?.encode(&text, budget)?.into_iter().map(f64::from).collect())),
            };
        }
        let ids = plugin_ids(reply)?;
        budget.work(ids.len())?;
        if let Some(memo) = self.memo.as_mut() {
            let key: Vec<u16> = text.units().collect();
            let cost = key.len().saturating_add(ids.len());
            memo.set(key, ids.clone(), cost);
        }
        Ok(Step::Done(ids))
    }
}

// The IDs of `new Uint32Array(totalTokens)`, charged before they are allocated since the API chooses the length.
fn zeros(total: Total, budget: &Budget) -> Result<Vec<f64>, StageError> {
    let length = usize::try_from(total.length()?).map_err(|_| StageError::Limit(Cow::Borrowed("owned-bytes")))?;
    budget.work(length)?;
    let _charge = budget.reserve_owned(length.saturating_mul(std::mem::size_of::<f64>()))?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(length).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
    ids.resize(length, 0.0);
    Ok(ids)
}
