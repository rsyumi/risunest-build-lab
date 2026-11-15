//! Charged SameValueZero keys for memo sets, chat memo lookup, main ID sets and score aggregation.
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Charge, OwnedVec, Text, Value};
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

/// A primitive under SameValueZero. Objects and arrays have no key: every stored one is a distinct identity.
pub(crate) enum Key { Undefined, Null, Bool(bool), Number(u64), Text(OwnedVec<u16>) }

impl Key {
    pub(crate) fn of(value: &Value, budget: &Budget) -> Result<Option<Self>, StageError> {
        budget.work(1)?;
        Ok(Some(match value {
            Value::Undefined => Self::Undefined,
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(*value),
            Value::Number(value) => Self::Number(number_bits(*value)),
            Value::Text(value) => Self::text(value, budget)?,
            Value::Array(_) | Value::Object(_) => return Ok(None),
        }))
    }
    /// A prompt message memo: absent is `undefined`.
    pub(crate) fn memo(memo: Option<&Text>, budget: &Budget) -> Result<Self, StageError> {
        match memo { Some(text) => Self::text(text, budget), None => { budget.work(1)?; Ok(Self::Undefined) } }
    }
    pub(crate) fn text(text: &Text, budget: &Budget) -> Result<Self, StageError> {
        // Copying, hashing and comparing the key each scan its UTF-16 units.
        budget.work(text.len_units().saturating_mul(3).saturating_add(1))?;
        let mut units = OwnedVec::new(budget)?;
        for (index, unit) in text.units().enumerate() {
            if index & 4095 == 0 { budget.check()?; }
            units.push(unit)?;
        }
        Ok(Self::Text(units))
    }
}

fn number_bits(value: f64) -> u64 {
    if value.is_nan() { f64::NAN.to_bits() } else if value == 0.0 { 0f64.to_bits() } else { value.to_bits() }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Undefined, Self::Undefined) | (Self::Null, Self::Null) => true,
            (Self::Bool(left), Self::Bool(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => left == right,
            (Self::Text(left), Self::Text(right)) => left.as_slice() == right.as_slice(),
            _ => false,
        }
    }
}
impl Eq for Key {}
impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Undefined => 0u8.hash(state),
            Self::Null => 1u8.hash(state),
            Self::Bool(value) => { 2u8.hash(state); value.hash(state); }
            Self::Number(value) => { 3u8.hash(state); value.hash(state); }
            Self::Text(value) => { 4u8.hash(state); value.as_slice().hash(state); }
        }
    }
}

/// First position of each key. Table growth is charged before it happens and the charge follows the table.
pub(crate) struct KeyIndex { map: HashMap<Key, usize>, charge: Option<Charge> }

impl KeyIndex {
    pub(crate) fn new() -> Self { Self { map: HashMap::new(), charge: None } }

    /// Records `key` at `position` unless it is already present, and returns the earlier position if so.
    pub(crate) fn first(&mut self, key: Key, position: usize, budget: &Budget) -> Result<Option<usize>, StageError> {
        budget.work(1)?;
        if let Some(&existing) = self.map.get(&key) { return Ok(Some(existing)); }
        self.reserve(budget)?;
        self.map.insert(key, position);
        Ok(None)
    }

    pub(crate) fn get(&self, key: &Key, budget: &Budget) -> Result<Option<usize>, StageError> {
        budget.work(1)?;
        Ok(self.map.get(key).copied())
    }

    fn reserve(&mut self, budget: &Budget) -> Result<(), StageError> {
        if self.map.len() < self.map.capacity() { return Ok(()); }
        let limit = || StageError::Limit(Cow::Borrowed("owned-bytes"));
        // HashMap capacity excludes spare buckets and control bytes. Twice capacity plus one bucket bounds those
        // allocations, including the small-table layout; the old charge remains until growth finishes.
        let bytes_for = |capacity: usize| capacity.checked_add(1).and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_mul(size_of::<(Key, usize)>() + 1)).and_then(|n| n.checked_add(16)).ok_or_else(limit);
        let capacity = self.map.capacity().checked_mul(2).and_then(|n| n.checked_add(1)).ok_or_else(limit)?.max(3);
        let bytes = bytes_for(capacity)?;
        let mut charge = budget.reserve_owned(bytes)?;
        self.map.try_reserve(1).map_err(|_| StageError::Limit(Cow::Borrowed("allocation")))?;
        let actual = bytes_for(self.map.capacity())?;
        if actual > bytes { charge = budget.reserve_owned(actual)?; }
        self.charge = Some(charge);
        Ok(())
    }
}

/// `new Set(values)` in insertion order: later SameValueZero duplicates are dropped, objects are always kept.
pub(crate) fn unique(values: &mut [Value], budget: &Budget) -> Result<OwnedVec<Value>, StageError> {
    let mut seen = KeyIndex::new();
    let mut result = OwnedVec::new(budget)?;
    for value in values {
        let keep = match Key::of(value, budget)? {
            Some(key) => seen.first(key, result.as_slice().len(), budget)?.is_none(),
            None => true,
        };
        if keep { result.push(std::mem::replace(value, Value::Undefined))?; }
    }
    Ok(result)
}
