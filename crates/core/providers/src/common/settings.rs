//! Sampling settings `applyParameters` reads.

use risunest_ir::Text;

/// The values `applyParameters` reads from the database or one separate-parameter set.
#[derive(Debug, Clone, Copy)]
pub struct ParameterValues {
    pub temperature: f64,
    pub top_k: f64,
    pub repetition_penalty: f64,
    pub min_p: f64,
    pub top_a: f64,
    pub top_p: f64,
    pub frequency_penalty: f64,
    pub presence_penalty: f64,
    pub thinking_tokens: f64,
    pub reasoning_effort: f64,
    pub verbosity: f64,
}

/// `db.seperateParameters*`; the tables are keyed by model mode and by model id.
#[derive(Debug)]
pub struct SamplingSettings {
    pub base: ParameterValues,
    pub separate_enabled: bool,
    pub separate_by_model: bool,
    pub by_mode: Vec<(Text, ParameterValues)>,
    pub overrides: Vec<(Text, ParameterValues)>,
}
