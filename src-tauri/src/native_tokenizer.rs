use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Limits, Text};
use risunest_tokenizer::id::model::OwnedModel;
use risunest_tokenizer::id::TokenizerId;
use risunest_tokenizer::tiktoken::{DisallowedSpecial, Encoding, Refusal};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard, OnceLock, PoisonError};

const CL100K_FINGERPRINT: &str = "cl100k_base:dqbd-1.0.22:49a4e05dea02c8fafbd50cc4725c4aab8f39386c0afedef118e0dfebc2fe523a:tiktoken-rs-0.12.0:contract-1";
const O200K_FINGERPRINT: &str = "o200k_base:dqbd-1.0.22:a2c363f80642c0f07d916716b3940ff030121f3b5cef72b2c5bd1d4f64c14fb8:tiktoken-rs-0.12.0:contract-1";
const MAX_BATCH_ITEMS: usize = 1_000;
const MAX_AGGREGATE_INPUT_BYTES: usize = 1_048_576;
const MAX_RESPONSE_IDS: usize = 262_144;
// One batch's texts: at most 1 MiB of UTF-8, which the encoders copy and return IDs for.
const MODEL_LIMITS: Limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 20, max_text_units: 1 << 26,
    max_values: 1 << 20, max_depth: 64, max_work: usize::MAX / 4 };

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeTokenizerId {
    Cl100kBase,
    O200kBase,
    Mistral,
    Llama,
    #[serde(rename = "novelai")]
    NovelAi,
    #[serde(rename = "novellist")]
    NovelList,
    Claude,
    Llama3,
    Gemma,
    Cohere,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "deepseek-v4")]
    DeepSeekV4,
    Glm4,
    Glm5,
}

// What encodes a family: tiktoken-rs, or the model built from the family's artifact.
enum Engine {
    Tiktoken(Encoding),
    Model(TokenizerId),
}

impl NativeTokenizerId {
    fn engine(self) -> Engine {
        Engine::Model(match self {
            Self::Cl100kBase => return Engine::Tiktoken(Encoding::Cl100kBase),
            Self::O200kBase => return Engine::Tiktoken(Encoding::O200kBase),
            Self::Mistral => TokenizerId::Mistral,
            Self::Llama => TokenizerId::Llama,
            Self::NovelAi => TokenizerId::NovelAi,
            Self::NovelList => TokenizerId::NovelList,
            Self::Claude => TokenizerId::Claude,
            Self::Llama3 => TokenizerId::Llama3,
            Self::Gemma => TokenizerId::Gemma,
            Self::Cohere => TokenizerId::Cohere,
            Self::DeepSeek => TokenizerId::DeepSeek,
            Self::DeepSeekV4 => TokenizerId::DeepSeekV4,
            Self::Glm4 => TokenizerId::Glm4,
            Self::Glm5 => TokenizerId::Glm5,
        })
    }

    fn encoding(self) -> Encoding {
        match self.engine() {
            Engine::Tiktoken(encoding) => encoding,
            Engine::Model(_) => unreachable!("{self:?} has no tiktoken encoding"),
        }
    }

    // The tokenizer name TS uses, also the request's `tokenizer_id`.
    fn name(self) -> &'static str {
        match self {
            Self::Cl100kBase => "cl100k_base",
            Self::O200kBase => "o200k_base",
            Self::Mistral => "mistral",
            Self::Llama => "llama",
            Self::NovelAi => "novelai",
            Self::NovelList => "novellist",
            Self::Claude => "claude",
            Self::Llama3 => "llama3",
            Self::Gemma => "gemma",
            Self::Cohere => "cohere",
            Self::DeepSeek => "deepseek",
            Self::DeepSeekV4 => "deepseek-v4",
            Self::Glm4 => "glm4",
            Self::Glm5 => "glm5",
        }
    }

    /// The artifact TS and this command must both encode with: for a model family, the web-tokenizers release TS
    /// loads the model into and the SHA-256 of the model `OwnedModel::new` verifies.
    fn fingerprint(self) -> Cow<'static, str> {
        match self.engine() {
            Engine::Tiktoken(Encoding::Cl100kBase) => Cow::Borrowed(CL100K_FINGERPRINT),
            Engine::Tiktoken(Encoding::O200kBase) => Cow::Borrowed(O200K_FINGERPRINT),
            Engine::Model(id) => {
                let sha256 = id.artifact().expect("a model family has an artifact").sha256;
                Cow::Owned(format!("{}:web-tokenizers-0.1.6:{sha256}:risunest-tokenizer:contract-1", self.name()))
            }
        }
    }
}

/// Where the command reads a model family's artifact: the frontend assets the app bundles.
pub(crate) trait ModelAssets {
    /// The bytes served at `path`, the URL TS fetches the model from (`/token/...`), or `None`.
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

/// The bundled frontend assets. A release build embeds `frontendDist` in the binary on every target; a dev build with
/// `devUrl` reads that directory from disk instead, so it finds the models only after a frontend build. A path that
/// is not bundled resolves to `index.html`, which `Artifact::verify` refuses.
struct AppAssets(tauri::AppHandle);

impl ModelAssets for AppAssets {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.asset_resolver().get(path.to_owned()).map(|asset| asset.bytes)
    }
}

const MODEL_FAMILIES: [TokenizerId; 12] = [
    TokenizerId::Mistral, TokenizerId::Llama, TokenizerId::NovelAi, TokenizerId::NovelList, TokenizerId::Claude,
    TokenizerId::Llama3, TokenizerId::Gemma, TokenizerId::Cohere, TokenizerId::DeepSeek, TokenizerId::DeepSeekV4,
    TokenizerId::Glm4, TokenizerId::Glm5,
];

/// How many model families stay loaded: the ones selected last. TS keeps only the model it selected; the second spares a
/// switch back the rebuild, which takes up to about 0.8 s for the largest model in a release build.
const LOADED_MODELS: usize = 2;

/// The models of one asset source. A family's artifact is read and built on the batch that selects it, one family at a
/// time, and the model is kept while the family is among the `LOADED_MODELS` selected last. An artifact the command
/// refuses is not read again; a read that finds no asset is tried again on the next batch.
pub(crate) struct Models<A> {
    assets: A,
    refused: [OnceLock<Cow<'static, str>>; MODEL_FAMILIES.len()],
    // Most recently selected first. Batches run at once (each command runs on Tauri's blocking pool, and TS does not
    // queue them), and each holds its model until it ends, so a released model stays usable for its batch.
    loaded: Mutex<Vec<(TokenizerId, Arc<OwnedModel>)>>,
    // Held by the batch that loads a family, so a second batch that selects the family waits for that model instead of
    // building another.
    building: [Mutex<()>; MODEL_FAMILIES.len()],
    // Held while an artifact is read and built, so one model is built at a time (the largest build peaks at about
    // 330 MiB); a batch of a loaded family takes neither lock.
    build_slot: Mutex<()>,
}

impl<A: ModelAssets> Models<A> {
    pub(crate) const fn new(assets: A) -> Self {
        Self {
            assets,
            refused: [const { OnceLock::new() }; MODEL_FAMILIES.len()],
            loaded: Mutex::new(Vec::new()),
            building: [const { Mutex::new(()) }; MODEL_FAMILIES.len()],
            build_slot: Mutex::new(()),
        }
    }

    // The model of `id`, or the `Unsupported` name of a family whose model cannot be loaded.
    fn ensure(&self, id: TokenizerId) -> Result<Arc<OwnedModel>, Cow<'static, str>> {
        let index = MODEL_FAMILIES.iter().position(|family| *family == id).expect("a model family");
        // The refusal kept for the family, or its loaded model.
        let settled = || match self.refused[index].get() {
            Some(name) => Some(Err(name.clone())),
            None => self.selected(id).map(Ok),
        };
        if let Some(outcome) = settled() {
            return outcome;
        }
        let _building = self.building[index].lock().unwrap_or_else(PoisonError::into_inner);
        // A batch that waited finds the model or the refusal of the batch it waited for.
        if let Some(outcome) = settled() {
            return outcome;
        }
        let refuse = |name: Cow<'static, str>| self.refused[index].get_or_init(|| name).clone();
        if let Some(name) = id.decline() {
            return Err(refuse(Cow::Borrowed(name)));
        }
        let artifact = id.artifact().expect("a model family has an artifact");
        let _build_slot = self.build_slot.lock().unwrap_or_else(PoisonError::into_inner);
        let bytes = self.assets.read(artifact.path).ok_or(Cow::Borrowed("tokenizer-model"))?;
        // The families past the bound are released before the next model is built, and freed after the lock so that a
        // lookup does not wait for it. A batch still using a released model keeps it alive until the batch ends, so each
        // batch in flight can keep one model past the bound.
        let released = {
            let mut loaded = self.lock_loaded();
            let kept = loaded.len().min(LOADED_MODELS.saturating_sub(1));
            loaded.split_off(kept)
        };
        drop(released);
        let model = OwnedModel::new(id, &bytes).map_err(|error| refuse(match error {
            StageError::Unsupported(name) => name,
            _ => Cow::Borrowed("tokenizer-model"),
        }))?;
        let model = Arc::new(model);
        // Only the batch holding the build slot adds a model, so the release above left room for this one.
        self.lock_loaded().insert(0, (id, Arc::clone(&model)));
        Ok(model)
    }

    // The model of `id` if it is loaded, which becomes the most recently selected.
    fn selected(&self, id: TokenizerId) -> Option<Arc<OwnedModel>> {
        let mut loaded = self.lock_loaded();
        let position = loaded.iter().position(|(family, _)| *family == id)?;
        let entry = loaded.remove(position);
        let model = Arc::clone(&entry.1);
        loaded.insert(0, entry);
        Some(model)
    }

    fn lock_loaded(&self) -> MutexGuard<'_, Vec<(TokenizerId, Arc<OwnedModel>)>> {
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

// The cl100k counter of native generation, which reads no model.
struct NoAssets;

impl ModelAssets for NoAssets {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}

static NO_MODELS: Models<NoAssets> = Models::new(NoAssets);
static APP_MODELS: OnceLock<Models<AppAssets>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTokenizeMode {
    Count,
    Ids,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TokenizeBatchRequest {
    pub tokenizer_id: NativeTokenizerId,
    pub artifact_fingerprint: String,
    pub mode: NativeTokenizeMode,
    pub texts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum TokenizeBatchResponse {
    Count {
        artifact_fingerprint: String,
        counts: Vec<u32>,
    },
    Ids {
        artifact_fingerprint: String,
        ids: Vec<Vec<u32>>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NativeTokenizerError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub special_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<usize>,
}

impl NativeTokenizerError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            index: None,
            special_token: None,
            expected_fingerprint: None,
            received_fingerprint: None,
            limit: None,
            actual: None,
        }
    }
}

// The error for the batch item the encoder refuses. A decline names text tiktoken-rs cannot finish, such as a
// whitespace run past the encoder's bound (`tokenizer-regex-limit`).
fn refused(index: usize, refusal: Refusal) -> NativeTokenizerError {
    match refusal {
        Refusal::DisallowedSpecial(DisallowedSpecial { token }) => NativeTokenizerError {
            code: "disallowed_special_token".to_string(),
            message: format!("The text contains a special token that is not allowed: {token}"),
            index: Some(index),
            special_token: Some(token.to_string()),
            expected_fingerprint: None,
            received_fingerprint: None,
            limit: None,
            actual: None,
        },
        Refusal::Declined(name) => declined(Some(index), name),
    }
}

// A stage decline: of one batch item, such as text a pattern would split differently from TS, or of the whole batch
// when the family's model cannot be loaded (`tokenizer-model`, `tokenizer-artifact`, `tokenizer-huggingface`).
fn declined(index: Option<usize>, name: &str) -> NativeTokenizerError {
    NativeTokenizerError {
        index,
        ..NativeTokenizerError::new(
            &name.replace('-', "_"),
            format!("The native tokenizer declined the text: {name}"),
        )
    }
}

// The IDs of one text under a family's model, as `Tokenizer.encode` returns them in TS.
fn model_ids(model: &OwnedModel, index: usize, text: &str, budget: &Budget) -> Result<Vec<u32>, NativeTokenizerError> {
    let failed = |error: StageError| match error {
        StageError::Unsupported(name) => declined(Some(index), &name),
        error => NativeTokenizerError {
            index: Some(index),
            ..NativeTokenizerError::new("tokenizer_failed", format!("The native tokenizer failed: {error:?}"))
        },
    };
    let text = Text::from_str(text, budget).map_err(|error| failed(error.into()))?;
    model.encode(&text, budget).map_err(failed)
}

impl crate::native_log::CommandFailure for NativeTokenizerError {
    fn code(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.code)
    }

    fn expected(&self) -> bool {
        self.code != "tokenizer_worker_failed"
    }
}

fn tokenize_batch_core<A: ModelAssets>(
    request: TokenizeBatchRequest,
    models: &Models<A>,
) -> Result<TokenizeBatchResponse, NativeTokenizerError> {
    let expected_fingerprint = request.tokenizer_id.fingerprint();
    if request.artifact_fingerprint != expected_fingerprint {
        return Err(NativeTokenizerError {
            code: "unsupported_artifact_fingerprint".to_string(),
            message: "The native tokenizer artifact fingerprint is not supported.".to_string(),
            index: None,
            special_token: None,
            expected_fingerprint: Some(expected_fingerprint.to_string()),
            received_fingerprint: Some(request.artifact_fingerprint),
            limit: None,
            actual: None,
        });
    }

    if request.texts.len() > MAX_BATCH_ITEMS {
        return Err(NativeTokenizerError {
            code: "batch_item_limit_exceeded".to_string(),
            message: "Native tokenizer batch item limit exceeded.".to_string(),
            index: None,
            special_token: None,
            expected_fingerprint: None,
            received_fingerprint: None,
            limit: Some(MAX_BATCH_ITEMS),
            actual: Some(request.texts.len()),
        });
    }

    let aggregate_input_bytes = request
        .texts
        .iter()
        .try_fold(0usize, |total, text| total.checked_add(text.len()))
        .unwrap_or(usize::MAX);
    if aggregate_input_bytes > MAX_AGGREGATE_INPUT_BYTES {
        return Err(NativeTokenizerError {
            code: "aggregate_input_limit_exceeded".to_string(),
            message: "Native tokenizer aggregate UTF-8 input limit exceeded.".to_string(),
            index: None,
            special_token: None,
            expected_fingerprint: None,
            received_fingerprint: None,
            limit: Some(MAX_AGGREGATE_INPUT_BYTES),
            actual: Some(aggregate_input_bytes),
        });
    }

    // tiktoken checks every text for special tokens before any is encoded, so the first one with a special token is the
    // batch's error. A model family's model is loaded first, and the batch holds it until it ends.
    let engine = request.tokenizer_id.engine();
    let model = match engine {
        Engine::Tiktoken(encoding) => {
            for (index, text) in request.texts.iter().enumerate() {
                encoding
                    .check(text)
                    .map_err(|special| refused(index, Refusal::DisallowedSpecial(special)))?;
            }
            None
        }
        Engine::Model(id) => {
            let model = models.ensure(id).map_err(|name| declined(None, &name))?;
            let budget = Budget::new(MODEL_LIMITS, Arc::new(AtomicBool::new(false))).map_err(|error| {
                NativeTokenizerError::new("tokenizer_failed", format!("The native tokenizer failed: {error:?}"))
            })?;
            Some((model, budget))
        }
    };
    let encode = |index: usize, text: &str| match (&engine, &model) {
        (Engine::Tiktoken(encoding), _) => encoding.encode(text).map_err(|refusal| refused(index, refusal)),
        (Engine::Model(_), Some((model, budget))) => model_ids(model, index, text, budget),
        (Engine::Model(_), None) => unreachable!("a model family has a model"),
    };

    match request.mode {
        NativeTokenizeMode::Ids => {
            let mut ids = Vec::with_capacity(request.texts.len());
            let mut response_ids = 0usize;
            for (index, text) in request.texts.iter().enumerate() {
                let encoded = encode(index, text)?;
                response_ids = response_ids.saturating_add(encoded.len());
                if response_ids > MAX_RESPONSE_IDS {
                    return Err(NativeTokenizerError {
                        code: "response_id_limit_exceeded".to_string(),
                        message: "Native tokenizer ID response limit exceeded.".to_string(),
                        index: None,
                        special_token: None,
                        expected_fingerprint: None,
                        received_fingerprint: None,
                        limit: Some(MAX_RESPONSE_IDS),
                        actual: Some(response_ids),
                    });
                }
                ids.push(encoded);
            }
            Ok(TokenizeBatchResponse::Ids {
                artifact_fingerprint: request.tokenizer_id.fingerprint().to_string(),
                ids,
            })
        }
        NativeTokenizeMode::Count => {
            let counts = request
                .texts
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    let count = match &engine {
                        Engine::Tiktoken(encoding) => encoding.count(text).map_err(|refusal| refused(index, refusal))?,
                        Engine::Model(_) => encode(index, text)?.len(),
                    };
                    u32::try_from(count).map_err(|_| {
                        NativeTokenizerError::new(
                            "token_count_overflow",
                            "token count does not fit in a JavaScript-safe response",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TokenizeBatchResponse::Count {
                artifact_fingerprint: request.tokenizer_id.fingerprint().to_string(),
                counts,
            })
        }
    }
}

pub(crate) fn count_cl100k_text(text: &str) -> Result<u32, NativeTokenizerError> {
    let tokenizer_id = NativeTokenizerId::Cl100kBase;
    let response = tokenize_batch_core(TokenizeBatchRequest {
        tokenizer_id,
        artifact_fingerprint: tokenizer_id.fingerprint().into_owned(),
        mode: NativeTokenizeMode::Count,
        texts: vec![text.to_owned()],
    }, &NO_MODELS)?;
    match response {
        TokenizeBatchResponse::Count { counts, .. } => Ok(counts[0]),
        TokenizeBatchResponse::Ids { .. } => unreachable!("count request returned token IDs"),
    }
}

/// Why `count_cl100k_text` fails for `text` before it encodes anything other than a special token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cl100kDecline {
    /// More than the command's 1 MiB of UTF-8 input (`aggregate_input_limit_exceeded`).
    InputLimit,
    /// Text the encoder cannot finish (`tokenizer_regex_limit`).
    RegexLimit,
}

/// The failure `count_cl100k_text` returns for `text` other than a special token, checked in one pass without encoding.
pub(crate) fn cl100k_declines(text: &str) -> Option<Cl100kDecline> {
    if text.len() > MAX_AGGREGATE_INPUT_BYTES {
        return Some(Cl100kDecline::InputLimit);
    }
    NativeTokenizerId::Cl100kBase.encoding().screen(text).err().map(|_| Cl100kDecline::RegexLimit)
}

#[tauri::command]
pub async fn tokenize_batch(
    app: tauri::AppHandle,
    request: TokenizeBatchRequest,
) -> Result<TokenizeBatchResponse, NativeTokenizerError> {
    tokenize_batch_with(request, APP_MODELS.get_or_init(|| Models::new(AppAssets(app)))).await
}

async fn tokenize_batch_with<A: ModelAssets + Send + Sync>(
    request: TokenizeBatchRequest,
    models: &'static Models<A>,
) -> Result<TokenizeBatchResponse, NativeTokenizerError> {
    crate::native_log::logged_without_detail("tokenize_batch", async {
        tauri::async_runtime::spawn_blocking(move || tokenize_batch_core(request, models))
            .await
            .map_err(|error| {
                NativeTokenizerError::new(
                    "tokenizer_worker_failed",
                    format!("Native tokenizer worker failed: {error}"),
                )
            })?
    }.await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn core(request: TokenizeBatchRequest) -> Result<TokenizeBatchResponse, NativeTokenizerError> {
        tokenize_batch_core(request, &NO_MODELS)
    }

    async fn command(request: TokenizeBatchRequest) -> Result<TokenizeBatchResponse, NativeTokenizerError> {
        tokenize_batch_with(request, &NO_MODELS).await
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Corpus {
        cases: Vec<CorpusCase>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CorpusCase {
        name: String,
        tokenizer_id: NativeTokenizerId,
        input: CorpusInput,
        ids: Option<Vec<u32>>,
        error: Option<CorpusError>,
    }

    #[derive(Deserialize)]
    struct CorpusError {
        code: String,
        token: String,
    }

    #[derive(Deserialize)]
    #[serde(
        tag = "kind",
        rename_all = "kebab-case",
        rename_all_fields = "camelCase"
    )]
    enum CorpusInput {
        Text { value: String },
        Utf16CodeUnits { code_units: Vec<u16> },
        Repeat { value: String, count: usize },
    }

    impl CorpusInput {
        fn resolve(&self) -> String {
            match self {
                Self::Text { value } => value.clone(),
                Self::Utf16CodeUnits { code_units } => {
                    char::decode_utf16(code_units.iter().copied())
                        .map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER))
                        .collect()
                }
                Self::Repeat { value, count } => value.repeat(*count),
            }
        }
    }

    fn fingerprint(tokenizer_id: NativeTokenizerId) -> String {
        tokenizer_id.fingerprint().into_owned()
    }

    const ALL_IDS: [NativeTokenizerId; 14] = [
        NativeTokenizerId::Cl100kBase, NativeTokenizerId::O200kBase, NativeTokenizerId::Mistral, NativeTokenizerId::Llama,
        NativeTokenizerId::NovelAi, NativeTokenizerId::NovelList, NativeTokenizerId::Claude, NativeTokenizerId::Llama3,
        NativeTokenizerId::Gemma, NativeTokenizerId::Cohere, NativeTokenizerId::DeepSeek, NativeTokenizerId::DeepSeekV4,
        NativeTokenizerId::Glm4, NativeTokenizerId::Glm5,
    ];

    // The model files TS fetches, as the repository stores them under `public/`.
    struct PublicAssets;

    impl ModelAssets for PublicAssets {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            std::fs::read(format!("{}/../public{path}", env!("CARGO_MANIFEST_DIR"))).ok()
        }
    }

    static PUBLIC_MODELS: Models<PublicAssets> = Models::new(PublicAssets);

    // TS sends these fingerprints, so each must name the encoder the command runs: for a model family the artifact
    // `Artifact::verify` accepts. TS (`src/ts/tokenizer/nativeTokenizer.ts`) sends one fingerprint for each tokenizer, the
    // one the command expects for it.
    #[test]
    fn fingerprints_name_the_encoder_the_command_runs() {
        for tokenizer_id in ALL_IDS {
            assert_eq!(serde_json::to_value(tokenizer_id).unwrap(), tokenizer_id.name(), "{tokenizer_id:?}");
            let expected = match tokenizer_id.engine() {
                Engine::Tiktoken(encoding) => encoding.fingerprint().to_owned(),
                Engine::Model(id) => format!("{}:web-tokenizers-0.1.6:{}:risunest-tokenizer:contract-1", tokenizer_id.name(),
                    id.artifact().unwrap().sha256),
            };
            assert_eq!(tokenizer_id.fingerprint(), expected, "{tokenizer_id:?}");
        }
        let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../src/ts/tokenizer/nativeTokenizer.ts")).unwrap();
        let sent = regex::Regex::new(r"'([a-z0-9_-]+):(?:dqbd|web-tokenizers)-[^']*:contract-1'").unwrap();
        let mut seen = Vec::new();
        for captures in sent.captures_iter(&source) {
            let tokenizer_id: NativeTokenizerId = serde_json::from_value(serde_json::json!(&captures[1])).unwrap();
            assert_eq!(&captures[0][1..captures[0].len() - 1], tokenizer_id.fingerprint(), "{tokenizer_id:?}");
            seen.push(tokenizer_id.name());
        }
        let mut expected: Vec<&str> = ALL_IDS.iter().map(|tokenizer_id| tokenizer_id.name()).collect();
        seen.sort_unstable();
        expected.sort_unstable();
        assert_eq!(seen, expected);
    }

    // Records each model asset read, serving `bytes` for every path.
    struct Recorded {
        reads: std::sync::Mutex<Vec<String>>,
        bytes: fn(&str) -> Option<Vec<u8>>,
    }

    impl ModelAssets for Recorded {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            self.reads.lock().unwrap().push(path.to_owned());
            (self.bytes)(path)
        }
    }

    // A count batch of one text.
    fn batch<A: ModelAssets>(models: &Models<A>, tokenizer_id: NativeTokenizerId) -> Result<TokenizeBatchResponse, NativeTokenizerError> {
        let request = TokenizeBatchRequest { tokenizer_id, artifact_fingerprint: fingerprint(tokenizer_id),
            mode: NativeTokenizeMode::Count, texts: vec!["Hello world".to_owned()] };
        tokenize_batch_core(request, models)
    }

    fn artifact_path(tokenizer_id: NativeTokenizerId) -> &'static str {
        match tokenizer_id.engine() {
            Engine::Model(id) => id.artifact().unwrap().path,
            Engine::Tiktoken(_) => unreachable!("a model family"),
        }
    }

    // A family's model is read on the first batch that selects it and kept for the batches that select it next, and only
    // the recorded artifact is loaded: any other bytes (an unbundled path resolves to the app's `index.html`) decline the
    // batch and are not read again, while a read that finds no asset declines it and is tried again by the next batch.
    #[test]
    fn a_model_family_reads_its_artifact_once_and_accepts_only_the_recorded_bytes() {
        let request = |tokenizer_id: NativeTokenizerId, mode| TokenizeBatchRequest {
            tokenizer_id,
            artifact_fingerprint: fingerprint(tokenizer_id),
            mode,
            texts: vec!["Hello world".to_owned(), String::new()],
        };
        let sources: [(fn(&str) -> Option<Vec<u8>>, Option<&str>, usize); 3] = [
            (|_| None, Some("tokenizer_model"), 3),
            (|_| Some(b"<!doctype html><html></html>".to_vec()), Some("tokenizer_artifact"), 1),
            (|path| PublicAssets.read(path), None, 1),
        ];
        for (bytes, refused, mistral_reads) in sources {
            let models = Models::new(Recorded { reads: Default::default(), bytes });
            for mode in [NativeTokenizeMode::Count, NativeTokenizeMode::Ids, NativeTokenizeMode::Count] {
                match (tokenize_batch_core(request(NativeTokenizerId::Mistral, mode), &models), refused) {
                    (Err(error), Some(code)) => assert_eq!((error.code.as_str(), error.index), (code, None)),
                    (Ok(TokenizeBatchResponse::Count { counts, .. }), None) => assert_eq!(counts[1], 0),
                    (Ok(TokenizeBatchResponse::Ids { ids, .. }), None) => assert!(!ids[0].is_empty() && ids[1].is_empty()),
                    (outcome, _) => panic!("{refused:?}: {outcome:?}"),
                }
            }
            let _ = tokenize_batch_core(request(NativeTokenizerId::Llama, NativeTokenizeMode::Count), &models);
            let mut expected = vec![artifact_path(NativeTokenizerId::Mistral); mistral_reads];
            expected.push(artifact_path(NativeTokenizerId::Llama));
            assert_eq!(*models.assets.reads.lock().unwrap(), expected, "{refused:?}");
        }
    }

    // Selecting a family past `LOADED_MODELS` releases the one selected least recently, whose artifact is then read again
    // when it is selected, while the families selected after it stay loaded.
    #[test]
    fn only_the_families_selected_last_stay_loaded() {
        let families = [NativeTokenizerId::Mistral, NativeTokenizerId::Llama, NativeTokenizerId::NovelAi, NativeTokenizerId::NovelList];
        let models = Models::new(Recorded { reads: Default::default(), bytes: |path| PublicAssets.read(path) });
        let select = |tokenizer_id: NativeTokenizerId| assert!(batch(&models, tokenizer_id).is_ok(), "{tokenizer_id:?}");
        // One family past the bound, then the others again, the newest first, then the first family; the families
        // selected after it stay loaded.
        let selected = &families[..=LOADED_MODELS];
        selected.iter().chain(selected[1..].iter().rev()).for_each(|tokenizer_id| select(*tokenizer_id));
        select(selected[0]);
        selected[..LOADED_MODELS].iter().for_each(|tokenizer_id| select(*tokenizer_id));
        let expected: Vec<&str> = selected.iter().chain([&selected[0]]).map(|tokenizer_id| artifact_path(*tokenizer_id)).collect();
        assert_eq!(*models.assets.reads.lock().unwrap(), expected);
    }

    // A family whose artifact cannot be read declines its batch and releases none of the loaded families.
    #[test]
    fn a_missing_artifact_releases_no_loaded_family() {
        let models = Models::new(Recorded { reads: Default::default(),
            bytes: |path| if path.starts_with("/token/nai/") { None } else { PublicAssets.read(path) } });
        let loaded = [NativeTokenizerId::Mistral, NativeTokenizerId::Llama, NativeTokenizerId::NovelList];
        let loaded = &loaded[..LOADED_MODELS];
        loaded.iter().for_each(|tokenizer_id| assert!(batch(&models, *tokenizer_id).is_ok(), "{tokenizer_id:?}"));
        let error = batch(&models, NativeTokenizerId::NovelAi).unwrap_err();
        assert_eq!((error.code.as_str(), error.index), ("tokenizer_model", None));
        loaded.iter().for_each(|tokenizer_id| assert!(batch(&models, *tokenizer_id).is_ok(), "{tokenizer_id:?}"));
        let expected: Vec<&str> = loaded.iter().chain([&NativeTokenizerId::NovelAi]).map(|tokenizer_id| artifact_path(*tokenizer_id)).collect();
        assert_eq!(*models.assets.reads.lock().unwrap(), expected);
    }

    // A batch keeps the model it started with when batches of other families release it: it completes with that model,
    // and the loaded families are still the `LOADED_MODELS` selected last.
    #[test]
    fn a_batch_keeps_its_model_when_other_batches_release_it() {
        let models = Models::new(Recorded { reads: Default::default(), bytes: |path| PublicAssets.read(path) });
        let held = models.ensure(TokenizerId::Mistral).unwrap();
        let others = [NativeTokenizerId::Llama, NativeTokenizerId::NovelAi, NativeTokenizerId::NovelList];
        let others = &others[..LOADED_MODELS];
        others.iter().for_each(|tokenizer_id| assert!(batch(&models, *tokenizer_id).is_ok(), "{tokenizer_id:?}"));
        let budget = Budget::new(MODEL_LIMITS, Arc::new(AtomicBool::new(false))).unwrap();
        let ids = model_ids(&held, 0, "Hello world", &budget).unwrap();
        drop(held);
        others.iter().for_each(|tokenizer_id| assert!(batch(&models, *tokenizer_id).is_ok(), "{tokenizer_id:?}"));
        let request = TokenizeBatchRequest { tokenizer_id: NativeTokenizerId::Mistral,
            artifact_fingerprint: fingerprint(NativeTokenizerId::Mistral), mode: NativeTokenizeMode::Ids, texts: vec!["Hello world".to_owned()] };
        let Ok(TokenizeBatchResponse::Ids { ids: reloaded, .. }) = tokenize_batch_core(request, &models) else { panic!("Mistral batch failed") };
        assert_eq!(reloaded, [ids]);
        let mistral = artifact_path(NativeTokenizerId::Mistral);
        let expected: Vec<&str> = [mistral].into_iter().chain(others.iter().map(|tokenizer_id| artifact_path(*tokenizer_id))).chain([mistral]).collect();
        assert_eq!(*models.assets.reads.lock().unwrap(), expected);
    }

    // Serves the repository's models and records each read, holding the reads of `held` until `release`.
    struct Gated {
        held: &'static str,
        state: std::sync::Mutex<(bool, Vec<String>)>,
        changed: std::sync::Condvar,
    }

    impl Gated {
        fn wait_for_read(&self, path: &str) {
            let state = self.state.lock().unwrap();
            drop(self.changed.wait_while(state, |(_, reads)| !reads.iter().any(|read| read == path)).unwrap());
        }

        fn release(&self) {
            self.state.lock().unwrap().0 = true;
            self.changed.notify_all();
        }
    }

    impl ModelAssets for Gated {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let mut state = self.state.lock().unwrap();
            state.1.push(path.to_owned());
            self.changed.notify_all();
            drop(self.changed.wait_while(state, |(released, _)| path == self.held && !*released).unwrap());
            PublicAssets.read(path)
        }
    }

    // A batch of a loaded family goes on while another family's artifact is read and built.
    #[test]
    fn a_loaded_family_does_not_wait_for_another_family_to_load() {
        let models = &Models::new(Gated { held: artifact_path(NativeTokenizerId::Llama), state: Default::default(), changed: Default::default() });
        assert!(batch(models, NativeTokenizerId::Mistral).is_ok());
        std::thread::scope(|scope| {
            let llama = scope.spawn(|| batch(models, NativeTokenizerId::Llama));
            models.assets.wait_for_read(artifact_path(NativeTokenizerId::Llama));
            let (sender, receiver) = std::sync::mpsc::channel();
            scope.spawn(move || sender.send(batch(models, NativeTokenizerId::Mistral).is_ok()));
            let mistral = receiver.recv_timeout(std::time::Duration::from_secs(30));
            models.assets.release();
            assert!(llama.join().unwrap().is_ok());
            assert_eq!(mistral, Ok(true), "the Mistral batch waited for Llama's load");
        });
        let expected = [artifact_path(NativeTokenizerId::Mistral), artifact_path(NativeTokenizerId::Llama)];
        assert_eq!(models.assets.state.lock().unwrap().1, expected);
    }

    // A batch that selects a family while another batch loads it waits for that model instead of reading the artifact
    // again.
    #[test]
    fn a_family_is_read_once_for_the_batches_that_wait_for_its_load() {
        let llama = artifact_path(NativeTokenizerId::Llama);
        let models = &Models::new(Gated { held: llama, state: Default::default(), changed: Default::default() });
        std::thread::scope(|scope| {
            let first = scope.spawn(|| batch(models, NativeTokenizerId::Llama));
            models.assets.wait_for_read(llama);
            let second = scope.spawn(|| batch(models, NativeTokenizerId::Llama));
            // Time for the second batch to find no model and wait for the first; nothing it does marks that point.
            std::thread::sleep(std::time::Duration::from_millis(200));
            models.assets.release();
            assert!(first.join().unwrap().is_ok());
            assert!(second.join().unwrap().is_ok());
        });
        assert_eq!(models.assets.state.lock().unwrap().1, [llama]);
    }

    // A family's artifact is not read while another family's model is read and built.
    #[test]
    fn model_families_are_loaded_one_at_a_time() {
        let (llama, novelai) = (artifact_path(NativeTokenizerId::Llama), artifact_path(NativeTokenizerId::NovelAi));
        let models = &Models::new(Gated { held: llama, state: Default::default(), changed: Default::default() });
        std::thread::scope(|scope| {
            let first = scope.spawn(|| batch(models, NativeTokenizerId::Llama));
            models.assets.wait_for_read(llama);
            let second = scope.spawn(|| batch(models, NativeTokenizerId::NovelAi));
            // Time for the second batch to reach the read it must not start yet.
            std::thread::sleep(std::time::Duration::from_millis(200));
            let reads_while_loading = models.assets.state.lock().unwrap().1.clone();
            models.assets.release();
            assert!(first.join().unwrap().is_ok());
            assert!(second.join().unwrap().is_ok());
            assert_eq!(reads_while_loading, [llama]);
        });
        assert_eq!(models.assets.state.lock().unwrap().1, [llama, novelai]);
    }

    #[test]
    fn cl100k_ascii_ids_match_the_checked_in_oracle() {
        let response = core(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::Cl100kBase,
            artifact_fingerprint: CL100K_FINGERPRINT.to_string(),
            mode: NativeTokenizeMode::Ids,
            texts: vec!["hello".to_string()],
        })
        .expect("cl100k request should succeed");

        assert_eq!(
            response,
            TokenizeBatchResponse::Ids {
                artifact_fingerprint: CL100K_FINGERPRINT.to_string(),
                ids: vec![vec![15339]],
            }
        );
    }

    // The decline the stage returns where the core web-encode oracle records one (`screened` in
    // `crates/core/tokenizer/tests/oracle_tokenizer.rs`): a code point assigned after Unicode 14.0 in a class a Hugging
    // Face family other than Gemma splits by, or a run past the regex engine's limits.
    fn screened(case: &str, id: TokenizerId) -> Option<&'static str> {
        let name = case.split_once(':').map_or(case, |(_, name)| name);
        let huggingface = id.artifact().unwrap().format == risunest_tokenizer::id::ArtifactFormat::HuggingFace;
        if id == TokenizerId::Gemma || !huggingface { return None; }
        if let Some((version, class)) = name.strip_prefix("unicode-").and_then(|rest| rest.split_once('-')) {
            let split = matches!(id, TokenizerId::DeepSeek | TokenizerId::DeepSeekV4) || class.starts_with("letter") || class == "number";
            return (version != "14.0" && split).then_some("tokenizer_unicode_version");
        }
        name.ends_with("-run-over-limit").then_some("tokenizer_regex_limit")
    }

    // Every model family through the command against the IDs TS recorded with web-tokenizers over the same artifact
    // (`tests/fixtures/rust-core/tokenizer/web-encode.*`): each admitted case's IDs and count, batched as the TS seams
    // send them, and the decline of each screened case, a family at a time since selecting another family loads its
    // model. A case whose text is not well-formed UTF-16 is left out: the TS seam keeps such a batch on web-tokenizers,
    // since the command receives only well-formed text.
    #[test]
    fn model_families_return_the_ids_ts_recorded() {
        let read = |name: &str| -> serde_json::Value {
            let path = format!("{}/../tests/fixtures/rust-core/tokenizer/web-encode.{name}.json", env!("CARGO_MANIFEST_DIR"));
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        let (cases, expected) = (read("cases"), read("expected"));
        let (cases, expected) = (cases["cases"].as_array().unwrap(), expected["cases"].as_array().unwrap());
        assert_eq!(cases.len(), expected.len());
        let mut families: Vec<(NativeTokenizerId, Vec<(String, Vec<u32>)>, Vec<(String, &str, String)>)> = Vec::new();
        let (mut ill_formed, mut declines, mut unavailable) = (0, 0, 0);
        for (case, expected) in cases.iter().zip(expected) {
            assert_eq!(case["id"], expected["id"]);
            let tokenizer_id: NativeTokenizerId = serde_json::from_value(case["tokenizer"].clone()).unwrap();
            let Engine::Model(id) = tokenizer_id.engine() else { panic!("{tokenizer_id:?} is not a web tokenizer") };
            let result = &expected["result"];
            // TS fetched the model the command reads, with the recorded SHA-256.
            if let Some(fetch) = result["fetches"].as_array().unwrap().first() {
                let artifact = id.artifact().unwrap();
                assert_eq!((fetch["url"].as_str(), fetch["sha256"].as_str()), (Some(artifact.path), Some(artifact.sha256)));
            }
            let input = &case["input"];
            let text = match (input.as_str(), input.get("repeat").and_then(serde_json::Value::as_str)) {
                (Some(text), _) => text.to_owned(),
                (None, Some(unit)) => unit.repeat(input["count"].as_u64().unwrap() as usize),
                (None, None) => { ill_formed += 1; continue }
            };
            let ids: Vec<u32> = result["ids"].as_array().unwrap().iter().map(|id| id.as_u64().unwrap() as u32).collect();
            let request = |mode, texts| TokenizeBatchRequest { tokenizer_id, artifact_fingerprint: fingerprint(tokenizer_id), mode, texts };
            if let Some(decline) = id.decline() {
                let error = tokenize_batch_core(request(NativeTokenizeMode::Count, vec![text]), &PUBLIC_MODELS).unwrap_err();
                assert_eq!((error.code.as_str(), error.index), (decline.replace('-', "_").as_str(), None), "{}", case["id"]);
                unavailable += 1;
                continue;
            }
            let position = families.iter().position(|(family, ..)| *family == tokenizer_id).unwrap_or_else(|| {
                families.push((tokenizer_id, Vec::new(), Vec::new()));
                families.len() - 1
            });
            match screened(case["id"].as_str().unwrap(), id) {
                Some(decline) => families[position].2.push((text, decline, case["id"].to_string())),
                None => families[position].1.push((text, ids)),
            }
        }
        // The `{u}` cases: the shared surrogate cases of every family and the families' own surrogate inputs.
        assert_eq!(ill_formed, 63);
        let mut compared = 0;
        for (tokenizer_id, admitted, screened_cases) in families {
            for (text, decline, case) in screened_cases {
                declines += 1;
                for mode in [NativeTokenizeMode::Count, NativeTokenizeMode::Ids] {
                    let request = TokenizeBatchRequest { tokenizer_id, artifact_fingerprint: fingerprint(tokenizer_id), mode,
                        texts: vec!["safe".to_owned(), text.clone()] };
                    let error = tokenize_batch_core(request, &PUBLIC_MODELS).unwrap_err();
                    assert_eq!((error.code.as_str(), error.index), (decline, Some(1)), "{case}");
                }
            }
            let mut start = 0;
            while start < admitted.len() {
                let (mut end, mut bytes, mut returned) = (start, 0, 0);
                while end < admitted.len() && end - start < MAX_BATCH_ITEMS && bytes + admitted[end].0.len() <= MAX_AGGREGATE_INPUT_BYTES
                    && returned + admitted[end].1.len() <= MAX_RESPONSE_IDS {
                    bytes += admitted[end].0.len();
                    returned += admitted[end].1.len();
                    end += 1;
                }
                let batch = &admitted[start..end];
                let request = |mode| TokenizeBatchRequest { tokenizer_id, artifact_fingerprint: fingerprint(tokenizer_id), mode,
                    texts: batch.iter().map(|(text, _)| text.clone()).collect() };
                let Ok(TokenizeBatchResponse::Ids { ids, .. }) = tokenize_batch_core(request(NativeTokenizeMode::Ids), &PUBLIC_MODELS) else {
                    panic!("{tokenizer_id:?} IDs batch failed");
                };
                let Ok(TokenizeBatchResponse::Count { counts, .. }) = tokenize_batch_core(request(NativeTokenizeMode::Count), &PUBLIC_MODELS) else {
                    panic!("{tokenizer_id:?} count batch failed");
                };
                for (index, (text, expected)) in batch.iter().enumerate() {
                    assert_eq!(&ids[index], expected, "{tokenizer_id:?} {text:?}");
                    assert_eq!(counts[index] as usize, expected.len(), "{tokenizer_id:?} {text:?}");
                }
                compared += batch.len();
                start = end;
            }
        }
        // Every web-encode case is compared, declined or left out as ill-formed.
        let huggingface_declined = if TokenizerId::Claude.decline().is_some() { 0 } else { 2 * 12 + 5 * 7 + 2 };
        assert_eq!(declines, huggingface_declined);
        assert_eq!(compared + declines + ill_formed + unavailable, cases.len());
    }

    #[test]
    fn successful_corpus_entries_have_exact_native_ids() {
        let corpus: Corpus = serde_json::from_str(include_str!(
            "../../benchmarks/tokenizer/native-tokenizer-corpus.json"
        ))
        .expect("checked-in tokenizer corpus should parse");

        for entry in corpus.cases {
            let Some(expected_ids) = entry.ids else {
                continue;
            };
            let response = core(TokenizeBatchRequest {
                tokenizer_id: entry.tokenizer_id,
                artifact_fingerprint: fingerprint(entry.tokenizer_id).to_string(),
                mode: NativeTokenizeMode::Ids,
                texts: vec![entry.input.resolve()],
            })
            .unwrap_or_else(|error| panic!("{} failed: {error:?}", entry.name));
            let TokenizeBatchResponse::Ids { ids, .. } = response else {
                panic!("{} returned the wrong response mode", entry.name);
            };
            assert_eq!(ids, vec![expected_ids], "{} IDs differed", entry.name);
        }
    }

    #[test]
    fn count_mode_preserves_empty_and_duplicate_batch_items() {
        let response = core(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::O200kBase,
            artifact_fingerprint: O200K_FINGERPRINT.to_string(),
            mode: NativeTokenizeMode::Count,
            texts: vec![String::new(), "hello".to_string(), "hello".to_string()],
        })
        .expect("count batch should succeed");

        assert_eq!(
            response,
            TokenizeBatchResponse::Count {
                artifact_fingerprint: O200K_FINGERPRINT.to_string(),
                counts: vec![0, 1, 1],
            }
        );
    }

    #[test]
    fn special_token_errors_match_the_oracle_and_report_the_batch_index() {
        let corpus: Corpus = serde_json::from_str(include_str!(
            "../../benchmarks/tokenizer/native-tokenizer-corpus.json"
        ))
        .expect("checked-in tokenizer corpus should parse");

        for entry in corpus.cases {
            let Some(expected_error) = entry.error else {
                continue;
            };
            let error = core(TokenizeBatchRequest {
                tokenizer_id: entry.tokenizer_id,
                artifact_fingerprint: fingerprint(entry.tokenizer_id).to_string(),
                mode: NativeTokenizeMode::Ids,
                texts: vec!["safe prefix".to_string(), entry.input.resolve()],
            })
            .expect_err("special token should reject the entire batch");

            assert_eq!(
                serde_json::to_value(error).expect("error should serialize"),
                serde_json::json!({
                    "code": expected_error.code,
                    "message": format!(
                        "The text contains a special token that is not allowed: {}",
                        expected_error.token
                    ),
                    "index": 1,
                    "special_token": expected_error.token,
                }),
                "{} error differed",
                entry.name,
            );
        }
    }

    #[test]
    fn rejects_an_unreviewed_artifact_fingerprint() {
        let error = core(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::Cl100kBase,
            artifact_fingerprint: "cl100k_base:unreviewed".to_string(),
            mode: NativeTokenizeMode::Count,
            texts: vec!["hello".to_string()],
        })
        .expect_err("unreviewed artifact should be rejected");

        assert_eq!(
            serde_json::to_value(error).expect("error should serialize"),
            serde_json::json!({
                "code": "unsupported_artifact_fingerprint",
                "message": "The native tokenizer artifact fingerprint is not supported.",
                "expected_fingerprint": CL100K_FINGERPRINT,
                "received_fingerprint": "cl100k_base:unreviewed",
            })
        );
    }

    #[test]
    fn rejects_more_than_one_thousand_batch_items_before_encoding() {
        let error = core(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::Cl100kBase,
            artifact_fingerprint: CL100K_FINGERPRINT.to_string(),
            mode: NativeTokenizeMode::Count,
            texts: vec![String::new(); 1_001],
        })
        .expect_err("oversized batch should be rejected");

        assert_eq!(
            serde_json::to_value(error).expect("error should serialize"),
            serde_json::json!({
                "code": "batch_item_limit_exceeded",
                "message": "Native tokenizer batch item limit exceeded.",
                "limit": 1_000,
                "actual": 1_001,
            })
        );
    }

    #[test]
    fn rejects_more_than_one_mebibyte_of_aggregate_utf8_input() {
        let text = "x".repeat(1_048_577);
        let error = core(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::O200kBase,
            artifact_fingerprint: O200K_FINGERPRINT.to_string(),
            mode: NativeTokenizeMode::Count,
            texts: vec![text],
        })
        .expect_err("oversized input should be rejected");

        assert_eq!(
            serde_json::to_value(error).expect("error should serialize"),
            serde_json::json!({
                "code": "aggregate_input_limit_exceeded",
                "message": "Native tokenizer aggregate UTF-8 input limit exceeded.",
                "limit": 1_048_576,
                "actual": 1_048_577,
            })
        );
    }

    #[test]
    fn rejects_an_ids_response_larger_than_the_transfer_budget() {
        let result = core(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::Cl100kBase,
            artifact_fingerprint: CL100K_FINGERPRINT.to_string(),
            mode: NativeTokenizeMode::Ids,
            texts: vec!["\0".repeat(262_145)],
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("oversized IDs response should be rejected"),
        };

        assert_eq!(
            serde_json::to_value(error).expect("error should serialize"),
            serde_json::json!({
                "code": "response_id_limit_exceeded",
                "message": "Native tokenizer ID response limit exceeded.",
                "limit": 262_144,
                "actual": 262_145,
            })
        );
    }

    // tiktoken-rs panics on a whitespace run of 999,999 code points; the encoder declines a run longer than 900,000
    // instead, and both TS callers fall back to the JavaScript tokenizer on any error.
    #[test]
    fn a_whitespace_run_the_encoder_cannot_finish_is_declined_with_its_batch_index() {
        let run = format!("a{}x", " ".repeat(900_001));
        let request = |tokenizer_id: NativeTokenizerId, mode| TokenizeBatchRequest {
            tokenizer_id,
            artifact_fingerprint: tokenizer_id.fingerprint().to_string(),
            mode,
            texts: vec!["safe".to_string(), run.clone()],
        };
        for tokenizer_id in [NativeTokenizerId::Cl100kBase, NativeTokenizerId::O200kBase] {
            for mode in [NativeTokenizeMode::Count, NativeTokenizeMode::Ids] {
                let error = core(request(tokenizer_id, mode))
                    .expect_err("the run should be declined");
                assert_eq!(
                    serde_json::to_value(error).expect("error should serialize"),
                    serde_json::json!({
                        "code": "tokenizer_regex_limit",
                        "message": "The native tokenizer declined the text: tokenizer-regex-limit",
                        "index": 1,
                    }),
                    "{tokenizer_id:?} {mode:?}",
                );
            }
        }
        // Every text is still checked for special tokens before the first is encoded.
        let error = core(TokenizeBatchRequest {
            texts: vec![run.clone(), "<|endoftext|>".to_string()],
            ..request(NativeTokenizerId::O200kBase, NativeTokenizeMode::Count)
        })
        .expect_err("the special token should reject the batch");
        assert_eq!((error.code.as_str(), error.index), ("disallowed_special_token", Some(1)));
    }

    #[test]
    fn command_boundary_returns_the_serialized_count_contract() {
        let response = tauri::async_runtime::block_on(command(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::Cl100kBase,
            artifact_fingerprint: CL100K_FINGERPRINT.to_string(),
            mode: NativeTokenizeMode::Count,
            texts: vec!["hello".to_string(), "안녕하세요".to_string()],
        }))
        .expect("command should complete");

        assert_eq!(
            serde_json::to_value(response).expect("response should serialize"),
            serde_json::json!({
                "mode": "count",
                "artifact_fingerprint": CL100K_FINGERPRINT,
                "counts": [1, 5],
            })
        );
    }

    #[test]
    fn command_refusal_logs_only_the_code_without_fingerprint_or_input() {
        let private = "synthetic-private-tokenizer-input";
        let failure = tauri::async_runtime::block_on(command(TokenizeBatchRequest {
            tokenizer_id: NativeTokenizerId::Cl100kBase,
            artifact_fingerprint: private.to_owned(),
            mode: NativeTokenizeMode::Count,
            texts: vec![private.to_owned()],
        })).unwrap_err();
        assert_eq!(failure.received_fingerprint.as_deref(), Some(private));
        let entries = crate::native_log::global_state().tail(None);
        let failures: Vec<_> = entries.iter().filter(|entry| entry.message.starts_with("tokenize_batch failed: ")).collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].level, "warn");
        assert!(failures[0].message.contains(&format!("code={}", failure.code)));
        assert!(!failures[0].message.contains(private));
    }

    #[test]
    fn empty_batches_and_every_error_position_have_stable_shapes() {
        for mode in [NativeTokenizeMode::Count, NativeTokenizeMode::Ids] {
            let response = core(TokenizeBatchRequest {
                tokenizer_id: NativeTokenizerId::Cl100kBase,
                artifact_fingerprint: CL100K_FINGERPRINT.to_string(),
                mode,
                texts: Vec::new(),
            })
            .expect("empty batch should succeed");
            let serialized = serde_json::to_value(response).expect("response should serialize");
            assert_eq!(serialized["mode"], serde_json::to_value(mode).unwrap());
            assert_eq!(
                serialized[if matches!(mode, NativeTokenizeMode::Count) {
                    "counts"
                } else {
                    "ids"
                }],
                serde_json::json!([])
            );
        }

        for error_index in 0..3 {
            let mut texts = vec!["safe".to_string(); 3];
            texts[error_index] = "<|endoftext|>".to_string();
            let error = core(TokenizeBatchRequest {
                tokenizer_id: NativeTokenizerId::O200kBase,
                artifact_fingerprint: O200K_FINGERPRINT.to_string(),
                mode: NativeTokenizeMode::Ids,
                texts,
            })
            .expect_err("special token should reject the batch");
            assert_eq!(error.index, Some(error_index));
        }
    }

    #[test]
    #[ignore = "Windows release measurement"]
    fn windows_core_benchmark() {
        fn percentile(samples: &[f64], fraction: f64) -> f64 {
            let mut sorted = samples.to_vec();
            sorted.sort_by(f64::total_cmp);
            sorted[(sorted.len() as f64 * fraction).ceil() as usize - 1]
        }

        fn short_segments(count: usize) -> Vec<String> {
            (0..count)
                .map(|index| {
                    format!("chat segment {index}: user asks a concise tokenizer question.")
                })
                .collect()
        }

        let paragraph =
            "System: answer carefully. User: Explain tokenizer batching with Unicode 안녕하세요 and code `value += 1`. ";
        let mut fixtures = [1, 10, 100, 1_000]
            .into_iter()
            .map(|size| (format!("short-segments-{size}"), short_segments(size)))
            .collect::<Vec<_>>();
        fixtures.push(("prompt-32-kib".to_string(), vec!["P".repeat(32 * 1024)]));
        fixtures.push((
            "realistic-prompt-512-kib".to_string(),
            vec![paragraph
                .repeat((512usize * 1024).div_ceil(paragraph.len()))
                .chars()
                .take(512 * 1024)
                .collect()],
        ));

        let mut tokenizer_results = Vec::new();
        for tokenizer_id in [NativeTokenizerId::Cl100kBase, NativeTokenizerId::O200kBase] {
            let cold_started = Instant::now();
            let cold = core(TokenizeBatchRequest {
                tokenizer_id,
                artifact_fingerprint: tokenizer_id.fingerprint().to_string(),
                mode: NativeTokenizeMode::Count,
                texts: vec!["cold singleton initialization".to_string()],
            })
            .unwrap();
            std::hint::black_box(cold);
            let cold_ms = cold_started.elapsed().as_secs_f64() * 1_000.0;

            let mut cases = Vec::new();
            for (fixture, texts) in &fixtures {
                for mode in [NativeTokenizeMode::Count, NativeTokenizeMode::Ids] {
                    let mut samples = Vec::new();
                    for _ in 0..20 {
                        let started = Instant::now();
                        let response = core(TokenizeBatchRequest {
                            tokenizer_id,
                            artifact_fingerprint: tokenizer_id.fingerprint().to_string(),
                            mode,
                            texts: texts.clone(),
                        })
                        .unwrap();
                        std::hint::black_box(response);
                        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
                    }
                    cases.push(serde_json::json!({
                        "fixture": fixture,
                        "mode": mode,
                        "samples": samples.len(),
                        "p50Ms": percentile(&samples, 0.5),
                        "p95Ms": percentile(&samples, 0.95),
                    }));
                }
            }
            tokenizer_results.push(serde_json::json!({
                "tokenizerId": tokenizer_id,
                "coldMs": cold_ms,
                "cases": cases,
            }));
        }

        println!(
            "TOKENIZER_CORE_BENCHMARK_JSON={}",
            serde_json::json!({
                "schemaVersion": 1,
                "release": !cfg!(debug_assertions),
                "samplesPerWarmCase": 20,
                "tokenizers": tokenizer_results,
            })
        );
    }
}
