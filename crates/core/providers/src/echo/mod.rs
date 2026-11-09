//! The non-network `echo_model` provider selected by `requestChatDataMain`.

use risunest_ir::provider::{OutputShape, ProviderOutput};
use risunest_ir::{Budget, Error, Text};

#[cfg(test)]
mod replay;

#[derive(Debug)]
pub struct EchoPlan {
    /// The owner must complete this wait before delivering `output`. This is the requested
    /// timer delay, not an elapsed-time guarantee or a native timer duration conversion.
    pub requested_delay_ms: Option<f64>,
    pub output: ProviderOutput,
}

/// Plans Echo from its typed database settings. `None` represents absent or null settings;
/// an explicitly empty message remains empty. No timer or transport runs here.
pub fn plan(message: Option<Text>, delay_seconds: Option<f64>, budget: &Budget) -> Result<EchoPlan, Error> {
    budget.check()?;
    let message = match message {
        Some(message) => message,
        None => Text::from_str("Echo Message", budget)?,
    };
    let delay = delay_seconds.unwrap_or(0.0);
    Ok(EchoPlan {
        requested_delay_ms: if delay > 0.0 { Some(delay * 1000.0) } else { None },
        output: ProviderOutput {
            texts: vec![message], shape: OutputShape::Text, thoughts: Vec::new(), tool_calls: Vec::new(), usage: None, finish: None,
        },
    })
}
