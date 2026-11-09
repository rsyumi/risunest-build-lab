use crate::budget::{Budget, Error, OwnedVec};
use crate::text::Text;

#[derive(Debug)]
pub enum Value {
    /// Exists only in evaluator results; stored and parsed JSON never contains it.
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Text(Text),
    Array(OwnedVec<Value>),
    /// Own properties in JS order: array-index keys first in ascending order, then the other keys in insertion order,
    /// each key once. `parse_json`, [`Value::define`] and the `chat` conversions build objects in this order, and
    /// `json::stringify` writes members in stored order, so an object built another way must keep it too.
    Object(OwnedVec<(Text, Value)>),
}

// [[DefineOwnProperty]] order: array-index keys first in ascending order, then other keys in insertion order; an
// existing key keeps its position and takes the new value, as `json::parse` and `Object.assign` do.
pub(crate) fn define_property(properties: &mut OwnedVec<(Text, Value)>, key: Text, value: Value, budget: &Budget) -> Result<(), Error> {
    budget.work(1)?;
    let length = properties.data.len();
    let index = key.array_index_key();
    let mut position = length;
    let mut existing = None;
    for (at, (name, _)) in properties.as_slice().iter().enumerate() {
        budget.work(name.len_units().min(key.len_units()).saturating_add(1))?;
        if name.equals(&key) { existing = Some(at); break; }
        if let Some(index) = index {
            if position == length && name.array_index_key().is_none_or(|old| old > index) { position = at; }
        }
    }
    if let Some(at) = existing { properties.as_mut_slice()[at].1 = value; return Ok(()); }
    budget.work(length - position)?;
    properties.push((key, value))?;
    properties.data[position..].rotate_right(1);
    Ok(())
}

impl Value {
    /// Defines an own data property as `Object.defineProperty` does on an ordinary object, keeping the [`Value::Object`]
    /// order: an existing key keeps its position and takes `value`, a new array-index key goes before every larger index
    /// and every other key, and any other new key, `__proto__` included, goes last. Charges `work` for the keys it
    /// compares and moves; a limit error leaves the object unchanged. A value that is not an object is a `Type` error.
    pub fn define(&mut self, key: Text, value: Value, budget: &Budget) -> Result<(), Error> {
        match self {
            Self::Object(properties) => define_property(properties, key, value, budget),
            _ => Err(Error::Type { operation: "define-property" }),
        }
    }
    // Own-object lookup only. The SDK operation decides whether a non-object receiver is a TypeError.
    pub fn get(&self, key: &[u8], budget: &Budget) -> Result<Option<&Self>, Error> {
        budget.work(1)?;
        if let Self::Object(values) = self {
            for (name, value) in values.as_slice() {
                budget.work(name.len_units().min(key.len()).saturating_add(1))?;
                if name.equals_ascii(key) { return Ok(Some(value)); }
            }
        }
        Ok(None)
    }
    pub fn take(&mut self, key: &[u8], budget: &Budget) -> Result<Option<Self>, Error> {
        budget.work(1)?;
        let Self::Object(values) = self else { return Ok(None) };
        let mut found = None;
        for (index, (name, _)) in values.as_slice().iter().enumerate() {
            budget.work(name.len_units().min(key.len()).saturating_add(1))?;
            if name.equals_ascii(key) { found = Some(index); break; }
        }
        let Some(index) = found else { return Ok(None) };
        budget.work(values.data.len() - index - 1)?;
        Ok(Some(values.data.remove(index).1))
    }
    pub fn object(&self) -> Option<&[(Text, Self)]> { match self { Self::Object(values) => Some(values.as_slice()), _ => None } }
    pub fn array(&self) -> Option<&[Self]> { match self { Self::Array(values) => Some(values.as_slice()), _ => None } }
    pub fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Text(value) => value.len_units() != 0,
            Self::Array(_) | Self::Object(_) => true,
        }
    }
    pub fn is_object(&self) -> bool { matches!(self, Self::Array(_) | Self::Object(_)) }
    pub fn to_text(&self, budget: &Budget) -> Result<Text, Error> {
        budget.work(1)?;
        match self {
            Self::Undefined => Text::from_str("undefined", budget),
            Self::Null => Text::from_str("null", budget),
            Self::Bool(value) => Text::from_str(if *value { "true" } else { "false" }, budget),
            Self::Number(value) => {
                if value.is_nan() { return Text::from_str("NaN", budget); }
                if *value == f64::INFINITY { return Text::from_str("Infinity", budget); }
                if *value == f64::NEG_INFINITY { return Text::from_str("-Infinity", budget); }
                if *value == 0.0 { return Text::from_str("0", budget); }
                Text::from_str(ryu_js::Buffer::new().format(*value), budget)
            }
            Self::Text(value) => value.copy(budget),
            Self::Array(values) => {
                let mut output = Text::from_str("", budget)?;
                let comma = Text::from_str(",", budget)?;
                for (index, value) in values.as_slice().iter().enumerate() {
                    budget.work(1)?;
                    if index != 0 { output.append(&comma)?; }
                    if !matches!(value, Self::Undefined | Self::Null) { output.append(&value.to_text(budget)?)?; }
                }
                Ok(output)
            }
            Self::Object(_) => {
                if self.get(b"toString", budget)?.is_some() { return Err(Error::Type { operation: "object-to-primitive" }); }
                Text::from_str("[object Object]", budget)
            }
        }
    }
}
