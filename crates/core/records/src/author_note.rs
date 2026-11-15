//! Author-note preset reads, preserving the stored note and link for later edits.

use crate::root::copy;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Text, Value};

/// The link and the selected preset text. A note assignment can detach or reattach this link.
#[derive(Debug)]
pub struct Binding { fingerprint: Text, text: Option<Value> }

impl Binding {
    pub fn from_records(root: &Value, conversation: &Value, budget: &Budget) -> Result<Option<Self>, StageError> {
        let Some((id, fingerprint)) = link_fields(conversation, budget)? else { return Ok(None) };
        let mut text = Text::from_str("", budget)?;
        let presets = root.get(b"authorNotePresets", budget)?;
        if let Some(Value::Array(presets)) = presets {
            for preset in presets.as_slice() {
                budget.work(1)?;
                let (Some(Value::Text(preset_id)), Some(Value::Text(_)), Some(Value::Text(preset_text))) =
                    (preset.get(b"id", budget)?, preset.get(b"name", budget)?, preset.get(b"text", budget)?) else { continue };
                budget.work(id.len_units().saturating_add(preset_id.len_units()))?;
                if id.units().eq(preset_id.units()) { text = preset_text.copy(budget)?; break; }
            }
        }
        let text = match presets {
            None | Some(Value::Undefined | Value::Null | Value::Array(_)) => Some(Value::Text(text)),
            _ => None,
        };
        Ok(Some(Self { fingerprint: fingerprint.copy(budget)?, text }))
    }

    pub fn copy(&self, budget: &Budget) -> Result<Self, StageError> {
        Ok(Self { fingerprint: self.fingerprint.copy(budget)?, text: self.text.as_ref().map(|text| copy(text, budget)).transpose()? })
    }
}

fn link_fields<'a>(conversation: &'a Value, budget: &Budget) -> Result<Option<(&'a Text, &'a Text)>, StageError> {
    let Some(link) = conversation.get(b"authorNotePreset", budget)? else { return Ok(None) };
    Ok(match (link.get(b"id", budget)?, link.get(b"fingerprint", budget)?) {
        (Some(Value::Text(id)), Some(Value::Text(fingerprint))) => Some((id, fingerprint)),
        _ => None,
    })
}

fn matches_fingerprint(expected: &Text, note: Option<&Value>, budget: &Budget) -> Result<bool, StageError> {
    let fingerprint = fingerprint(note, budget)?;
    budget.work(expected.len_units())?;
    Ok(expected.equals_ascii(fingerprint.as_bytes()))
}

/// The currently attached preset id, without reading or copying any preset records.
pub fn matching_id<'a>(conversation: &'a Value, budget: &Budget) -> Result<Option<&'a Text>, StageError> {
    let Some((id, fingerprint)) = link_fields(conversation, budget)? else { return Ok(None) };
    Ok(matches_fingerprint(fingerprint, conversation.get(b"note", budget)?, budget)?.then_some(id))
}

/// FNV-1a over UTF-16 code units, including isolated surrogates, as `authorNoteFingerprint` computes it.
pub fn fingerprint(note: Option<&Value>, budget: &Budget) -> Result<String, StageError> {
    let mut hash: u32 = 0x811c_9dc5;
    let mut length = 0usize;
    if let Some(Value::Text(text)) = note {
        budget.work(text.len_units())?;
        for unit in text.units() { hash = (hash ^ u32::from(unit)).wrapping_mul(0x0100_0193); length += 1; }
    }
    Ok(format!("{length}:{hash:08x}"))
}

/// The latest valid preset text when the current note matches its link, empty for a deleted preset.
/// Otherwise returns the raw note, including non-string values for the caller's existing shape checks.
pub fn resolve<'a>(binding: Option<&'a Binding>, note: Option<&'a Value>, budget: &Budget)
    -> Result<Option<&'a Value>, StageError>
{
    let Some(binding) = binding else { return Ok(note) };
    if !matches_fingerprint(&binding.fingerprint, note, budget)? { return Ok(note); }
    binding.text.as_ref().map(Some).ok_or_else(|| StageError::Unsupported("author-note-presets".into()))
}


#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::{parse_json, Limits};
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn a_link_fingerprint_uses_utf16_even_for_an_isolated_surrogate() {
        let budget = Budget::new(Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 16,
            max_text_units: 1 << 16, max_values: 1 << 16, max_depth: 64, max_work: 1 << 20 },
            Arc::new(AtomicBool::new(false))).unwrap();
        let root = parse_json(r#"{"authorNotePresets":[{"id":"p","name":"Synthetic","text":"Resolved"}]}"#, &budget).unwrap();
        let conversation = parse_json(r#"{"note":"\ud800","authorNotePreset":{"id":"p","fingerprint":"1:0481d51f"}}"#, &budget).unwrap();
        assert!(matching_id(&conversation, &budget).unwrap().unwrap().equals_ascii(b"p"));
        let binding = Binding::from_records(&root, &conversation, &budget).unwrap();
        let resolved = resolve(binding.as_ref(), conversation.get(b"note", &budget).unwrap(), &budget).unwrap();
        assert!(matches!(resolved, Some(Value::Text(text)) if text.equals_ascii(b"Resolved")));
        for raw in [None, Some("null"), Some("false"), Some("17"), Some("{}"), Some("[]")] {
            let note = raw.map(|raw| format!(r#", "note":{raw}"#)).unwrap_or_default();
            let attached = parse_json(&format!(r#"{{"authorNotePreset":{{"id":"p","fingerprint":"0:811c9dc5"}}{note}}}"#), &budget).unwrap();
            assert!(matching_id(&attached, &budget).unwrap().unwrap().equals_ascii(b"p"));
        }
        let detached = parse_json(r#"{"note":"manual","authorNotePreset":{"id":"p","fingerprint":"1:0481d51f"}}"#, &budget).unwrap();
        assert!(matching_id(&detached, &budget).unwrap().is_none());
        // Capturing a detached link still allows a later assignment to reattach it.
        let binding = Binding::from_records(&root, &detached, &budget).unwrap();
        let resolved = resolve(binding.as_ref(), conversation.get(b"note", &budget).unwrap(), &budget).unwrap();
        assert!(matches!(resolved, Some(Value::Text(text)) if text.equals_ascii(b"Resolved")));
    }
}
