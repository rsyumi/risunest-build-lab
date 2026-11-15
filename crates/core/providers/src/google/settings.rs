//! Everything `requestGoogleCloudVertex` reads from outside its `arg`: the database fields.

use risunest_ir::Text;

/// The database fields the Google family reads. `Debug` prints the access token and the additional parameters (which
/// can carry credentials in headers) as redacted.
pub struct Settings {
    /// `db.google.accessToken`: the API key of the `GoogleCloud` endpoint when `arg.key` is empty.
    pub access_token: Text,
    /// `db.google.projectId`.
    pub project_id: Text,
    /// `db.vertexRegion`.
    pub vertex_region: Text,
    /// `db.gptVisionQuality === 'high'`.
    pub vision_high: bool,
    /// `db.saveSignatures`.
    pub save_signatures: bool,
    /// `db.jsonSchemaEnabled`.
    pub json_schema_enabled: bool,
    /// `db.antiServerOverloads`.
    pub anti_server_overloads: bool,
    /// `db.simplifiedToolUse`.
    pub simplified_tool_use: bool,
    /// `db.streamGeminiThoughts`.
    pub stream_gemini_thoughts: bool,
    /// `getAdditionalParameters(arg.aiModel)`, see [`crate::common::params::additional_parameters`].
    pub additional_params: Vec<(Text, Text)>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const HIDDEN: &str = "<redacted>";
        formatter.debug_struct("Settings")
            .field("access_token", &HIDDEN)
            .field("vertex_region", &String::from_utf16_lossy(&self.vertex_region.units().collect::<Vec<_>>()))
            .field("vision_high", &self.vision_high)
            .field("save_signatures", &self.save_signatures)
            .field("json_schema_enabled", &self.json_schema_enabled)
            .field("anti_server_overloads", &self.anti_server_overloads)
            .field("simplified_tool_use", &self.simplified_tool_use)
            .field("stream_gemini_thoughts", &self.stream_gemini_thoughts)
            .field("additional_params", &format_args!("{} entries, {HIDDEN}", self.additional_params.len()))
            .finish()
    }
}
