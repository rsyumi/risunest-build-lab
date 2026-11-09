use super::{Budget, Conversion, Scalar, ScalarConversion, ValueLimits};
use mlua::{Function, Lua, MultiValue, Value};
use std::{cell::{Cell, RefCell}, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryReader { One, Recent, Full }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumberArgument { Value(f64), Invalid }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreparedHistory { One(NumberArgument), Recent(NumberArgument), Full }

#[derive(Clone)]
enum Js { Null, Boolean(bool), Number(f64), Text(Vec<u16>), Reference(usize), Function }

enum Prototype { Builtin, Null, Value(Js) }
enum Node { Array(Vec<Js>), Object { own: Vec<(Vec<u16>, Js)>, prototype: Prototype } }

pub(super) struct Input<'a> {
    lua: &'a Lua,
    control: Conversion<'a>,
    limits: ValueLimits,
    nodes: Cell<usize>,
    bytes: Cell<usize>,
    key_string: Option<Function>,
    graph: Vec<Node>,
}

fn error(message: &'static str) -> mlua::Error { mlua::Error::runtime(message) }
fn key_is(key: &[u16], text: &str) -> bool { key.iter().copied().eq(text.encode_utf16()) }

impl<'a> Input<'a> {
    pub(super) fn new(lua: &'a Lua, budget: &'a Rc<RefCell<Option<Budget>>>, limits: ValueLimits, key_string: Option<Function>) -> Self {
        Self { lua, control: Conversion::new(lua, budget, limits), limits, nodes: Cell::new(0),
            bytes: Cell::new(0), key_string, graph: Vec::new() }
    }

    pub(super) fn check(&self) -> mlua::Result<()> { self.control.check() }

    fn charge(&self, bytes: usize) -> mlua::Result<()> {
        self.check()?;
        self.bytes.set(self.bytes.get().checked_add(bytes).filter(|bytes| *bytes <= self.limits.utf8_bytes)
            .ok_or_else(|| error("values input byte limit exceeded"))?);
        Ok(())
    }

    fn node(&self, depth: usize) -> mlua::Result<()> {
        self.check()?;
        if depth > self.limits.depth { return Err(error("values input depth limit exceeded")); }
        self.nodes.set(self.nodes.get().checked_add(1).filter(|nodes| *nodes <= self.limits.nodes)
            .ok_or_else(|| error("values input node limit exceeded"))?);
        self.charge(std::mem::size_of::<Js>())
    }

    fn text(&self, bytes: &[u8]) -> mlua::Result<Vec<u16>> {
        // Monotonic admission includes vector capacity and overlapping UTF-8/UTF-16 copies.
        self.charge(bytes.len().checked_mul(8).and_then(|n| n.checked_add(64)).ok_or_else(|| error("values input byte limit exceeded"))?)?;
        let result = wasmoon_text(bytes)?;
        self.check()?;
        Ok(result)
    }

    pub(super) fn scalar(&self, value: Value) -> mlua::Result<Scalar> {
        self.node(0)?;
        Ok(match value {
            Value::Nil => Scalar::Nil,
            Value::Boolean(value) => Scalar::Boolean(value),
            Value::Integer(value) => Scalar::Number(value as f64),
            Value::Number(value) => Scalar::Number(value),
            Value::String(value) => Scalar::String(String::from_utf16(&self.text(&value.as_bytes())?)
                .map_err(|_| error("values compatibility: unpaired UTF-16 store string"))?),
            _ => return Err(error("unsupported scalar value")),
        })
    }

    pub(super) fn encode_scalar(&self, value: &Scalar, budget: &'a Rc<RefCell<Option<Budget>>>) -> mlua::Result<Value> {
        self.check()?;
        match value {
            Scalar::String(text) => {
                self.node(0)?;
                self.charge(text.len())?;
                let prefix = text.split('\0').next().unwrap_or("");
                Ok(Value::String(self.lua.create_string(prefix.as_bytes())?))
            }
            _ => ScalarConversion::new(self.lua, budget, self.limits).encode(value),
        }
    }

    fn stringify_key(&self, key: Value) -> mlua::Result<Vec<u16>> {
        self.node(0)?;
        let value: mlua::LuaString = self.key_string.as_ref().ok_or_else(|| error("missing key converter"))?.call(key)?;
        self.text(&value.as_bytes())
    }

    fn convert(&mut self, value: Value, depth: usize, seen: &mut Vec<(usize, usize)>) -> mlua::Result<Js> {
        self.node(depth)?;
        Ok(match value {
            Value::Nil => Js::Null,
            Value::Boolean(value) => Js::Boolean(value),
            Value::Integer(value) => Js::Number(value as f64),
            Value::Number(value) => Js::Number(value),
            Value::String(value) => Js::Text(self.text(&value.as_bytes())?),
            Value::Function(_) => Js::Function,
            Value::Table(table) => {
                if let Some(meta) = table.metatable() {
                    match meta.raw_get::<Value>("__name")? {
                        Value::Nil => (),
                        Value::String(name) => { self.text(&name.as_bytes())?; }
                        _ => (),
                    }
                }
                let identity = table.to_pointer() as usize;
                if let Some((_, id)) = seen.iter().find(|(pointer, _)| *pointer == identity) { return Ok(Js::Reference(*id)); }
                let mut sequential = true;
                let mut count = 0_usize;
                // Wasmoon classifies before registering the table in its seen map.
                for pair in table.clone().pairs::<Value, Value>() {
                    let (key, _) = pair?;
                    count = count.checked_add(1).ok_or_else(|| error("values input node limit exceeded"))?;
                    let key = self.stringify_key(key)?;
                    self.charge(64)?;
                    sequential &= key_is(&key, &count.to_string());
                }
                self.charge(4 * (std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>()))?;
                let id = self.graph.len();
                self.graph.push(if sequential && count > 0 { Node::Array(Vec::new()) }
                    else { Node::Object { own: Vec::new(), prototype: Prototype::Builtin } });
                seen.push((identity, id));
                // A second live traversal is observable through key __tostring.
                for pair in table.pairs::<Value, Value>() {
                    let (key, value) = pair?;
                    let key = self.stringify_key(key)?;
                    let value = self.convert(value, depth + 1, seen)?;
                    self.assign(id, key, value)?;
                }
                Js::Reference(id)
            }
            // This profile creates no JS proxy, promise, pointer or thread capabilities.
            _ => return Err(error("unsupported values-profile host capability")),
        })
    }

    fn proto_setter(&self, id: usize, key: &[u16]) -> mlua::Result<bool> {
        let Node::Object { own, prototype } = &self.graph[id] else { return Ok(false); };
        if own.iter().any(|(name, _)| name == key) { return Ok(false); }
        let mut prototype = prototype;
        for _ in 0..=self.graph.len() {
            self.check()?;
            match prototype {
                Prototype::Builtin => return Ok(key_is(key, "__proto__")),
                Prototype::Null => return Ok(false),
                Prototype::Value(Js::Function) => {
                    if ["name", "length", "caller", "arguments"].iter().any(|name| key_is(key, name)) {
                        return Err(error("cannot assign inherited function property"));
                    }
                    return Ok(key_is(key, "__proto__"));
                }
                Prototype::Value(Js::Reference(next)) => match &self.graph[*next] {
                    Node::Object { own, prototype: next } => {
                        if own.iter().any(|(name, _)| name == key) { return Ok(false); }
                        prototype = next;
                    }
                    Node::Array(_) => return Ok(key_is(key, "__proto__")),
                },
                Prototype::Value(_) => return Ok(false),
            }
        }
        Err(error("cyclic object prototype"))
    }

    fn assign(&mut self, id: usize, key: Vec<u16>, value: Js) -> mlua::Result<()> {
        if matches!(&self.graph[id], Node::Array(_)) {
            self.charge(4 * std::mem::size_of::<Js>())?;
            if let Node::Array(items) = &mut self.graph[id] { items.push(value); }
            return Ok(());
        }
        if self.proto_setter(id, &key)? {
            let next = match &value {
                Js::Null => Prototype::Null,
                Js::Reference(_) | Js::Function => Prototype::Value(value.clone()),
                _ => return Ok(()),
            };
            let mut cursor = &next;
            for _ in 0..=self.graph.len() {
                self.check()?;
                match cursor {
                    Prototype::Value(Js::Reference(next)) => {
                        if *next == id { return Err(error("cyclic object prototype")); }
                        if let Node::Object { prototype, .. } = &self.graph[*next] { cursor = prototype; } else { break; }
                    }
                    _ => break,
                }
            }
            if let Node::Object { prototype, .. } = &mut self.graph[id] { *prototype = next; }
            return Ok(());
        }
        self.charge(4 * std::mem::size_of::<(Vec<u16>, Js)>())?;
        if let Node::Object { own, .. } = &mut self.graph[id] {
            if let Some((_, previous)) = own.iter_mut().find(|(name, _)| name == &key) { *previous = value; }
            else { own.push((key, value)); }
        }
        Ok(())
    }

    fn cloneable(&self, value: &Js, seen: &mut Vec<bool>, depth: usize) -> mlua::Result<()> {
        self.node(depth)?;
        match value {
            Js::Function => Err(error("values input could not be cloned")),
            Js::Reference(id) if !seen[*id] => {
                seen[*id] = true;
                match &self.graph[*id] {
                    Node::Array(items) => for value in items { self.cloneable(value, seen, depth + 1)?; },
                    Node::Object { own, .. } => for (_, value) in own { self.cloneable(value, seen, depth + 1)?; },
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn truthy(value: &Js) -> bool {
        match value { Js::Null => false, Js::Boolean(value) => *value,
            Js::Number(value) => *value != 0.0 && !value.is_nan(), Js::Text(value) => !value.is_empty(), _ => true }
    }

    fn append_text(&self, output: &mut Vec<u16>, text: &[u16]) -> mlua::Result<()> {
        self.charge(text.len().checked_mul(4).and_then(|n| n.checked_add(16)).ok_or_else(|| error("values input byte limit exceeded"))?)?;
        output.extend_from_slice(text);
        Ok(())
    }

    fn primitive_text(&self, value: &Js, output: &mut Vec<u16>, arrays: &mut Vec<usize>, depth: usize) -> mlua::Result<bool> {
        self.node(depth)?;
        match value {
            Js::Null => self.append_text(output, &[110,117,108,108])?,
            Js::Boolean(value) => self.append_text(output, if *value { &[116,114,117,101] } else { &[102,97,108,115,101] })?,
            Js::Number(value) => {
                self.charge(2048)?;
                let text = if value.is_nan() { "NaN".into() } else if *value == f64::INFINITY { "Infinity".into() }
                    else if *value == f64::NEG_INFINITY { "-Infinity".into() } else if *value == 0.0 { "0".into() } else { value.to_string() };
                self.append_text(output, &text.encode_utf16().collect::<Vec<_>>())?;
            }
            Js::Text(value) => self.append_text(output, value)?,
            Js::Reference(id) => match &self.graph[*id] {
                Node::Object { own, .. } => {
                    if own.iter().any(|(key, _)| key_is(key, "toString")) { return Ok(false); }
                    self.append_text(output, &[91,111,98,106,101,99,116,32,79,98,106,101,99,116,93])?;
                }
                Node::Array(items) => {
                    if arrays.contains(id) { return Ok(true); }
                    self.charge(4 * std::mem::size_of::<usize>())?;
                    arrays.push(*id);
                    for (index, value) in items.iter().enumerate() {
                        if index > 0 { self.append_text(output, &[44])?; }
                        if !matches!(value, Js::Null) && !self.primitive_text(value, output, arrays, depth + 1)? { arrays.pop(); return Ok(false); }
                    }
                    arrays.pop();
                }
            },
            Js::Function => return Ok(false),
        }
        Ok(true)
    }

    fn number(&self, value: Option<&Js>) -> mlua::Result<NumberArgument> {
        let number = match value {
            None => f64::NAN,
            Some(Js::Null) => 0.0,
            Some(Js::Boolean(value)) => if *value { 1.0 } else { 0.0 },
            Some(Js::Number(value)) => *value,
            Some(Js::Text(value)) => { self.charge(value.len())?; js_number(value) },
            Some(value) => {
                let mut text = Vec::new();
                if !self.primitive_text(value, &mut text, &mut Vec::new(), 0)? { return Ok(NumberArgument::Invalid); }
                self.charge(text.len())?;
                js_number(&text)
            }
        };
        self.check()?;
        Ok(NumberArgument::Value(number))
    }

    pub(super) fn history(&mut self, arguments: MultiValue, reader: HistoryReader) -> mlua::Result<PreparedHistory> {
        let mut converted = Vec::new();
        for value in arguments {
            // getStackValues starts a new Wasmoon seen map for every argument.
            self.charge(4 * std::mem::size_of::<Js>())?;
            converted.push(self.convert(value, 0, &mut Vec::new())?);
        }
        self.charge(self.graph.len())?;
        let mut seen = vec![false; self.graph.len()];
        for value in &converted { self.cloneable(value, &mut seen, 0)?; }
        let result = match reader {
            HistoryReader::Full => PreparedHistory::Full,
            HistoryReader::One => PreparedHistory::One(self.number(converted.get(1))?),
            HistoryReader::Recent => {
                let number = if converted.get(1).is_none_or(|value| !Self::truthy(value)) { NumberArgument::Value(0.0) }
                    else { self.number(converted.get(1))? };
                PreparedHistory::Recent(match number {
                    NumberArgument::Value(value) if !value.is_nan() => NumberArgument::Value(value.floor().max(0.0)),
                    value => value,
                })
            }
        };
        self.check()?;
        Ok(result)
    }
}

/// A value `Input::scalar` converts as Wasmoon does: a primitive, or a string that reads into JS as Unicode text.
pub(super) fn scalar_like(value: &Value) -> bool {
    match value {
        Value::Nil | Value::Boolean(_) | Value::Integer(_) | Value::Number(_) => true,
        Value::String(text) => wasmoon_text(&text.as_bytes()).is_ok_and(|units| char::decode_utf16(units).all(|unit| unit.is_ok())),
        _ => false,
    }
}

// Only bytes owned by Lua, including its one trailing NUL, may be consulted.
pub(super) fn wasmoon_text(bytes: &[u8]) -> mlua::Result<Vec<u16>> {
    let end = bytes.iter().position(|value| *value == 0).unwrap_or(bytes.len());
    if end > 16 {
        let text = String::from_utf8_lossy(&bytes[..end]);
        return Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).encode_utf16().collect());
    }
    let mut output = Vec::new();
    let mut position = 0;
    while position < end {
        let first = bytes[position];
        let width = if first & 128 == 0 { 1 } else if first & 224 == 192 { 2 } else if first & 240 == 224 { 3 } else { 4 };
        if position + width - 1 > bytes.len() { return Err(mlua::Error::runtime(format!("values compatibility: unknown UTF-8 tail (lead {position}, required {}, owned through {})", position + width - 1, bytes.len()))); }
        let byte = |offset: usize| if offset == bytes.len() { 0 } else { bytes[offset] };
        let value = match width {
            1 => first as u32,
            2 => ((first & 31) as u32) << 6 | (byte(position + 1) & 63) as u32,
            3 => ((first & 15) as u32) << 12 | ((byte(position + 1) & 63) as u32) << 6 | (byte(position + 2) & 63) as u32,
            _ => ((first & 7) as u32) << 18 | ((byte(position + 1) & 63) as u32) << 12
                | ((byte(position + 2) & 63) as u32) << 6 | (byte(position + 3) & 63) as u32,
        };
        if value < 65536 { output.push(value as u16); }
        else { let value = value - 65536; output.push((55296 | value >> 10) as u16); output.push((56320 | value & 1023) as u16); }
        position += width;
    }
    Ok(output)
}

fn whitespace(value: u16) -> bool {
    matches!(value, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn js_number(value: &[u16]) -> f64 {
    let start = value.iter().position(|unit| !whitespace(*unit)).unwrap_or(value.len());
    let end = value.iter().rposition(|unit| !whitespace(*unit)).map_or(start, |index| index + 1);
    let value = &value[start..end];
    if value.is_empty() { return 0.0; }
    if value.iter().any(|unit| *unit > 127) { return f64::NAN; }
    let bytes = value.iter().map(|unit| *unit as u8).collect::<Vec<_>>();
    if bytes == b"Infinity" || bytes == b"+Infinity" { return f64::INFINITY; }
    if bytes == b"-Infinity" { return f64::NEG_INFINITY; }
    if bytes.len() >= 2 && bytes[0] == b'0' {
        let bits = match bytes[1] { b'x' | b'X' => 4, b'b' | b'B' => 1, b'o' | b'O' => 3, _ => 0 };
        if bits != 0 { return radix_number(&bytes[2..], bits); }
    }
    let mut index = usize::from(matches!(bytes[0], b'+' | b'-'));
    let mut digits = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() { digits += 1; index += 1; }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() { digits += 1; index += 1; }
    }
    if digits == 0 { return f64::NAN; }
    if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
        index += 1;
        if index < bytes.len() && matches!(bytes[index], b'+' | b'-') { index += 1; }
        let before = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() { index += 1; }
        if before == index { return f64::NAN; }
    }
    if index != bytes.len() { return f64::NAN; }
    std::str::from_utf8(&bytes).unwrap().parse::<f64>().unwrap_or(f64::NAN)
}

fn radix_number(bytes: &[u8], width: usize) -> f64 {
    if bytes.is_empty() { return f64::NAN; }
    let mut significand = 0_u64;
    let mut length = 0_usize;
    let mut guard = false;
    let mut sticky = false;
    for byte in bytes {
        let digit = match byte { b'0'..=b'9' => byte - b'0', b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10, _ => return f64::NAN };
        if digit as usize >= 1 << width { return f64::NAN; }
        for shift in (0..width).rev() {
            let bit = digit >> shift & 1;
            if length == 0 && bit == 0 { continue; }
            if length < 53 { significand = significand * 2 + bit as u64; }
            else if length == 53 { guard = bit != 0; }
            else { sticky |= bit != 0; }
            length += 1;
        }
    }
    if length == 0 { return 0.0; }
    if length <= 53 { return significand as f64; }
    if guard && (sticky || significand & 1 != 0) { significand += 1; }
    if length > 1024 { return f64::INFINITY; }
    significand as f64 * 2_f64.powi((length - 53) as i32)
}
