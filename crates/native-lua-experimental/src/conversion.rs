use super::{Budget, Terminal, ValueLimits};
use mlua::{Lua, LuaSerdeExt, TerminalReason, Value};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value as Json};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::Rc;

pub(super) struct Conversion<'a> {
    lua: &'a Lua,
    budget: &'a Rc<RefCell<Option<Budget>>>,
    limits: ValueLimits,
    nodes: Cell<usize>,
    bytes: Cell<usize>,
}

impl<'a> Conversion<'a> {
    pub fn new(lua: &'a Lua, budget: &'a Rc<RefCell<Option<Budget>>>, limits: ValueLimits) -> Self {
        Self { lua, budget, limits, nodes: Cell::new(0), bytes: Cell::new(0) }
    }

    pub fn check(&self) -> mlua::Result<()> {
        if self.lua.terminal_reason().is_some() {
            return Err(mlua::Error::runtime("Lua execution terminated"));
        }
        let reason = self.budget.borrow_mut().as_mut()
            .ok_or_else(|| mlua::Error::runtime("no active conversion budget"))?.check();
        if let Some(reason) = reason {
            self.lua.retire_terminal(match reason {
                Terminal::Cancelled => TerminalReason::Cancelled,
                Terminal::Deadline => TerminalReason::Deadline,
            });
            return Err(mlua::Error::runtime("Lua execution terminated"));
        }
        Ok(())
    }

    fn node<E: de::Error>(&self) -> Result<(), E> {
        self.check().map_err(E::custom)?;
        let next = self.nodes.get().checked_add(1)
            .filter(|n| *n <= self.limits.nodes).ok_or_else(|| E::custom("JSON value limit exceeded"))?;
        self.nodes.set(next);
        Ok(())
    }

    fn bytes<E: de::Error>(&self, length: usize) -> Result<(), E> {
        self.check().map_err(E::custom)?;
        let next = self.bytes.get().checked_add(length)
            .filter(|n| *n <= self.limits.utf8_bytes).ok_or_else(|| E::custom("JSON UTF-8 byte limit exceeded"))?;
        self.bytes.set(next);
        Ok(())
    }

    fn container<E: de::Error>(&self, parent_depth: usize) -> Result<usize, E> {
        let depth = parent_depth + 1;
        if depth > self.limits.depth { return Err(E::custom("JSON depth limit exceeded")); }
        Ok(depth)
    }

    pub fn decode(&self, value: Value) -> mlua::Result<Json> {
        let result = JsonSeed { conversion: self, depth: 0 }
            .deserialize(mlua::serde::Deserializer::new(value));
        self.check()?;
        result
    }

    pub fn service(&self, service: mlua::LuaString) -> mlua::Result<String> {
        self.node::<mlua::Error>()?;
        self.bytes::<mlua::Error>(service.as_bytes().len())?;
        Ok(service.to_str()?.to_owned())
    }

    pub fn rejection(&self, error: &str) -> mlua::Result<()> {
        self.node::<mlua::Error>()?;
        self.bytes::<mlua::Error>(error.len())
    }

    pub fn encode(&self, value: &Json) -> mlua::Result<Value> {
        self.admit(value)?;
        let result = self.lua.to_value(value);
        self.check()?;
        result
    }

    // Only concrete, immutably borrowed JSON enters this pass. It has no custom
    // Serialize callbacks; the subsequent mlua traversal cannot expand aliases.
    fn admit(&self, value: &Json) -> mlua::Result<()> {
        enum Frame<'a> {
            Value(&'a Json, usize),
            Array(std::slice::Iter<'a, Json>, usize),
            Object(serde_json::map::Iter<'a>, usize),
        }
        let mut stack = vec![Frame::Value(value, 0)];
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Value(value, depth) => {
                    self.node::<mlua::Error>()?;
                    match value {
                        Json::String(s) => self.bytes::<mlua::Error>(s.len())?,
                        Json::Array(values) => {
                            let depth = self.container::<mlua::Error>(depth)?;
                            stack.push(Frame::Array(values.iter(), depth));
                        }
                        Json::Object(values) => {
                            let depth = self.container::<mlua::Error>(depth)?;
                            stack.push(Frame::Object(values.iter(), depth));
                        }
                        _ => {}
                    }
                }
                Frame::Array(mut values, depth) => {
                    if let Some(value) = values.next() {
                        stack.push(Frame::Array(values, depth));
                        stack.push(Frame::Value(value, depth));
                    }
                }
                Frame::Object(mut values, depth) => {
                    if let Some((key, value)) = values.next() {
                        self.node::<mlua::Error>()?;
                        self.bytes::<mlua::Error>(key.len())?;
                        stack.push(Frame::Object(values, depth));
                        stack.push(Frame::Value(value, depth));
                    }
                }
            }
        }
        self.check()
    }
}

struct JsonSeed<'a, 'b> { conversion: &'a Conversion<'b>, depth: usize }

impl<'de> DeserializeSeed<'de> for JsonSeed<'_, '_> {
    type Value = Json;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        self.conversion.node::<D::Error>()?;
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonSeed<'_, '_> {
    type Value = Json;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result { formatter.write_str("any valid JSON value") }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Json, E> { Ok(Json::Bool(value)) }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Json, E> { Ok(Json::Number(value.into())) }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Json, E> { Ok(Json::Number(value.into())) }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Json, E> {
        Ok(Number::from_f64(value).map_or(Json::Null, Json::Number))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Json, E> {
        self.conversion.bytes::<E>(value.len())?;
        Ok(Json::String(value.to_owned()))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Json, E> {
        self.conversion.bytes::<E>(value.len())?;
        Ok(Json::String(value))
    }
    fn visit_none<E: de::Error>(self) -> Result<Json, E> { Ok(Json::Null) }
    fn visit_unit<E: de::Error>(self) -> Result<Json, E> { Ok(Json::Null) }
    fn visit_some<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Json, D::Error> {
        self.conversion.check().map_err(de::Error::custom)?;
        deserializer.deserialize_any(self)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Json, A::Error> {
        let depth = self.conversion.container::<A::Error>(self.depth)?;
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(JsonSeed { conversion: self.conversion, depth })? {
            values.push(value);
        }
        Ok(Json::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let depth = self.conversion.container::<A::Error>(self.depth)?;
        let mut values = Map::new();
        while let Some(key) = map.next_key_seed(KeySeed(self.conversion))? {
            let value = map.next_value_seed(JsonSeed { conversion: self.conversion, depth })?;
            values.insert(key, value);
        }
        Ok(Json::Object(values))
    }
}

struct KeySeed<'a, 'b>(&'a Conversion<'b>);

impl<'de> DeserializeSeed<'de> for KeySeed<'_, '_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        self.0.node::<D::Error>()?;
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for KeySeed<'_, '_> {
    type Value = String;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result { formatter.write_str("a string key") }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        self.0.bytes::<E>(value.len())?;
        Ok(value.to_owned())
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
        self.0.bytes::<E>(value.len())?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cancellation;
    use std::time::{Duration, Instant};

    #[test]
    fn rust_conversion_checks_cancel_and_deadline_without_entering_lua() {
        for cancelled in [false, true] {
            let lua = Lua::new();
            let cancellation = Cancellation::default();
            if cancelled { cancellation.cancel(); }
            let budget = Rc::new(RefCell::new(Some(Budget {
                deadline: if cancelled { Instant::now() + Duration::from_secs(1) } else { Instant::now() },
                cancellation,
                terminal: None,
            })));
            let conversion = Conversion::new(&lua, &budget, ValueLimits::default());
            assert!(conversion.decode(Value::Boolean(true)).is_err());
            assert_eq!(lua.terminal_reason(), Some(if cancelled { TerminalReason::Cancelled } else { TerminalReason::Deadline }));
            assert!(conversion.encode(&Json::Bool(true)).is_err());
        }
    }
}
