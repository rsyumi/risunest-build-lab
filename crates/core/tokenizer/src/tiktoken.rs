//! The cl100k and o200k encoders.

use crate::id::{CL100K_FINGERPRINT, O200K_FINGERPRINT};
use crate::unicode::{self, Classes};
use tiktoken_rs::{cl100k_base_singleton, o200k_base_singleton, CoreBPE};

/// A `@dqbd/tiktoken` encoding, encoded the way `Tiktoken.encode(text)` does with its default arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding { Cl100kBase, O200kBase }

/// `Tiktoken.encode` rejects text that contains a special token (its default `disallowed_special` is `"all"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisallowedSpecial {
    /// The special token that starts first in the text.
    pub token: &'static str,
}

/// Why [`Encoding::encode`] or [`Encoding::count`] returns no tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// `Tiktoken.encode` throws for this text.
    DisallowedSpecial(DisallowedSpecial),
    /// The `Unsupported` name of text tiktoken-rs cannot finish: `tokenizer-regex-limit` for a whitespace run longer
    /// than 900,000 code points (tiktoken-rs panics from 999,999).
    Declined(&'static str),
}

impl Encoding {
    fn bpe(self) -> &'static CoreBPE {
        match self {
            Self::Cl100kBase => cl100k_base_singleton(),
            Self::O200kBase => o200k_base_singleton(),
        }
    }
    pub fn fingerprint(self) -> &'static str {
        match self {
            Self::Cl100kBase => CL100K_FINGERPRINT,
            Self::O200kBase => O200K_FINGERPRINT,
        }
    }
    pub fn special_tokens(self) -> &'static [&'static str] {
        match self {
            Self::Cl100kBase => &["<|endoftext|>", "<|fim_prefix|>", "<|fim_middle|>", "<|fim_suffix|>", "<|endofprompt|>"],
            Self::O200kBase => &["<|endoftext|>", "<|endofprompt|>"],
        }
    }
    /// The special token `Tiktoken.encode` rejects `text` for.
    pub fn check(self, text: &str) -> Result<(), DisallowedSpecial> {
        let first = self.special_tokens().iter().copied()
            .filter_map(|token| text.find(token).map(|position| (position, token)))
            .min_by_key(|(position, _)| *position);
        match first { Some((_, token)) => Err(DisallowedSpecial { token }), None => Ok(()) }
    }
    /// The decline [`Encoding::encode`] and [`Encoding::count`] return for `text`, found in one pass without encoding.
    pub fn screen(self, text: &str) -> Result<(), Refusal> {
        unicode::screen(text, Classes::Tiktoken).map_err(Refusal::Declined)
    }
    // Text the encoder cannot finish is declined before the special-token check, as the stage has always done.
    fn admit(self, text: &str) -> Result<(), Refusal> {
        self.screen(text)?;
        self.check(text).map_err(Refusal::DisallowedSpecial)
    }
    /// Token IDs of `text`. A JS string with an isolated surrogate reaches the WASM encoder as U+FFFD, so callers
    /// convert such text lossily first.
    pub fn encode(self, text: &str) -> Result<Vec<u32>, Refusal> {
        self.admit(text)?;
        Ok(self.bpe().encode_ordinary(text))
    }
    /// `encode(text).length`.
    pub fn count(self, text: &str) -> Result<usize, Refusal> {
        self.admit(text)?;
        Ok(self.bpe().count_ordinary(text))
    }
}
