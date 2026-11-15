use super::{cbs::Draws, inputs::PreparationSnapshot, modules::Evaluation, prepare, scripts::RouteScripts};
use risunest_ir::{Budget, Error as CompatError, Text, Value};
use risunest_ir::stage::{CbsConditions, ScriptMode, ScriptRequest, ScriptRunner, StageError, Step};
use risunest_prompt::response::OutputScripts;
use crate::persistent_store::generation::FailureDescriptor;

// Text admitted for storage. A failure means ordinary append wrote this fallback and then rethrew.
pub(super) struct StorageText {
    pub text: String,
    pub failure: Option<FailureDescriptor>,
}

pub(super) enum Refusal {
    Cancelled,
    Failed(FailureDescriptor),
}

pub(super) fn descriptor(category: &str, detail: Option<&str>, stage: Option<&str>) -> FailureDescriptor {
    FailureDescriptor { category: category.into(), detail: detail.map(Into::into), stage: stage.map(Into::into), http_status: None }
}

fn compat(error: CompatError, stage: &str) -> Refusal {
    match error {
        CompatError::Cancelled => Refusal::Cancelled,
        CompatError::Limit(kind) => Refusal::Failed(descriptor("limit", Some(kind), Some(stage))),
        CompatError::Allocation => Refusal::Failed(descriptor("limit", Some("allocation"), Some(stage))),
        CompatError::Type { operation } => Refusal::Failed(descriptor("type", Some(operation), Some(stage))),
        _ => Refusal::Failed(descriptor("compat", None, Some(stage))),
    }
}

// The subsequent current-chat parser runs with runVar=true. A result it could still rewrite cannot be committed here.
fn literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    !text.contains("{{") && !text.contains("{#")
        && ![b"<user>".as_slice(), b"<char>", b"<bot>"].iter().any(|tag| bytes.windows(tag.len()).any(|window| window.eq_ignore_ascii_case(tag)))
}

// Native persistence repair: paired units keep their scalar and each isolated unit becomes U+FFFD.
fn well_formed(text: &Text) -> String {
    match text.as_unicode() {
        Some(value) => value.to_owned(),
        None => char::decode_utf16(text.units()).map(|value| value.unwrap_or(char::REPLACEMENT_CHARACTER)).collect(),
    }
}

// Response CBS reads the owner's current stored context after HTTP, with the original target/preset and host
// identities. One processor takes fresh draws across its input and every replacement parse.
pub(super) struct Response {
    snapshot: Option<PreparationSnapshot>,
    evaluation: Option<Evaluation>,
    scripts: Option<RouteScripts>,
    message_count: usize,
    refreshed: bool,
    #[cfg(test)]
    entropy: Option<super::cbs::Entropy>,
}

impl Response {
    pub(super) fn new(snapshot: PreparationSnapshot) -> Self {
        Self { message_count: snapshot.history.messages.len(), snapshot: Some(snapshot), evaluation: None, scripts: None, refreshed: false,
            #[cfg(test)] entropy: None }
    }

    pub(super) fn needs_refresh(&self) -> bool { !self.refreshed }

    pub(super) fn refresh(&mut self, mut current: PreparationSnapshot) -> Result<(), Refusal> {
        let previous = self.snapshot.as_ref().expect("response refresh before parsing");
        // The journal clears host values once preparation is ready; the executing owner retains that capture.
        current.host = previous.host.clone();
        // Compare GUI identities separately from the captured send override; equivalent ID/index/default forms agree.
        if previous.selected_preset_id != current.selected_preset_id {
            return Err(Refusal::Failed(descriptor("unsupported-output", Some("preset-selection-changed"), Some("output"))));
        }
        // A changed module selection/configuration cannot reuse the captured contribution proof, and response
        // processing must not rerun prompt stages to obtain one.
        let module_fields = ["modules", "enabledModules", "moduleIntergration"];
        let changed_modules = module_fields.iter().any(|field| previous.root[*field] != current.root[*field])
            || previous.preset["moduleIntergration"] != current.preset["moduleIntergration"]
            || previous.history.character["modules"] != current.history.character["modules"]
            || previous.history.conversation["modules"] != current.history.conversation["modules"];
        let embedded = |snapshot: &PreparationSnapshot| {
            let (selected, bound) = super::inputs::selected_persona(&snapshot.root, &snapshot.history.conversation);
            [selected, bound].map(|persona| persona.and_then(|value| value.get("embeddedModule")).cloned())
        };
        if changed_modules || embedded(previous) != embedded(&current) {
            return Err(Refusal::Failed(descriptor("unsupported-output", Some("modules-changed"), Some("output"))));
        }
        self.evaluation = Some(prepare::admit_response(&current).map_err(stage_error)?);
        self.message_count = current.history.messages.len();
        self.snapshot = Some(current);
        self.refreshed = true;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn pin_entropy(&mut self, entropy: super::cbs::Entropy) { self.entropy = Some(entropy); }

    fn scripts(&mut self, budget: &Budget) -> Result<&mut RouteScripts, Refusal> {
        if self.scripts.is_none() {
            let snapshot = self.snapshot.as_ref().expect("response snapshot before its first parse");
            let draws = Draws::Fresh;
            #[cfg(test)]
            let draws = self.entropy.as_ref().map_or(draws, Draws::Replay);
            let evaluation = self.evaluation.as_ref().expect("response capture after refresh");
            let mut scripts = prepare::output_scripts(snapshot, evaluation, budget, &draws).map_err(stage_error)?;
            if !scripts.set_message_count(self.message_count) {
                return Err(Refusal::Failed(descriptor("invalid", Some("output-busy"), Some("output"))));
            }
            self.scripts = Some(scripts);
            self.evaluation = None;
            self.snapshot = None;
        }
        Ok(self.scripts.as_mut().expect("response scripts"))
    }

    // processOutput trims first, so non-text fails there; ordinary append stores ('' + value).trim() and rethrows.
    pub(super) fn storage_text(&mut self, value: &Value, budget: &Budget, max_bytes: usize) -> Result<StorageText, Refusal> {
        let (processed, failure, stage) = match value {
            Value::Text(text) => {
                let input = text.trim(budget).map_err(|error| compat(error, "output"))?;
                let chat_index = self.message_count as i64;
                let scripts = self.scripts(budget)?;
                let output = match scripts.run(ScriptRequest { mode: ScriptMode::EditOutput, input, chat_index,
                    conditions: CbsConditions::default() }, budget).map_err(stage_error)? {
                    Step::Done(output) => output,
                    Step::NeedsHost(_) => return Err(Refusal::Failed(descriptor("unsupported-output", Some("host"), Some("output")))),
                };
                if !output.effects.is_empty() || !scripts.take_emotions(budget).map_err(stage_error)?.is_empty() {
                    return Err(Refusal::Failed(descriptor("unsupported-output", Some("effect"), Some("output"))));
                }
                (output.text, None, "output")
            }
            other => {
                let coerced = other.to_text(budget).map_err(|error| compat(error, "fallback"))?;
                (coerced.trim(budget).map_err(|error| compat(error, "fallback"))?,
                    Some(descriptor("type", Some("trim"), Some("output"))), "fallback")
            }
        };
        // Bound and hash the transformed value, including native storage's isolated-surrogate repair.
        if processed.utf8_accounting_len(budget).map_err(|error| compat(error, stage))? > max_bytes {
            return Err(Refusal::Failed(descriptor("limit", Some("output-bytes"), Some(stage))));
        }
        let text = well_formed(&processed);
        if !literal(&text) { return Err(Refusal::Failed(descriptor("unsupported-output", None, Some(stage)))); }
        Ok(StorageText { text, failure })
    }
}

fn stage_error(error: StageError) -> Refusal {
    match error {
        StageError::Cancelled => Refusal::Cancelled,
        StageError::Unsupported(name) => Refusal::Failed(descriptor("unsupported-output", Some(&name), Some("output"))),
        StageError::Limit(name) => Refusal::Failed(descriptor("limit", Some(&name), Some("output"))),
        StageError::Invalid(name) => Refusal::Failed(descriptor("invalid", Some(&name), Some("output"))),
        _ => Refusal::Failed(descriptor("stage", None, Some("output"))),
    }
}
