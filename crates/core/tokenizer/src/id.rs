//! Tokenizer IDs, the choice `encode` (`src/ts/tokenizer.ts`) makes between them, and artifact fingerprints.

pub mod sentencepiece;
#[cfg(feature = "huggingface")]
pub(crate) mod huggingface;
pub mod model;

use crate::tiktoken::Encoding;
use risunest_ir::stage::StageError;
use risunest_ir::{Text, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;

/// The reviewed `@dqbd/tiktoken` artifacts these encoders reproduce, as the app's native tokenizer command names them.
pub const CL100K_FINGERPRINT: &str = "cl100k_base:dqbd-1.0.22:49a4e05dea02c8fafbd50cc4725c4aab8f39386c0afedef118e0dfebc2fe523a:tiktoken-rs-0.12.0:contract-1";
pub const O200K_FINGERPRINT: &str = "o200k_base:dqbd-1.0.22:a2c363f80642c0f07d916716b3940ff030121f3b5cef72b2c5bd1d4f64c14fb8:tiktoken-rs-0.12.0:contract-1";

/// The tokenizer a counter runs, chosen from the settings by [`select`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TokenizerId {
    /// `tikJS(data, 'cl100k_base')`.
    Cl100kBase,
    /// `tikJS(data, 'o200k_base')`.
    O200kBase,
    /// The plugin provider's `tokenizerFunc`, selected by the `custom` plugin tokenizer.
    Plugin,
    /// The `custom` plugin tokenizer of a provider without `tokenizerFunc`: every text encodes to `[0]`.
    PluginMissing,
    Mistral,
    Llama,
    #[serde(rename = "novelai")]
    NovelAi,
    Claude,
    #[serde(rename = "novellist")]
    NovelList,
    Llama3,
    Gemma,
    Cohere,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "deepseek-v4")]
    DeepSeekV4,
    Glm4,
    Glm5,
    /// `tokenizeGoogleCloud`: the Gemini `countTokens` API, and Gemma when it answers other than 200.
    GoogleCloud,
    /// `tokenizeTransformers` with the tokenizer of the `hf:::` model in `aiModel`.
    Local,
}

/// How an artifact is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactFormat {
    /// A `@dqbd/tiktoken` encoder JSON (`bpe_ranks`, `special_tokens`, `pat_str`).
    Tiktoken,
    /// A Hugging Face `tokenizer.json`, loaded with `Tokenizer.fromJSON`.
    HuggingFace,
    /// A SentencePiece `ModelProto`, loaded with `Tokenizer.fromSentencePiece`.
    SentencePiece,
}

/// A tokenizer artifact as the repository stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Artifact {
    /// What TS loads: the module it imports, or the URL it fetches from `public/`.
    pub path: &'static str,
    pub format: ArtifactFormat,
    /// The fingerprint [`Artifact::verify`] checks: SHA-256 of the artifact bytes in lowercase hex, reading CRLF as LF in
    /// a JSON artifact, and the length of those bytes.
    pub sha256: &'static str,
    pub bytes: u64,
}

const fn artifact(path: &'static str, format: ArtifactFormat, sha256: &'static str, bytes: u64) -> Artifact {
    Artifact { path, format, sha256, bytes }
}

impl Artifact {
    /// Accepts loaded bytes only when their fingerprint is the recorded one. A JSON artifact reads CRLF as LF, since a
    /// checkout that converts line endings changes only whitespace there (a JSON string cannot hold a raw CR); a
    /// SentencePiece model is read byte for byte. Other bytes would build a tokenizer that counts differently from TS,
    /// so they are declined as `tokenizer-artifact`.
    pub fn verify(&self, loaded: &[u8]) -> Result<(), StageError> {
        let mut hasher = Sha256::new();
        let mut length = 0;
        let mut start = 0;
        if self.format != ArtifactFormat::SentencePiece {
            for (index, pair) in loaded.windows(2).enumerate() {
                if pair == b"\r\n" {
                    hasher.update(&loaded[start..index]);
                    length += index - start;
                    start = index + 1;
                }
            }
        }
        hasher.update(&loaded[start..]);
        length += loaded.len() - start;
        let digest = hasher.finalize();
        let recorded = self.sha256.as_bytes();
        let same = self.bytes == length as u64 && recorded.len() == 2 * digest.len() && digest.iter().enumerate().all(|(index, byte)| {
            std::str::from_utf8(&recorded[index * 2..index * 2 + 2]).ok().and_then(|pair| u8::from_str_radix(pair, 16).ok()) == Some(*byte)
        });
        if same { Ok(()) } else { Err(StageError::Unsupported(Cow::Borrowed("tokenizer-artifact"))) }
    }
}

impl TokenizerId {
    /// The built-in encoding, or `None` when this crate does not encode locally.
    pub fn encoding(self) -> Option<Encoding> {
        match self {
            Self::Cl100kBase => Some(Encoding::Cl100kBase),
            Self::O200kBase => Some(Encoding::O200kBase),
            _ => None,
        }
    }
    pub fn fingerprint(self) -> Option<&'static str> { self.encoding().map(Encoding::fingerprint) }
    /// The tokenizer whose model [`model::register`] must hold before this one counts: itself for a SentencePiece or
    /// Hugging Face artifact, Gemma for Google Cloud (its fallback when `countTokens` answers other than 200), and
    /// `None` for a tokenizer without a model.
    pub fn model(self) -> Option<TokenizerId> {
        match self {
            Self::GoogleCloud => Some(Self::Gemma),
            _ => self.artifact().filter(|artifact| artifact.format != ArtifactFormat::Tiktoken).map(|_| self),
        }
    }

    /// The artifact TS loads for this tokenizer. The plugin, Google Cloud and local tokenizers have none.
    pub fn artifact(self) -> Option<Artifact> {
        use ArtifactFormat::{HuggingFace, SentencePiece, Tiktoken};
        Some(match self {
            Self::Cl100kBase => artifact("@dqbd/tiktoken/encoders/cl100k_base.json", Tiktoken,
                "49a4e05dea02c8fafbd50cc4725c4aab8f39386c0afedef118e0dfebc2fe523a", 1_090_747),
            Self::O200kBase => artifact("src/etc/o200k_base.json", Tiktoken,
                "a2c363f80642c0f07d916716b3940ff030121f3b5cef72b2c5bd1d4f64c14fb8", 2_325_457),
            Self::Mistral => artifact("/token/mistral/tokenizer.model", SentencePiece,
                "dadfd56d766715c61d2ef780a525ab43b8e6da4de6865bda3d95fdef5e134055", 493_443),
            Self::Llama => artifact("/token/llama/llama.model", SentencePiece,
                "9e556afd44213b6bd1be2b850ebbbd98f5481437a8021afaf58ee7fb1818d347", 499_723),
            Self::NovelAi => artifact("/token/nai/nerdstash_v2.model", SentencePiece,
                "005ad680b10f1abd406bdb0ca9c6a5d83fc1f6e0a855bdd1942c1ceab1fb47ab", 1_033_744),
            Self::Claude => artifact("/token/claude/claude.json", HuggingFace,
                "c241737df24b4e7f7c9af4fdcee29a0ca903dcb288a8b753bc346a3092911767", 1_774_213),
            Self::NovelList => artifact("/token/trin/spiece.model", SentencePiece,
                "ec31a22c0dca80dc303894ca87547154961b22e312c10c6fab751db7c4584439", 1_186_989),
            Self::Llama3 => artifact("/token/llama/llama3.json", HuggingFace,
                "c05a3c2174e9edd5be19dc5a0748c42a9037bec2811ce062728bfd71f8702d78", 9_084_490),
            Self::Gemma => artifact("/token/gemma/gemma4.json", HuggingFace,
                "4453a86456cdf02b3c8f79091ffda7ca017619ff5da495b9eda12141d5793962", 13_593_370),
            Self::Cohere => artifact("/token/cohere/tokenizer.json", HuggingFace,
                "5e07ab012ff6144f924acb0c46e8462ade9119d6375a7712a170dc7620291493", 12_777_505),
            Self::DeepSeek => artifact("/token/deepseek/tokenizer.json", HuggingFace,
                "ecb6f9fc369894346f0511f4074ca75cee5cd5f3b06d02f1ba35fcd39f8e121d", 7_847_602),
            Self::DeepSeekV4 => artifact("/token/deepseek/v4/tokenizer.json", HuggingFace,
                "3ad46a8c78edf140516eb696f948ae6e7c3522ec64993b219833145e2e9951d0", 4_363_807),
            Self::Glm4 => artifact("/token/glm4/tokenizer.json", HuggingFace,
                "1dfc31b811042be1dab7f489cd8e677eb014caa3f435d9543ae4807b27cb65ce", 8_578_513),
            Self::Glm5 => artifact("/token/glm5/tokenizer.json", HuggingFace,
                "f73933d2e4b633ce5a75f049a68b339790c52b966025e895aae0a0eda67d5cda", 8_683_422),
            Self::Plugin | Self::PluginMissing | Self::GoogleCloud | Self::Local => return None,
        })
    }

    /// The `Unsupported` name counters return for this tokenizer, or `None` when this crate counts with it. A build
    /// without the `huggingface` feature declines the Hugging Face families and Google Cloud, whose fallback is Gemma.
    pub fn decline(self) -> Option<&'static str> {
        Some(match self {
            Self::Cl100kBase | Self::O200kBase | Self::Plugin | Self::PluginMissing => return None,
            Self::Mistral | Self::Llama | Self::NovelAi | Self::NovelList => return None,
            Self::Claude | Self::DeepSeek | Self::DeepSeekV4 | Self::Llama3 | Self::Glm4 | Self::Glm5
                | Self::Gemma | Self::Cohere | Self::GoogleCloud => {
                if cfg!(feature = "huggingface") { return None; }
                "tokenizer-huggingface"
            }
            Self::Local => "tokenizer-local",
        })
    }
}

/// `LLMTokenizer` (`src/ts/model/types.ts`), the `tokenizer` of `getModelInfo(db.aiModel)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelTokenizer {
    Unknown,
    Cl100kBase,
    O200kBase,
    Mistral,
    Llama,
    NovelAi,
    Claude,
    NovelList,
    Llama3,
    Gemma,
    GoogleCloud,
    Cohere,
    Local,
    DeepSeek,
    DeepSeekV4,
    Glm4,
    Glm5,
    /// A value no `LLMTokenizer` member equals, such as a custom model's missing or non-number `tokenizer`.
    Other,
}

impl ModelTokenizer {
    const MEMBERS: [Self; 17] = [
        Self::Unknown, Self::Cl100kBase, Self::O200kBase, Self::Mistral, Self::Llama, Self::NovelAi, Self::Claude,
        Self::NovelList, Self::Llama3, Self::Gemma, Self::GoogleCloud, Self::Cohere, Self::Local, Self::DeepSeek,
        Self::DeepSeekV4, Self::Glm4, Self::Glm5,
    ];
    /// The member `encode` finds equal (`===`) to `value`.
    pub fn from_number(value: f64) -> Self {
        if (0.0..=16.0).contains(&value) && value.fract() == 0.0 { Self::MEMBERS[value as usize] } else { Self::Other }
    }
    pub fn from_value(value: &Value) -> Self {
        match value { Value::Number(value) => Self::from_number(*value), _ => Self::Other }
    }
}

/// The options of the current plugin provider, `pluginV2.providerOptions.get(db.currentPluginProvider)`.
#[derive(Debug, Clone, Copy)]
pub struct PluginOptions<'a> {
    /// `tokenizer`, or `None` when it is nullish, which `encode` reads as `"none"`. An owner passes a non-string value
    /// by its truthiness: a truthy one as any text no tokenizer name equals, a falsy one as the empty text.
    pub tokenizer: Option<&'a Text>,
    /// `typeof tokenizerFunc === 'function'`.
    pub tokenizer_function: bool,
}

/// What `encode` reads to choose a tokenizer: the settings its cache key (`getHash`) lists.
#[derive(Debug, Clone, Copy)]
pub struct TokenizerSettings<'a> {
    /// `db.aiModel`.
    pub ai_model: &'a Text,
    /// `db.customTokenizer`, or `None` when it is not a string.
    pub custom_tokenizer: Option<&'a Text>,
    /// The truthiness of `db.googleClaudeTokenizing`.
    pub google_claude_tokenizing: bool,
    /// `getModelInfo(db.aiModel).tokenizer`.
    pub model_tokenizer: ModelTokenizer,
    /// `None` when the current plugin provider has no options.
    pub plugin: Option<PluginOptions<'a>>,
}

fn starts_with_ascii(text: &Text, prefix: &[u8]) -> bool {
    text.len_units() >= prefix.len() && text.units().zip(prefix).all(|(unit, byte)| unit == *byte as u16)
}

// The tokenizers both switch statements of `encode` name.
fn named(name: &Text) -> Option<TokenizerId> {
    const NAMES: [(&str, TokenizerId); 12] = [
        ("mistral", TokenizerId::Mistral), ("llama", TokenizerId::Llama), ("novelai", TokenizerId::NovelAi),
        ("claude", TokenizerId::Claude), ("novellist", TokenizerId::NovelList), ("llama3", TokenizerId::Llama3),
        ("gemma", TokenizerId::Gemma), ("cohere", TokenizerId::Cohere), ("deepseek", TokenizerId::DeepSeek),
        ("deepseek-v4", TokenizerId::DeepSeekV4), ("glm4", TokenizerId::Glm4), ("glm5", TokenizerId::Glm5),
    ];
    NAMES.iter().find(|(key, _)| name.equals_ascii(key.as_bytes())).map(|(_, id)| *id)
}

/// The tokenizer `encodeWithTokenizer(data, name)` runs: `o200k_base`, a `tokenizerList` name other than `tik`, and
/// cl100k for `tik` and every other name.
pub fn encode_with_tokenizer(name: &Text) -> TokenizerId {
    if name.equals_ascii(b"o200k_base") { return TokenizerId::O200kBase; }
    named(name).unwrap_or(TokenizerId::Cl100kBase)
}

/// The tokenizer `encode` runs under `settings`. `openrouter` and `reverse_proxy` read `customTokenizer` and fall
/// back to o200k (so `tik` selects o200k there); `custom` reads the plugin provider's tokenizer, also falling back to
/// o200k, unless that tokenizer is the empty text; everything else follows the model's `LLMTokenizer`, where
/// `Unknown`, `tiktokenCl100kBase`, `Llama3` and any other value reach cl100k. A `Local` model outside `hf:::` is the
/// error TS throws.
pub fn select(settings: &TokenizerSettings<'_>) -> Result<TokenizerId, StageError> {
    let model = settings.ai_model;
    if model.equals_ascii(b"openrouter") || model.equals_ascii(b"reverse_proxy") {
        return Ok(settings.custom_tokenizer.and_then(named).unwrap_or(TokenizerId::O200kBase));
    }
    if model.equals_ascii(b"custom") {
        let tokenizer = settings.plugin.and_then(|plugin| plugin.tokenizer);
        if tokenizer.is_none_or(|name| name.len_units() != 0) {
            let function = settings.plugin.is_some_and(|plugin| plugin.tokenizer_function);
            return Ok(match tokenizer {
                Some(name) if name.equals_ascii(b"cl100k_base") => TokenizerId::Cl100kBase,
                Some(name) if name.equals_ascii(b"custom") => if function { TokenizerId::Plugin } else { TokenizerId::PluginMissing },
                Some(name) => named(name).unwrap_or(TokenizerId::O200kBase),
                None => TokenizerId::O200kBase,
            });
        }
    }
    Ok(match settings.model_tokenizer {
        ModelTokenizer::NovelList => TokenizerId::NovelList,
        ModelTokenizer::Claude => TokenizerId::Claude,
        ModelTokenizer::NovelAi => TokenizerId::NovelAi,
        ModelTokenizer::Mistral => TokenizerId::Mistral,
        ModelTokenizer::Llama => TokenizerId::Llama,
        ModelTokenizer::Local if starts_with_ascii(model, b"hf:::") => TokenizerId::Local,
        ModelTokenizer::Local => return Err(StageError::Invalid(Cow::Borrowed("local-tokenizer-model"))),
        ModelTokenizer::O200kBase => TokenizerId::O200kBase,
        ModelTokenizer::GoogleCloud if settings.google_claude_tokenizing => TokenizerId::GoogleCloud,
        ModelTokenizer::Gemma | ModelTokenizer::GoogleCloud => TokenizerId::Gemma,
        ModelTokenizer::DeepSeek => TokenizerId::DeepSeek,
        ModelTokenizer::DeepSeekV4 => TokenizerId::DeepSeekV4,
        ModelTokenizer::Glm4 => TokenizerId::Glm4,
        ModelTokenizer::Glm5 => TokenizerId::Glm5,
        ModelTokenizer::Cohere => TokenizerId::Cohere,
        ModelTokenizer::Unknown | ModelTokenizer::Cl100kBase | ModelTokenizer::Llama3 | ModelTokenizer::Other => TokenizerId::Cl100kBase,
    })
}
