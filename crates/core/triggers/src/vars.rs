//! Trigger variables: the script access's stored, pending and default chat variables
//! (`src/ts/process/scriptConversationAccess.ts:93-113`, `:418-434`) and each invocation's local scopes
//! (`src/ts/process/triggers.ts:1262-1338`).

use crate::js::{copy_value, literal, object_prototype_key, strict, unsupported, units};
use risunest_ir::budget::{Budget, Error};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use std::collections::{BTreeMap, HashMap};

/// A value as `getVar` returns it: a stored variable reads as its string, while a write of this run keeps the value
/// `setVar` was given, which can be any JSON value or `undefined`. `id` tells arrays and objects apart for `===`; an
/// array or object with id 0 is identical to nothing.
#[derive(Debug)]
pub(crate) struct Var { pub value: Value, pub id: u64 }

impl Var {
    pub fn text(text: Text) -> Self { Self { value: Value::Text(text), id: 0 } }

    pub fn copy(&self, budget: &Budget) -> Result<Self, Error> { Ok(Self { value: copy_value(&self.value, budget)?, id: self.id }) }
}

/// `left === right`.
pub(crate) fn identical(left: &Var, right: &Var) -> bool {
    if left.value.is_object() || right.value.is_object() { return left.id != 0 && left.id == right.id; }
    strict(&left.value, &right.value)
}

#[derive(Debug, Default)]
pub(crate) struct Variables {
    /// `scriptstate['$' + key]` for every stored key.
    stored: HashMap<Vec<u16>, Value>,
    /// The writes buffered since the run began, by key.
    pending: HashMap<Vec<u16>, Var>,
    /// The character's `defaultVariables`, then the template's, first entry per key.
    defaults: HashMap<Vec<u16>, Text>,
    /// The last id given to an array or object.
    last_id: u64,
}

/// `parseKeyValue` (`src/ts/util.ts:939-958`).
fn key_values(template: &Value, budget: &Budget) -> Result<Vec<(Vec<u16>, Vec<u16>)>, StageError> {
    // A falsy template is empty, and any other non-string throws inside the `try`.
    let Value::Text(template) = template else { return Ok(Vec::new()) };
    let template = units(template);
    budget.work(template.len())?;
    let mut entries = Vec::new();
    for line in template.split(|unit| *unit == u16::from(b'\n')) {
        let mut parts = line.split(|unit| *unit == u16::from(b'='));
        let (key, value) = (parts.next().unwrap_or(&[]), parts.next().unwrap_or(&[]));
        if !key.is_empty() && !value.is_empty() { entries.push((key.to_vec(), value.to_vec())); }
    }
    Ok(entries)
}

impl Variables {
    pub fn new(scriptstate: &Value, character_defaults: &Value, template_defaults: &Value, budget: &Budget) -> Result<Self, StageError> {
        let mut variables = Self::default();
        match scriptstate {
            Value::Undefined | Value::Null => {}
            Value::Object(entries) => {
                for (key, value) in entries.as_slice() {
                    budget.work(1)?;
                    let key = units(key);
                    if key.first() == Some(&u16::from(b'$')) {
                        variables.stored.insert(key[1..].to_vec(), copy_value(value, budget)?);
                    }
                }
            }
            _ => return Err(unsupported("trigger-conversation-shape")),
        }
        for (key, value) in key_values(character_defaults, budget)?.into_iter().chain(key_values(template_defaults, budget)?) {
            if !variables.defaults.contains_key(&key) { variables.defaults.insert(key, crate::js::text(&value, budget)?); }
        }
        Ok(variables)
    }

    /// A value a `JSON.parse` or a property read just produced: an array or object gets an identity of its own.
    pub fn fresh(&mut self, value: Value) -> Var {
        if !value.is_object() { return Var { value, id: 0 }; }
        self.last_id += 1;
        Var { value, id: self.last_id }
    }

    /// `access.getVariable(key)`: a pending value other than `undefined` as is, else the stored value as a string,
    /// else the default, else `'null'`.
    pub fn get(&self, key: &[u16], budget: &Budget) -> Result<Var, StageError> {
        budget.work(key.len().saturating_add(1))?;
        if let Some(pending) = self.pending.get(key).filter(|pending| !matches!(pending.value, Value::Undefined)) {
            return Ok(pending.copy(budget)?);
        }
        Ok(Var::text(match self.stored.get(key) {
            None | Some(Value::Undefined | Value::Null) => match self.defaults.get(key) {
                Some(value) => value.copy(budget)?,
                None => literal("null", budget)?,
            },
            Some(value) => value.to_text(budget)?,
        }))
    }

    /// `access.setVariable(key, value)`: buffers the write and returns whether it differs, by strict equality, from
    /// the pending value, or the stored one when none is pending.
    pub fn set(&mut self, key: &[u16], value: Var, budget: &Budget) -> Result<bool, StageError> {
        budget.work(key.len().saturating_add(1))?;
        let same = match self.pending.get(key) {
            Some(current) => identical(current, &value),
            // A stored array or object is never the value being written.
            None => match self.stored.get(key) {
                None => matches!(value.value, Value::Undefined),
                Some(stored) => strict(stored, &value.value),
            },
        };
        if same { return Ok(false); }
        self.pending.insert(key.to_vec(), value);
        Ok(true)
    }
}

/// One invocation's `localVarScopes[last]`: values by indent, then key.
#[derive(Debug, Default)]
pub(crate) struct Locals {
    scopes: BTreeMap<u32, HashMap<Vec<u16>, Var>>,
}

impl Locals {
    // `scope[key] !== undefined` also finds an inherited `Object.prototype` member on every scope object.
    fn inherited(&self, key: &[u16], indent: u32) -> Result<(), StageError> {
        if object_prototype_key(key) && self.scopes.range(..=indent).next().is_some() { return Err(unsupported("trigger-local-key")); }
        Ok(())
    }

    /// `getLocalVar(key)` from `indent` down to 0.
    pub fn get(&self, key: &[u16], indent: u32, budget: &Budget) -> Result<Option<&Var>, StageError> {
        self.inherited(key, indent)?;
        for (_, scope) in self.scopes.range(..=indent).rev() {
            budget.work(1)?;
            if let Some(value) = scope.get(key) { return Ok(Some(value)); }
        }
        Ok(None)
    }

    /// `setLocalVar(key, value, indent)`: the nearest scope at or below `indent` that holds `key`, else `indent`.
    pub fn set(&mut self, key: &[u16], value: Var, indent: u32, budget: &Budget) -> Result<bool, StageError> {
        if object_prototype_key(key) { return Err(unsupported("trigger-local-key")); }
        let value = match value.value { Value::Undefined | Value::Null => Var::text(literal("null", budget)?), _ => value };
        let mut target = indent;
        for (at, scope) in self.scopes.range(..=indent).rev() {
            budget.work(1)?;
            if scope.contains_key(key) { target = *at; break; }
        }
        let scope = self.scopes.entry(target).or_default();
        if scope.get(key).is_some_and(|current| identical(current, &value)) { return Ok(false); }
        scope.insert(key.to_vec(), value);
        Ok(true)
    }

    /// `clearLocalVarsAtIndent(indent)`: every scope at `indent` or deeper.
    pub fn clear_from(&mut self, indent: u32) {
        let _ = self.scopes.split_off(&indent);
    }
}
