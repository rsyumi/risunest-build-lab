//! The stored database root.
//!
//! Also the field helpers every record accessor uses: a missing or `undefined` property reads as absent, and a
//! property of the wrong type is `Invalid("<record>.<field>")`.

use risunest_ir::stage::StageError;
use risunest_ir::{Budget, OwnedVec, Text, Value};
use std::borrow::Cow;

pub(crate) fn invalid(kind: &str, key: &str) -> StageError { StageError::Invalid(Cow::Owned(format!("{kind}.{key}"))) }

/// A deep copy, as `safeStructuredClone` makes one of stored JSON.
pub fn copy(value: &Value, budget: &Budget) -> Result<Value, StageError> { copy_at(value, 0, budget) }

fn copy_at(value: &Value, depth: usize, budget: &Budget) -> Result<Value, StageError> {
    budget.work(1)?;
    if depth > budget.limits().max_depth { return Err(StageError::Limit(Cow::Borrowed("json-depth"))); }
    Ok(match value {
        Value::Undefined => Value::Undefined,
        Value::Null => Value::Null,
        Value::Bool(value) => Value::Bool(*value),
        Value::Number(value) => Value::Number(*value),
        Value::Text(value) => Value::Text(value.copy(budget)?),
        Value::Array(values) => {
            let mut output = OwnedVec::new(budget)?;
            for value in values.as_slice() { output.push(copy_at(value, depth + 1, budget)?)?; }
            Value::Array(output)
        }
        Value::Object(values) => {
            let mut output = OwnedVec::new(budget)?;
            for (key, value) in values.as_slice() { output.push((key.copy(budget)?, copy_at(value, depth + 1, budget)?))?; }
            Value::Object(output)
        }
    })
}

/// `value === undefined || value === null`.
pub fn nullish(value: Option<&Value>) -> bool { matches!(value, None | Some(Value::Undefined | Value::Null)) }

/// `value === text` for a string value.
pub fn is_text(value: Option<&Value>, text: &str) -> bool {
    matches!(value, Some(Value::Text(value)) if value.len_units() == text.len() && value.units().eq(text.encode_utf16()))
}

/// Typed reads of one record's own properties.
#[derive(Debug, Clone, Copy)]
pub struct Fields<'a> { value: &'a Value, kind: &'static str }

impl<'a> Fields<'a> {
    pub fn new(value: &'a Value, kind: &'static str) -> Result<Self, StageError> {
        if !matches!(value, Value::Object(_)) { return Err(StageError::Invalid(Cow::Borrowed(kind))); }
        Ok(Self { value, kind })
    }
    pub fn value(&self) -> &'a Value { self.value }
    pub fn kind(&self) -> &'static str { self.kind }
    pub fn invalid(&self, key: &str) -> StageError { invalid(self.kind, key) }
    pub fn get(&self, key: &str, budget: &Budget) -> Result<Option<&'a Value>, StageError> {
        Ok(self.value.get(key.as_bytes(), budget)?.filter(|value| !matches!(value, Value::Undefined)))
    }
    pub fn truthy(&self, key: &str, budget: &Budget) -> Result<bool, StageError> {
        Ok(self.get(key, budget)?.is_some_and(Value::truthy))
    }
    /// A string property; absent and `null` read as `None`.
    pub fn text(&self, key: &str, budget: &Budget) -> Result<Option<&'a Text>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Text(text)) => Ok(Some(text)),
            Some(_) => Err(self.invalid(key)),
        }
    }
    /// A string property that must be present.
    pub fn required_text(&self, key: &str, budget: &Budget) -> Result<&'a Text, StageError> {
        self.text(key, budget)?.ok_or_else(|| self.invalid(key))
    }
    pub fn number(&self, key: &str, budget: &Budget) -> Result<Option<f64>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(value)) => Ok(Some(*value)),
            Some(_) => Err(self.invalid(key)),
        }
    }
    pub fn array(&self, key: &str, budget: &Budget) -> Result<Option<&'a [Value]>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Array(values)) => Ok(Some(values.as_slice())),
            Some(_) => Err(self.invalid(key)),
        }
    }
    pub fn object(&self, key: &str, kind: &'static str, budget: &Budget) -> Result<Option<Fields<'a>>, StageError> {
        match self.get(key, budget)? {
            None | Some(Value::Null) => Ok(None),
            Some(value @ Value::Object(_)) => Ok(Some(Fields { value, kind })),
            Some(_) => Err(self.invalid(key)),
        }
    }
}

/// The stored root record. A stored root names the selected preset and persona by ID.
#[derive(Debug, Clone, Copy)]
pub struct Root<'a>(Fields<'a>);

impl<'a> Root<'a> {
    pub fn new(value: &'a Value) -> Result<Self, StageError> { Ok(Self(Fields::new(value, "root")?)) }
    pub fn fields(&self) -> Fields<'a> { self.0 }
    pub fn value(&self) -> &'a Value { self.0.value() }
    /// `botPresetsId`, the ID of the selected preset.
    pub fn preset_id(&self, budget: &Budget) -> Result<Option<&'a Text>, StageError> { self.0.text("botPresetsId", budget) }
    pub fn personas(&self, budget: &Budget) -> Result<&'a [Value], StageError> {
        Ok(self.0.array("personas", budget)?.unwrap_or_default())
    }
    /// `selectedPersona` as stored: an ID, or an index into `personas` in a root that predates IDs.
    pub fn selected_persona(&self, budget: &Budget) -> Result<Option<&'a Value>, StageError> {
        Ok(self.0.get("selectedPersona", budget)?.filter(|value| !matches!(value, Value::Null)))
    }
}
