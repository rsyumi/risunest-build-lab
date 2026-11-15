//! Everything `requestOpenAI` reads from outside its `arg`: the database fields and the values the owner resolves.

use risunest_ir::chat::Role;
use risunest_ir::{Text, Value};

pub use crate::common::settings::{ParameterValues, SamplingSettings};

/// `db.openrouterProvider`: a list is sent only when it is non-empty.
#[derive(Debug, Default)]
pub struct OpenRouterProvider {
    pub order: Vec<Text>,
    pub only: Vec<Text>,
    pub ignore: Vec<Text>,
}

/// The database fields `requestOpenAI`, `reformater` and `getAdditionalParameters` read. `Debug` prints the keys,
/// the additional parameters (which can carry credentials in headers) and the ooba arguments as redacted.
pub struct Settings {
    /// `db.newOAIHandle`.
    pub new_oai_handle: bool,
    /// `db.gptVisionQuality`, the `detail` of every image part.
    pub gpt_vision_quality: Text,
    pub reverse_proxy_ooba_mode: bool,
    /// `db.reverseProxyOobaArgs`; entries that are `undefined` or `null` are skipped.
    pub reverse_proxy_ooba_args: Value,
    /// `db.proxyRequestModel`.
    pub proxy_request_model: Text,
    /// `db.customProxyRequestModel`.
    pub custom_proxy_request_model: Text,
    /// `db.openrouterRequestModel`, already resolved by the owner when it is `risu/free` (a network call in TS).
    pub openrouter_request_model: Text,
    /// `db.generationSeed`; sent when greater than zero.
    pub generation_seed: f64,
    /// `db.jsonSchemaEnabled`.
    pub json_schema_enabled: bool,
    /// `db.strictJsonSchema`; `None` is `undefined` and leaves `strict` out.
    pub strict_json_schema: Option<bool>,
    /// `db.OAIPrediction`.
    pub oai_prediction: Text,
    pub openrouter_fallback: bool,
    pub openrouter_middle_out: bool,
    /// `db.openrouterProvider`; `None` when the setting is absent.
    pub openrouter_provider: Option<OpenRouterProvider>,
    /// `db.useInstructPrompt`. The native builder does not render the chat template; it declines such a request.
    pub use_instruct_prompt: bool,
    /// `db.deepseekThinkingType === 'enabled'`.
    pub deepseek_thinking_enabled: bool,
    /// `db.deepseekReasoningEffort`; `None` falls back to `high`.
    pub deepseek_reasoning_effort: Option<Text>,
    /// `db.autofillRequestUrl`.
    pub autofill_request_url: bool,
    /// `db.openAIFlexProcessing`.
    pub open_ai_flex_processing: bool,
    pub openai_key: Text,
    pub proxy_key: Text,
    pub openrouter_key: Text,
    pub nanogpt_key: Text,
    pub nanogpt_request_model: Text,
    pub nanogpt_use_subscription_endpoint: bool,
    pub nanogpt_provider: Text,
    /// `db.OaiCompAPIKeys`.
    pub oai_comp_api_keys: Vec<(Text, Text)>,
    /// `db.genTime`, the `n` of a multi-output request.
    pub gen_time: f64,
    /// `getAdditionalParameters(aiModel)`, see [`super::params::additional_parameters`].
    pub additional_params: Vec<(Text, Text)>,
    /// `db.systemContentReplacement`; `None` or empty uses `system: {content}`.
    pub system_content_replacement: Option<Text>,
    /// `db.systemRoleReplacement`; `None` or empty uses `user`.
    pub system_role_replacement: Option<Role>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        formatter.debug_struct("Settings")
            .field("new_oai_handle", &self.new_oai_handle)
            .field("gpt_vision_quality", &self.gpt_vision_quality)
            .field("reverse_proxy_ooba_mode", &self.reverse_proxy_ooba_mode)
            .field("reverse_proxy_ooba_args", &HIDDEN)
            .field("proxy_request_model", &self.proxy_request_model)
            .field("custom_proxy_request_model", &self.custom_proxy_request_model)
            .field("openrouter_request_model", &self.openrouter_request_model)
            .field("generation_seed", &self.generation_seed)
            .field("json_schema_enabled", &self.json_schema_enabled)
            .field("strict_json_schema", &self.strict_json_schema)
            .field("oai_prediction", &HIDDEN)
            .field("openrouter_fallback", &self.openrouter_fallback)
            .field("openrouter_middle_out", &self.openrouter_middle_out)
            .field("openrouter_provider", &self.openrouter_provider)
            .field("use_instruct_prompt", &self.use_instruct_prompt)
            .field("deepseek_thinking_enabled", &self.deepseek_thinking_enabled)
            .field("deepseek_reasoning_effort", &self.deepseek_reasoning_effort)
            .field("autofill_request_url", &self.autofill_request_url)
            .field("open_ai_flex_processing", &self.open_ai_flex_processing)
            .field("openai_key", &HIDDEN)
            .field("proxy_key", &HIDDEN)
            .field("openrouter_key", &HIDDEN)
            .field("nanogpt_key", &HIDDEN)
            .field("nanogpt_request_model", &self.nanogpt_request_model)
            .field("nanogpt_use_subscription_endpoint", &self.nanogpt_use_subscription_endpoint)
            .field("nanogpt_provider", &self.nanogpt_provider)
            .field("oai_comp_api_keys", &format_args!("{} entries, {HIDDEN}", self.oai_comp_api_keys.len()))
            .field("gen_time", &self.gen_time)
            .field("additional_params", &format_args!("{} entries, {HIDDEN}", self.additional_params.len()))
            .field("system_content_replacement", &self.system_content_replacement)
            .field("system_role_replacement", &self.system_role_replacement)
            .finish()
    }
}
