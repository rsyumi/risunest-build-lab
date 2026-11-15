use std::sync::{Arc, Mutex};

use risunest_ir::{budget::{Budget, Charge}, stage::Effect, Text, Value};
use risunest_native_lua_experimental::{ValueLimits, VariableValue};
use serde_json::Value as Json;

use super::lua_edit::{property_key, storage_key};
use super::lua_view::Assigned;
use crate::persistent_store::script_access::ScriptVariable;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BindingPolicy { AccessScope, DirectOperation }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ScopeKind { #[default] None, Edits, Operation }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VariableFinish { Variables, Finish, WithoutMessages, Failure }

pub(crate) trait VariableReader {
    fn character_available(&mut self) -> Result<bool, String>;
    fn defaults_available(&mut self) -> Result<bool, String>;
    fn stored(&mut self, name: &Text) -> Result<Value, String>;
    fn default(&mut self, key: Option<&[u16]>) -> Result<Value, String>;
}

pub(crate) fn string_key(key: &str, budget: &Budget) -> Result<VariableValue, String> {
    let held = Arc::new(budget.reserve_owned(key.len().saturating_mul(4).saturating_add(128)).map_err(ir)?);
    Ok(VariableValue::scalar(risunest_native_lua_experimental::Scalar::String(key.to_owned()),
        risunest_native_lua_experimental::Limits::default().values).retain(held))
}

pub(crate) fn assigned_input(value: &Assigned, limits: ValueLimits, budget: &Budget) -> Result<VariableValue, String> {
    match value {
        Assigned::Structured { input, .. } => {
            let held = Arc::new(budget.reserve_owned(input.owned_bytes().saturating_add(128)).map_err(ir)?);
            Ok(input.clone().retain(held))
        },
        value => super::lua_view::variable_input(&value.value(budget).map_err(ir)?, limits, budget),
    }
}

pub(crate) fn assigned_from_ir(value: &Value, limits: ValueLimits, budget: &Budget) -> Result<Assigned, String> {
    if matches!(value, Value::Undefined) { return Ok(Assigned::Undefined); }
    Assigned::converted(Some(super::lua_view::variable_input(value, limits, budget)?), budget)
}

#[derive(Clone, Debug)]
pub(crate) struct RawOperation {
    pub key: VariableValue,
    pub value: Assigned,
    pub local: bool,
    acknowledged_effect: Arc<Mutex<Option<Effect>>>,
    _held: Arc<Charge>,
}

impl PartialEq for RawOperation {
    fn eq(&self, other: &Self) -> bool { self.key == other.key && self.value == other.value }
}

impl RawOperation {
    pub(crate) fn take_effect(&self) -> Option<Effect> {
        self.acknowledged_effect.lock().unwrap_or_else(|error| error.into_inner()).take()
    }
}

// Both payloads are prepared before publication. Settlement selects and moves them without a new conversion.
#[derive(Debug)]
pub(crate) struct RawEntry {
    pub operation: RawOperation,
    pub name: Text,
    stored_name: Text,
    pub storage_key: Option<String>,
    pub projected_name: Option<String>,
    normalized_key: String,
    pub durable: ScriptVariable,
    durable_error: Option<&'static str>,
    effect: Effect,
    retained_bytes: usize,
    logical_bytes: usize,
    _durable: Option<Charge>,
}

#[derive(Debug)]
struct Stored {
    name: Text, value: Assigned, durable: ScriptVariable, durable_error: Option<&'static str>,
    normalized_key: String, retained_bytes: usize, _held: Arc<Charge>, _durable: Option<Charge>,
}

#[derive(Debug)]
struct Local { name: Text, value: Assigned, write: Option<RawEntry>, _held: Option<Arc<Charge>> }

#[derive(Debug, Clone, Copy)]
enum Target { Local(usize), Entry(Option<usize>) }

#[derive(Debug)]
pub(crate) struct PreparedVariable { target: Target, entry: RawEntry }

impl PreparedVariable {
    pub(crate) fn operation(&self) -> &RawOperation { &self.entry.operation }
    pub(crate) fn retained_bytes(&self) -> usize { self.entry.retained_bytes }
}

pub(crate) struct RawVariables {
    pub policy: BindingPolicy,
    pub kind: ScopeKind,
    budget: Budget,
    entries: Vec<RawEntry>,
    stored: Vec<Stored>,
    locals: Vec<Local>,
    projection_attempts: usize,
    sealed: bool,
    legacy_effects_only: bool,
}

impl std::fmt::Debug for RawVariables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawVariables").field("policy", &self.policy).field("kind", &self.kind).field("entries", &self.entries.len()).finish()
    }
}

fn ir(error: impl std::fmt::Debug) -> String { format!("ir: {error:?}") }

pub(crate) fn matches_json(value: &Assigned, stored: Option<&Json>) -> bool {
    match (value, stored) {
        (Assigned::Undefined, None) | (Assigned::Null, Some(Json::Null)) => true,
        (Assigned::Bool(a), Some(Json::Bool(b))) => a == b,
        (Assigned::Number(a), Some(Json::Number(b))) => b.as_f64().is_some_and(|b| *a == b),
        (Assigned::Text(a), Some(Json::String(b))) => a == b,
        _ => false,
    }
}

pub(crate) fn matches_ir(value: &Assigned, stored: Option<&Value>) -> bool {
    match (value, stored) {
        (Assigned::Undefined, None | Some(Value::Undefined)) | (Assigned::Null, Some(Value::Null)) => true,
        (Assigned::Bool(a), Some(Value::Bool(b))) => a == b,
        (Assigned::Number(a), Some(Value::Number(b))) => a == b,
        (Assigned::Text(a), Some(Value::Text(b))) => b.units().eq(a.encode_utf16()),
        _ => false,
    }
}

fn ir_owned_bytes(value: &Value) -> usize {
    std::mem::size_of::<Value>().saturating_mul(4).saturating_add(match value {
        Value::Text(text) => text.len_units().saturating_mul(8).saturating_add(128),
        Value::Array(items) => items.as_slice().iter().map(ir_owned_bytes).sum(),
        Value::Object(items) => items.as_slice().iter().map(|(key, value)| key.len_units().saturating_mul(8)
            .saturating_add(256).saturating_add(ir_owned_bytes(value))).sum(),
        _ => 0,
    })
}

fn assigned_bytes(value: &Assigned) -> usize {
    match value {
        Assigned::Text(text) => text.capacity().saturating_add(std::mem::size_of::<Assigned>()),
        Assigned::Structured { input, value, .. } => input.owned_bytes().saturating_add(ir_owned_bytes(value)).saturating_add(256),
        _ => std::mem::size_of::<Assigned>(),
    }
}

fn persistence_value(value: &Value, budget: &Budget)
    -> Result<(ScriptVariable, Option<&'static str>, Option<Charge>), String> {
    fn normalize(value: &Value, collision: &mut bool) -> Option<Json> {
        Some(match value {
            Value::Undefined => return None,
            Value::Null => Json::Null,
            Value::Bool(value) => Json::Bool(*value),
            Value::Number(value) => serde_json::Number::from_f64(*value).map(Json::Number).unwrap_or(Json::Null),
            Value::Text(text) => Json::String(String::from_utf16_lossy(&text.units().collect::<Vec<_>>())),
            Value::Array(items) => Json::Array(items.as_slice().iter().map(|value| normalize(value, collision).unwrap_or(Json::Null)).collect()),
            Value::Object(items) => {
                let mut names = std::collections::BTreeSet::new();
                let mut result = serde_json::Map::new();
                for (name, value) in items.as_slice() {
                    let name = String::from_utf16_lossy(&name.units().collect::<Vec<_>>());
                    if !names.insert(name.clone()) { *collision = true; }
                    if let Some(value) = normalize(value, collision) { result.insert(name, value); }
                }
                Json::Object(result)
            }
        })
    }
    if matches!(value, Value::Undefined) { return Ok((ScriptVariable::Undefined, None, None)); }
    let bytes = ir_owned_bytes(value).saturating_mul(12).saturating_add(256);
    budget.work(bytes).map_err(ir)?;
    let charge = budget.reserve_owned(bytes).map_err(ir)?;
    let mut collision = false;
    let result = normalize(value, &mut collision).ok_or("variable persistence value")?;
    Ok((ScriptVariable::Value(result), collision.then_some("native persistence key normalization collision"), Some(charge)))
}

impl RawVariables {
    pub(crate) fn new(budget: &Budget, policy: BindingPolicy, projection_attempts: usize) -> Self {
        Self { policy, kind: ScopeKind::None, budget: budget.clone(), entries: Vec::new(), stored: Vec::new(),
            locals: Vec::new(), projection_attempts, sealed: false, legacy_effects_only: false }
    }

    pub(crate) fn open_edits(&mut self) { if self.kind == ScopeKind::None { self.kind = ScopeKind::Edits; } }
    pub(crate) fn open_operation(&mut self) { self.kind = ScopeKind::Operation; }
    pub(crate) fn legacy_effects_only(&mut self) { self.legacy_effects_only = true; }
    pub(crate) fn len(&self) -> usize { self.entries.len() }
    pub(crate) fn budget(&self) -> &Budget { &self.budget }
    pub(crate) fn rebind_budget(&mut self, budget: &Budget) { self.budget = budget.clone(); }
    pub(crate) fn variable_bytes(&self) -> usize { self.entries.iter().map(|entry| entry.logical_bytes).sum() }
    pub(crate) fn prepared_count(&self, prepared: &PreparedVariable) -> usize {
        self.entries.len() + usize::from(matches!(prepared.target, Target::Entry(None)))
    }
    pub(crate) fn prepared_bytes(&self, prepared: &PreparedVariable) -> usize {
        match prepared.target {
            Target::Local(_) => self.variable_bytes(),
            Target::Entry(index) => self.variable_bytes().saturating_sub(index.map_or(0, |index| self.entries[index].logical_bytes))
                .saturating_add(prepared.entry.logical_bytes),
        }
    }
    pub(crate) fn owned_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.entries.iter().map(|entry| entry.retained_bytes).sum::<usize>()
            + self.stored.iter().map(|stored| stored.retained_bytes).sum::<usize>()
            + self.locals.iter().filter_map(|local| local.write.as_ref()).map(|entry| entry.retained_bytes).sum::<usize>()
            + self.locals.iter().map(|local| assigned_bytes(&local.value).saturating_add(local.name.len_units().saturating_mul(8))
                .saturating_add(4 * std::mem::size_of::<Local>())).sum::<usize>()
    }

    fn key_index(&self, key: &VariableValue) -> Result<Option<usize>, String> {
        for (index, entry) in self.entries.iter().enumerate() {
            self.budget.work(key.key_comparison_work(&entry.operation.key)).map_err(ir)?;
            if key.same_value_zero(&entry.operation.key) { return Ok(Some(index)); }
        }
        Ok(None)
    }

    fn names_equal(&self, a: &Text, b: &Text) -> Result<bool, String> {
        self.budget.work(a.len_units().min(b.len_units()).saturating_add(1)).map_err(ir)?;
        Ok(a == b)
    }

    fn name_index(&self, name: &Text) -> Result<Option<usize>, String> {
        for (index, entry) in self.entries.iter().enumerate() {
            if self.names_equal(name, &entry.name)? { return Ok(Some(index)); }
        }
        Ok(None)
    }

    fn local_index(&self, key: &VariableValue) -> Result<Option<usize>, String> {
        if self.locals.is_empty() { return Ok(None); }
        let name = property_key(Some(key), &self.budget)?;
        for (index, local) in self.locals.iter().enumerate() {
            if self.names_equal(&name, &local.name)? { return Ok(Some(index)); }
        }
        Ok(None)
    }

    pub(crate) fn local(&self, key: &VariableValue) -> Result<Option<&Assigned>, String> {
        Ok(self.local_index(key)?.map(|index| &self.locals[index].value))
    }

    pub(crate) fn pending(&self, key: &VariableValue) -> Result<Option<&Assigned>, String> {
        if self.policy == BindingPolicy::DirectOperation { return Ok(None); }
        Ok(self.key_index(key)?.map(|index| &self.entries[index].operation.value))
    }

    pub(crate) fn stored(&self, name: &Text) -> Result<Option<&Assigned>, String> {
        if self.policy == BindingPolicy::DirectOperation {
            if let Some(index) = self.name_index(name)? { return Ok(Some(&self.entries[index].operation.value)); }
        }
        for stored in &self.stored {
            if self.names_equal(name, &stored.name)? { return Ok(Some(&stored.value)); }
        }
        if let Some(name) = name.as_unicode() {
            for stored in &self.stored {
                let projected = stored.normalized_key.strip_prefix('$').unwrap_or(&stored.normalized_key);
                self.budget.work(name.len().min(projected.len()).saturating_add(1)).map_err(ir)?;
                if name == projected { return Ok(Some(&Assigned::Undefined)); }
            }
        }
        Ok(None)
    }

    fn strict_equal(&self, a: &Assigned, b: &Assigned) -> Result<bool, String> {
        let work = match (a, b) {
            (Assigned::Text(a), Assigned::Text(b)) => a.len().min(b.len()).saturating_add(1),
            (Assigned::Structured { input: a, .. }, Assigned::Structured { input: b, .. }) => a.key_comparison_work(b),
            _ => 1,
        };
        self.budget.work(work).map_err(ir)?;
        Ok(a == b)
    }

    pub(crate) fn input(&self, key: &VariableValue, limits: ValueLimits, reader: &mut dyn VariableReader) -> Result<VariableValue, String> {
        if let Some(value) = self.local(key)? { return assigned_input(value, limits, &self.budget); }
        if let Some(value) = self.pending(key)?.filter(|value| !matches!(value, Assigned::Undefined)) {
            return assigned_input(value, limits, &self.budget);
        }
        let available = if self.policy == BindingPolicy::AccessScope { reader.defaults_available()? }
            else { reader.character_available()? };
        if !available {
            return super::lua_view::variable_input(&Value::Text(Text::from_str("null", &self.budget).map_err(ir)?), limits, &self.budget);
        }
        let name = storage_key(key, &self.budget)?;
        let value = if let Some(value) = self.stored(&name)? {
            match value {
                Assigned::Null | Assigned::Undefined => return super::lua_view::variable_input(&reader.default(key.text_units())?, limits, &self.budget),
                value => assigned_input(value, limits, &self.budget)?,
            }
        } else {
            let value = reader.stored(&name)?;
            if matches!(value, Value::Null | Value::Undefined) {
                return super::lua_view::variable_input(&reader.default(key.text_units())?, limits, &self.budget);
            }
            super::lua_view::variable_input(&value, limits, &self.budget)?
        };
        let text = property_key(Some(&value), &self.budget)?;
        super::lua_view::variable_input(&Value::Text(text), limits, &self.budget)
    }

    pub(crate) fn prepare(&self, key: VariableValue, mut value: Assigned, force: bool,
        mut read: impl FnMut(&Text) -> Result<Assigned, String>) -> Result<Option<PreparedVariable>, String> {
        if self.sealed { return Err("raw variable scope is sealed".into()); }
        let (target, name) = if let Some(index) = self.local_index(&key)? {
            if matches!(value, Assigned::Undefined | Assigned::Null) { value = Assigned::Text("null".into()); }
            if !force && self.strict_equal(&self.locals[index].value, &value)? { return Ok(None); }
            (Target::Local(index), self.locals[index].name.copy(&self.budget).map_err(ir)?)
        } else {
            if self.legacy_effects_only && key.text_units().is_none() { return Err("lua-raw-scope-required".into()); }
            let index = if self.policy == BindingPolicy::AccessScope { self.key_index(&key)? } else { None };
            let name = if let Some(index) = index { self.entries[index].name.copy(&self.budget).map_err(ir)? }
                else { storage_key(&key, &self.budget)? };
            let index = if self.policy == BindingPolicy::DirectOperation { self.name_index(&name)? } else { index };
            let unchanged = if let Some(index) = index { self.strict_equal(&self.entries[index].operation.value, &value)? }
                else if let Some(stored) = self.stored(&name)? { self.strict_equal(stored, &value)? }
                else { self.strict_equal(&read(&name)?, &value)? };
            if !force && unchanged { return Ok(None); }
            (Target::Entry(index), name)
        };
        let projection_work = name.len_units().saturating_add(match &value {
            Assigned::Text(text) => text.len(), Assigned::Structured { input, .. } => input.owned_bytes(), _ => 1,
        })
            .saturating_add(1).saturating_mul(self.projection_attempts.saturating_mul(2).saturating_add(4))
            .saturating_mul(self.stored.len().saturating_add(self.entries.len()).saturating_add(2));
        self.budget.work(projection_work).map_err(ir)?;
        if matches!(target, Target::Entry(_)) {
            self.budget.work(self.stored.len()).map_err(ir)?;
            let retained_work = self.stored.iter().fold(0usize, |work, stored| work.saturating_add(stored.retained_bytes));
            self.budget.work(retained_work.saturating_mul(self.projection_attempts.saturating_mul(2).saturating_add(2))).map_err(ir)?;
        }
        let input_bytes = key.owned_bytes().saturating_mul(2).saturating_add(match &value {
            Assigned::Text(text) => text.len().saturating_mul(4),
            Assigned::Structured { input, .. } => input.text_units().map_or(0, |units| units.len().saturating_mul(8)),
            _ => 0,
        });
        let held = Arc::new(self.budget.reserve_owned(input_bytes.saturating_add(name.len_units().saturating_mul(12))
            .saturating_add(16 * std::mem::size_of::<RawEntry>())).map_err(ir)?);
        let key = key.retain(held.clone());
        let raw = value.value(&self.budget).map_err(ir)?;
        let graph_bytes = assigned_bytes(&value).saturating_add(ir_owned_bytes(&raw).saturating_mul(2));
        let (durable, durable_error, durable_charge) = persistence_value(&raw, &self.budget)?;
        let normalized = String::from_utf16_lossy(&name.units().collect::<Vec<_>>());
        let projected_name = Some(normalized);
        let storage_key = name.as_unicode().map(|name| format!("${name}"));
        let normalized_key = format!("${}", projected_name.as_deref().unwrap_or_default());
        let stored_name = name.copy(&self.budget).map_err(ir)?;
        let effect = Effect::ChatVariable { name: name.copy(&self.budget).map_err(ir)?, value: raw };
        let acknowledged_effect = Effect::ChatVariable { name: name.copy(&self.budget).map_err(ir)?, value: value.value(&self.budget).map_err(ir)? };
        let retained_bytes = input_bytes.saturating_add(graph_bytes).saturating_add(durable.owned_bytes().saturating_mul(4)).saturating_add(projected_name.as_ref().map_or(0, String::capacity))
            .saturating_add(storage_key.as_ref().map_or(0, String::capacity)).saturating_add(16 * std::mem::size_of::<RawEntry>());
        let local = matches!(target, Target::Local(_));
        let logical_bytes = key.owned_bytes().saturating_add(match &value {
            Assigned::Text(value) => value.capacity(), Assigned::Structured { input, .. } => input.owned_bytes(), _ => std::mem::size_of::<Assigned>(),
        }).saturating_add(name.len_units().saturating_mul(2)).saturating_add(64);
        Ok(Some(PreparedVariable { target, entry: RawEntry { operation: RawOperation { key, value, local,
            acknowledged_effect: Arc::new(Mutex::new(Some(acknowledged_effect))), _held: held }, name, stored_name,
            storage_key, projected_name, normalized_key, durable, durable_error, effect, retained_bytes, logical_bytes, _durable: durable_charge } }))
    }

    pub(crate) fn publish(&mut self, prepared: PreparedVariable) {
        match prepared.target {
            Target::Local(index) => {
                let local = &mut self.locals[index];
                local.value = prepared.entry.operation.value.clone();
                local._held = Some(prepared.entry.operation._held.clone());
                local.write = Some(prepared.entry);
            }
            Target::Entry(Some(index)) => self.entries[index] = prepared.entry,
            Target::Entry(None) => self.entries.push(prepared.entry),
        }
    }

    pub(crate) fn seed_local(&mut self, name: Text, value: Assigned) -> Result<(), String> {
        let bytes = match &value { Assigned::Text(text) => text.capacity(), _ => 0 };
        let held = Arc::new(self.budget.reserve_owned(bytes.saturating_add(4 * std::mem::size_of::<Local>())).map_err(ir)?);
        self.locals.push(Local { name, value, write: None, _held: Some(held) });
        Ok(())
    }

    pub(crate) fn filter_original(&self, finish: VariableFinish) -> bool {
        self.policy == BindingPolicy::AccessScope && match finish {
            VariableFinish::Variables | VariableFinish::Failure => true,
            VariableFinish::Finish => self.kind == ScopeKind::None,
            VariableFinish::WithoutMessages => self.kind != ScopeKind::Operation,
        }
    }

    // Key/value comparison work and result slots were admitted by prepare, including the bounded retry/fallback count.
    // Whether each entry strictly equals what the original layer holds under its exact name.
    fn unchanged(&self, mut matches: impl FnMut(&RawEntry) -> bool) -> Vec<bool> {
        self.entries.iter().map(|entry| self.stored.iter().find(|stored| stored.name == entry.name)
            .map_or_else(|| {
                if entry.storage_key.as_ref().is_some_and(|key| self.stored.iter().any(|stored| stored.normalized_key == *key)) {
                    matches!(entry.operation.value, Assigned::Undefined)
                } else { matches(entry) }
            }, |stored| stored.value == entry.operation.value)).collect()
    }

    pub(crate) fn select(&self, finish: VariableFinish, matches: impl FnMut(&RawEntry) -> bool) -> Vec<bool> {
        let filtered = self.filter_original(finish);
        self.unchanged(matches).into_iter().map(|unchanged| !filtered || !unchanged).collect()
    }

    fn unchanged_json(&self, metadata: &Json) -> Vec<bool> {
        let state = metadata.get("scriptstate").and_then(Json::as_object);
        self.unchanged(|entry| matches_json(&entry.operation.value,
            entry.storage_key.as_ref().and_then(|key| state.and_then(|state| state.get(key)))))
    }

    pub(crate) fn project_json(&self, finish: VariableFinish, metadata: &Json,
        mut result: serde_json::Map<String, Json>)
        -> Result<(Vec<bool>, serde_json::Map<String, Json>, Vec<String>), String> {
        let unchanged = self.unchanged_json(metadata);
        let filtered = self.filter_original(finish);
        let selected = unchanged.iter().map(|unchanged| !filtered || !unchanged).collect::<Vec<_>>();
        if self.entries.is_empty() { return Ok((selected, result, Vec::new())); }
        // The existing JSON baseline is already Unicode scalar. Only raw overlays need normalization/collision work.
        // Their payload, comparison work and bounded projection scratch were admitted before each publication.
        let mut rows: Vec<(&Text, &str, &ScriptVariable, Option<&'static str>)> = Vec::with_capacity(self.stored.len() + self.entries.len());
        for stored in &self.stored {
            result.remove(&stored.normalized_key);
            rows.push((&stored.name, &stored.normalized_key, &stored.durable, stored.durable_error));
        }
        let mut changed = Vec::with_capacity(self.entries.len());
        for ((entry, selected), unchanged) in self.entries.iter().zip(&selected).zip(&unchanged) {
            if !selected { continue; }
            if let Some(exact) = &entry.storage_key { result.remove(exact); }
            let row = (&entry.name, entry.normalized_key.as_str(), &entry.durable, entry.durable_error);
            if let Some(index) = rows.iter().position(|(name, _, _, _)| *name == &entry.name) { rows[index] = row; }
            else { rows.push(row); }
            // Applying every entry still reports only the keys whose original value differs, as TS does.
            if !unchanged { changed.push(entry.projected_name.clone().ok_or("raw variable projection name")?); }
        }
        let mut names = std::collections::BTreeSet::new();
        for (_, name, value, error) in rows {
            // Own undefined properties participate before JSON omission.
            if result.contains_key(name) || !names.insert(name) {
                return Err("native persistence key normalization collision".into());
            }
            if let Some(error) = error { return Err(error.into()); }
            if let ScriptVariable::Value(value) = value { result.insert(name.to_owned(), value.clone()); }
        }
        Ok((selected, result, changed))
    }

    #[cfg(test)]
    pub(crate) fn committed_value(&self, metadata: &Json, budget: &Budget) -> Result<Value, String> {
        let mut value = Value::Object(risunest_ir::OwnedVec::new(budget).map_err(ir)?);
        if let Some(state) = metadata.get("scriptstate").and_then(Json::as_object) {
            for (name, item) in state {
                budget.work(name.len().saturating_mul(self.stored.len().saturating_add(1))).map_err(ir)?;
                if self.stored.iter().any(|stored| stored.normalized_key == *name) { continue; }
                value.define(Text::from_str(name, budget).map_err(ir)?, super::lua_view::ir_value(item, budget)?, budget).map_err(ir)?;
            }
        }
        for stored in &self.stored {
            let mut name = risunest_ir::TextBuilder::new(budget).map_err(ir)?;
            name.unit(b'$' as u16).map_err(ir)?;
            for unit in stored.name.units() { name.unit(unit).map_err(ir)?; }
            value.define(name.finish().map_err(ir)?, stored.value.value(budget).map_err(ir)?, budget).map_err(ir)?;
        }
        Ok(value)
    }

    #[cfg(test)]
    pub(crate) fn pending_text_is(&self, key: &str, value: &str) -> bool {
        self.entries.iter().any(|entry| entry.operation.key.text_units().is_some_and(|units|
            units.iter().copied().eq(key.encode_utf16())) && matches!(&entry.operation.value, Assigned::Text(text) if text == value))
    }

    pub(crate) fn entries(&self) -> &[RawEntry] { &self.entries }
    pub(crate) fn pending_string(&self, key: &str) -> Result<Option<&RawEntry>, String> {
        for entry in &self.entries {
            let units = if self.policy == BindingPolicy::AccessScope { entry.operation.key.text_units() }
                else { None };
            if self.policy == BindingPolicy::AccessScope {
                let Some(units) = units else { continue };
                self.budget.work(units.len().min(key.len()).saturating_add(1)).map_err(ir)?;
                if units.iter().copied().eq(key.encode_utf16()) { return Ok(Some(entry)); }
            } else {
                self.budget.work(entry.name.len_units().min(key.len()).saturating_add(1)).map_err(ir)?;
                if entry.name.units().eq(key.encode_utf16()) { return Ok(Some(entry)); }
            }
        }
        Ok(None)
    }
    pub(crate) fn matches_string(&self, key: &str, value: &ScriptVariable) -> Result<Option<bool>, String> {
        let current = if let Some(entry) = self.pending_string(key)? { Some(&entry.operation.value) } else {
            let mut current = None;
            for stored in &self.stored {
                self.budget.work(stored.name.len_units().min(key.len()).saturating_add(1)).map_err(ir)?;
                if stored.name.units().eq(key.encode_utf16()) { current = Some(&stored.value); break; }
            }
            current
        };
        let Some(current) = current else { return Ok(None); };
        let work = match (current, value) {
            (Assigned::Text(a), ScriptVariable::Value(Json::String(b))) => a.len().min(b.len()).saturating_add(1), _ => 1,
        };
        self.budget.work(work).map_err(ir)?;
        Ok(Some(match value { ScriptVariable::Undefined => matches!(current, Assigned::Undefined),
            ScriptVariable::Value(value) => matches_json(current, Some(value)) }))
    }
    pub(crate) fn clear_locals(&mut self) { self.locals.clear(); }
    pub(crate) fn seal(&mut self) { self.sealed = true; }

    pub(crate) fn finish(&mut self, selected: &[bool], apply: bool, finish: VariableFinish) -> Result<Vec<Effect>, String> {
        if selected.len() != self.entries.len() { return Err("raw variable selection changed".into()); }
        let mut effects = Vec::with_capacity(self.entries.len() + self.locals.len());
        for (entry, selected) in self.entries.drain(..).zip(selected.iter().copied()) {
            if apply && selected {
                let stored = Stored { name: entry.stored_name, value: entry.operation.value,
                    durable: entry.durable, durable_error: entry.durable_error,
                    normalized_key: entry.normalized_key,
                    retained_bytes: entry.retained_bytes, _held: entry.operation._held, _durable: entry._durable };
                if let Some(index) = self.stored.iter().position(|current| current.name == stored.name) { self.stored[index] = stored; }
                else { self.stored.push(stored); }
                effects.push(entry.effect);
            }
        }
        for local in &mut self.locals { if let Some(entry) = local.write.take() { effects.push(entry.effect); } }
        if finish != VariableFinish::Variables { self.kind = ScopeKind::None; }
        self.sealed = false;
        Ok(effects)
    }
}
