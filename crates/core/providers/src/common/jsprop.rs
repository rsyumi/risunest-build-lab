//! Reading JSON-shaped data the way JavaScript property access does: reads that throw on `null` and `undefined`,
//! `String(value)` for response values, and the exceptions a response parser turns into failures.

use super::js::{self, J};
use super::wire;
use super::BuildError;
use risunest_ir::{Budget, Error, Text, Value};

/// A JavaScript exception from a property read or call.
#[derive(Debug)]
pub(crate) struct Thrown;

pub(crate) enum Failed { Thrown, Decline(&'static str), Budget(Error) }
impl From<Thrown> for Failed { fn from(_: Thrown) -> Self { Self::Thrown } }
impl From<Error> for Failed { fn from(value: Error) -> Self { Self::Budget(value) } }
impl From<BuildError> for Failed {
    fn from(value: BuildError) -> Self {
        match value { BuildError::Unsupported(name) => Self::Decline(name), BuildError::Budget(error) => Self::Budget(error), _ => Self::Thrown }
    }
}
impl From<Failed> for BuildError {
    fn from(value: Failed) -> Self {
        match value { Failed::Decline(name) => Self::Unsupported(name), Failed::Budget(error) => Self::Budget(error), Failed::Thrown => Self::Unsupported("thrown") }
    }
}

/// `None` is `undefined`.
pub(crate) type Prop<'a> = Option<&'a J>;

pub(crate) fn nullish(value: Prop<'_>) -> bool { matches!(value, None | Some(J::Null) | Some(J::Undefined)) }

/// `value.key`.
pub(crate) fn read<'a>(value: Prop<'a>, key: &str) -> Result<Prop<'a>, Thrown> {
    match value {
        None | Some(J::Null) | Some(J::Undefined) => Err(Thrown),
        Some(J::Obj(entries)) => Ok(entries.iter().find(|(name, _)| name.equals_ascii(key.as_bytes())).map(|(_, value)| value)),
        Some(J::Arr(items)) => Ok(key.parse::<usize>().ok().and_then(|index| items.get(index))),
        Some(_) => Ok(None),
    }
}

/// `value?.key`.
pub(crate) fn read_optional<'a>(value: Prop<'a>, key: &str) -> Result<Prop<'a>, Thrown> {
    if nullish(value) { Ok(None) } else { read(value, key) }
}

pub(crate) fn truthy(value: Prop<'_>) -> bool { value.is_some_and(js::truthy) }

/// `String(value)` for the JSON-shaped values a response can hold.
pub(crate) fn to_text(value: &J, budget: &Budget) -> Result<Text, Failed> {
    Ok(match value {
        J::Undefined => js::text("undefined", budget)?,
        J::Null => js::text("null", budget)?,
        J::Bool(flag) => js::text(if *flag { "true" } else { "false" }, budget)?,
        J::Num(number) => Value::Number(*number).to_text(budget)?,
        J::Str(text) => text.copy(budget)?,
        J::Arr(items) => {
            let mut output = js::text("", budget)?;
            let comma = js::text(",", budget)?;
            for (index, item) in items.iter().enumerate() {
                if index != 0 { output.append(&comma)?; }
                if !matches!(item, J::Undefined | J::Null) { output.append(&to_text(item, budget)?)?; }
            }
            output
        }
        J::Obj(entries) => {
            if entries.iter().any(|(name, _)| name.equals_ascii(b"toString")) { return Err(Failed::Thrown); }
            js::text("[object Object]", budget)?
        }
    })
}

pub(crate) fn stringify(value: &J, budget: &Budget) -> Result<Text, Error> {
    let bytes = wire::stringify(value, budget)?.unwrap_or_else(|| b"undefined".to_vec());
    js::text(std::str::from_utf8(&bytes).map_err(|_| Error::Invariant("json-utf8"))?, budget)
}

