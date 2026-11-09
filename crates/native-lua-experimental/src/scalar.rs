use super::{Budget, Conversion, ValueLimits};
use mlua::{Lua, Value};
use std::{cell::{Cell, RefCell}, rc::Rc};

#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    Nil,
    Boolean(bool),
    Number(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScalarReturn {
    NoValues,
    One(Scalar),
    /// Declines the invocation with this label; the VM retires as `Terminal::Cancelled`.
    Unsupported(String),
}

// Wasmoon passes integral JS numbers as BigInt to a Wasm i64 parameter.
// Extract the integer's low 64 bits without a saturating float-to-int cast.
fn js_integer(value: f64) -> i64 {
    if value == 0.0 { return 0; }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let significand = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let low = if exponent < 52 { significand >> (52 - exponent) }
        else if exponent < 116 { significand << (exponent - 52) }
        else { 0 };
    if value.is_sign_negative() { (low as i64).wrapping_neg() } else { low as i64 }
}

pub(super) struct ScalarConversion<'a> {
    lua: &'a Lua,
    control: Conversion<'a>,
    limits: ValueLimits,
    nodes: Cell<usize>,
    bytes: Cell<usize>,
}

impl<'a> ScalarConversion<'a> {
    pub fn new(lua: &'a Lua, budget: &'a Rc<RefCell<Option<Budget>>>, limits: ValueLimits) -> Self {
        Self { lua, control: Conversion::new(lua, budget, limits), limits, nodes: Cell::new(0), bytes: Cell::new(0) }
    }

    pub fn check(&self) -> mlua::Result<()> { self.control.check() }

    fn node(&self) -> mlua::Result<()> {
        self.check()?;
        self.nodes.set(self.nodes.get().checked_add(1).filter(|count| *count <= self.limits.nodes)
            .ok_or_else(|| mlua::Error::runtime("scalar value limit exceeded"))?);
        Ok(())
    }

    fn text(&self, bytes: &[u8]) -> mlua::Result<()> {
        self.check()?;
        self.bytes.set(self.bytes.get().checked_add(bytes.len()).filter(|count| *count <= self.limits.utf8_bytes)
            .ok_or_else(|| mlua::Error::runtime("scalar UTF-8 byte limit exceeded"))?);
        if bytes.contains(&0) { return Err(mlua::Error::runtime("scalar strings cannot contain NUL")); }
        std::str::from_utf8(bytes).map_err(|_| mlua::Error::runtime("scalar string is not UTF-8"))?;
        self.check()
    }

    pub fn rejection(&self, text: &str) -> mlua::Result<()> {
        self.node()?;
        self.text(text.as_bytes())
    }

    pub fn decode(&self, value: Value) -> mlua::Result<Scalar> {
        self.node()?;
        let value = match value {
            Value::Nil => Scalar::Nil,
            Value::Boolean(value) => Scalar::Boolean(value),
            Value::Integer(value) => Scalar::Number(value as f64),
            Value::Number(value) => Scalar::Number(value),
            Value::String(value) => {
                self.text(&value.as_bytes())?;
                Scalar::String(value.to_str()?.to_owned())
            }
            _ => return Err(mlua::Error::runtime("unsupported scalar value")),
        };
        self.check()?;
        Ok(value)
    }

    pub fn encode(&self, value: &Scalar) -> mlua::Result<Value> {
        self.node()?;
        let value = match value {
            Scalar::Nil => Value::Nil,
            Scalar::Boolean(value) => Value::Boolean(*value),
            Scalar::Number(value) if value.is_finite() && value.fract() == 0.0 => {
                Value::Integer(js_integer(*value))
            }
            Scalar::Number(value) => Value::Number(*value),
            Scalar::String(value) => {
                self.text(value.as_bytes())?;
                Value::String(self.lua.create_string(value.as_bytes())?)
            }
        };
        self.check()?;
        Ok(value)
    }
}
