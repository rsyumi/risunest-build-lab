//! Everything `requestClaude` reads from outside its `arg`: the database fields.

use risunest_ir::Text;

/// `db.thinkingType`. A value other than the three names behaves like none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingType { Off, Budget, Adaptive, Other }

/// The database fields `requestClaude` and `getAdditionalParameters` read. `Debug` prints the keys and the additional
/// parameters (which can carry credentials in headers) as redacted.
pub struct Settings {
    pub claude_api_key: Text,
    pub proxy_key: Text,
    /// `db.autofillRequestUrl`.
    pub autofill_request_url: bool,
    /// `db.claude1HourCaching`.
    pub claude_one_hour_caching: bool,
    pub thinking_type: ThinkingType,
    /// `db.adaptiveThinkingEffort`; `None` is `undefined` or `null` and falls back to `high`.
    pub adaptive_thinking_effort: Option<Text>,
    /// `db.usePlainFetch`.
    pub use_plain_fetch: bool,
    /// `db.claudeBatching`. The request is built as for any other call; the batch protocol is the sans-IO machine in `batch`.
    pub claude_batching: bool,
    /// `getAdditionalParameters(aiModel)`, see [`crate::common::params::additional_parameters`].
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        formatter.debug_struct("Settings")
            .field("claude_api_key", &HIDDEN)
            .field("proxy_key", &HIDDEN)
            .field("autofill_request_url", &self.autofill_request_url)
            .field("claude_one_hour_caching", &self.claude_one_hour_caching)
            .field("thinking_type", &self.thinking_type)
            .field("adaptive_thinking_effort", &self.adaptive_thinking_effort.as_ref().map(|effort| String::from_utf16_lossy(&effort.units().collect::<Vec<_>>())))
            .field("use_plain_fetch", &self.use_plain_fetch)
            .field("claude_batching", &self.claude_batching)
            .field("additional_params", &format_args!("{} entries, {HIDDEN}", self.additional_params.len()))
            .finish()
    }
}
