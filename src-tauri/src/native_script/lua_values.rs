use super::access::{ChargedText, Error, HistoryRows, OutputReservation, ReadResult};
use risunest_native_lua_experimental::{Limits, ValueLimits};
use risunest_ir::{Budget, Charge, OwnedVec};
use serde_json::Value;

const JSON: &str = include_str!("../../../public/lua/json.lua");
const HELPERS: &str = include_str!("lua_value_helpers.lua");

pub(super) fn bootstrap(source: &str, mut limits: Limits) -> std::result::Result<(String, Limits), String> {
    if source.len() > limits.source_bytes { return Err("source byte limit exceeded".into()); }
    const BEFORE: &str = "require=(function() local reject=error; local module=(function()\n";
    const AFTER: &str = "\nend)(); return function(name) if name~='json' then reject('unsupported module') end return module end end)()\n";
    let fixed = BEFORE.len().checked_add(JSON.len()).and_then(|n| n.checked_add(AFTER.len()))
        .and_then(|n| n.checked_add(HELPERS.len())).and_then(|n| n.checked_add(1)).ok_or("source byte limit exceeded")?;
    limits.source_bytes = limits.source_bytes.checked_add(fixed).ok_or("source byte limit exceeded")?;
    let mut result = String::new();
    result.try_reserve_exact(fixed + source.len()).map_err(|_| "source allocation failed")?;
    for part in [BEFORE, JSON, AFTER, HELPERS, "\n", source] { result.push_str(part); }
    Ok((result, limits))
}

/// The JSON text owner stays charged until the runtime has copied it into Lua.
pub(super) enum HistoryText { Charged(ChargedText), Request(OwnedVec<u8>) }

impl AsRef<str> for HistoryText {
    fn as_ref(&self) -> &str { match self { Self::Charged(text) => text.as_ref(), Self::Request(text) => std::str::from_utf8(text.as_slice()).expect("history writer emits UTF-8") } }
}

/// The rows a history reader selected.
pub(super) enum Rows<'a, 'b> { One(Option<&'a Value>), Many(&'b mut dyn ExactSizeIterator<Item = &'a Value>) }

enum Output { Charged(ChargedText), Request { text: OwnedVec<u8>, budget: Budget, limit: usize } }

enum Scratch { Access { _reservation: OutputReservation }, Request { _charge: Charge } }

struct Writer { text: Output, nodes: usize, max_nodes: usize, max_depth: usize }
type Result<T> = std::result::Result<T, Error>;

impl Writer {
    fn push(&mut self, value: &str) -> Result<()> {
        match &mut self.text {
            Output::Charged(text) => text.push(value),
            Output::Request { text, limit, .. } => {
                if text.as_slice().len().checked_add(value.len()).is_none_or(|length| length > *limit) { return Err(Error::Limit("encoded-history-bytes")); }
                text.extend(value.as_bytes()).map_err(|_| Error::Limit("encoded-history-quota"))
            }
        }
    }

    fn scratch(&self, bytes: usize) -> Result<Scratch> {
        match &self.text {
            Output::Charged(text) => text.scratch(bytes).map(|_reservation| Scratch::Access { _reservation }),
            Output::Request { budget, .. } => budget.reserve_owned(bytes).map(|_charge| Scratch::Request { _charge }).map_err(|_| Error::Limit("encoded-history-quota")),
        }
    }

    fn visit(&mut self, depth: usize) -> Result<()> {
        self.nodes = self.nodes.checked_add(1).filter(|n| *n <= self.max_nodes).ok_or(Error::Limit("json-nodes"))?;
        if depth > self.max_depth { return Err(Error::Limit("json-depth")); }
        Ok(())
    }

    fn children(&self, count: usize, depth: usize) -> Result<()> {
        if self.nodes.checked_add(count).is_none_or(|n| n > self.max_nodes) { return Err(Error::Limit("json-nodes")); }
        if count > 0 && depth > self.max_depth { return Err(Error::Limit("json-depth")); }
        Ok(())
    }

    fn key(&mut self, key: &str, depth: usize) -> Result<()> { self.visit(depth)?; self.string(key) }


    fn string(&mut self, value: &str) -> Result<()> {
        self.push("\"")?;
        let mut start = 0;
        for (index, character) in value.char_indices() {
            let escape = match character { '"' => Some("\\\""), '\\' => Some("\\\\"), '\n' => Some("\\n"),
                '\r' => Some("\\r"), '\t' => Some("\\t"), '\u{8}' => Some("\\b"), '\u{c}' => Some("\\f"), _ => None };
            if escape.is_some() || character < '\u{20}' {
                self.push(&value[start..index])?;
                if let Some(escape) = escape { self.push(escape)?; }
                else {
                    const HEX: &[u8] = b"0123456789abcdef";
                    let byte = character as u8;
                    let escaped = [b'\\', b'u', b'0', b'0', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]];
                    self.push(std::str::from_utf8(&escaped).map_err(|_| Error::Limit("json-escape"))?)?;
                }
                start = index + character.len_utf8();
            }
        }
        self.push(&value[start..])?;
        self.push("\"")
    }

    fn value(&mut self, value: &Value, depth: usize) -> Result<()> {
        self.visit(depth)?;
        match value {
            Value::Null => self.push("null"),
            Value::Bool(value) => self.push(if *value { "true" } else { "false" }),
            Value::Number(value) => {
                let number = value.as_f64().ok_or(Error::Limit("json-number"))?;
                if !number.is_finite() { return self.push("null"); }
                self.push(ryu_js::Buffer::new().format(if number == 0.0 { 0.0 } else { number }))
            }
            Value::String(value) => self.string(value),
            Value::Array(values) => {
                self.children(values.len(), depth + 1)?;
                self.push("[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 { self.push(",")?; }
                    self.value(value, depth + 1)?;
                }
                self.push("]")
            }
            Value::Object(values) => {
                self.children(values.len().checked_mul(2).ok_or(Error::Limit("json-nodes"))?, depth + 1)?;
                // JavaScript enumerates canonical array indices first, then insertion order.
                let bytes = values.len().checked_mul(std::mem::size_of::<(u32, &str, &Value)>()).ok_or(Error::Limit("json-keys"))?;
                let _scratch = self.scratch(bytes)?;
                let mut indexed = Vec::new();
                indexed.try_reserve_exact(values.len()).map_err(|_| Error::Limit("json-keys"))?;
                let _extra_capacity = self.scratch(indexed.capacity().saturating_sub(values.len()).saturating_mul(std::mem::size_of::<(u32, &str, &Value)>()))?;
                for (key, value) in values {
                    if let Some(index) = array_index(key) { indexed.push((index, key.as_str(), value)); }
                }
                indexed.sort_unstable_by_key(|entry| entry.0);
                self.push("{")?;
                let mut comma = false;
                for (_, key, value) in indexed {
                    if comma { self.push(",")?; } comma = true;
                    self.key(key, depth + 1)?; self.push(":")?; self.value(value, depth + 1)?;
                }
                for (key, value) in values {
                    if array_index(key).is_some() { continue; }
                    if comma { self.push(",")?; } comma = true;
                    self.key(key, depth + 1)?; self.push(":")?; self.value(value, depth + 1)?;
                }
                self.push("}")
            }
        }
    }

    fn row(&mut self, row: &Value, full: bool, depth: usize) -> Result<()> {
        if row.is_null() { return Err(Error::HistoryCoercion); }
        self.visit(depth)?;
        let keys = if full { &["role", "data", "chatId"][..] } else { &["role", "data"][..] };
        let fields = 1 + keys.iter().filter(|key| row.get(**key).is_some()).count();
        self.children(fields * 2, depth + 1)?;
        self.push("{")?;
        let mut comma = false;
        for key in keys {
            if let Some(value) = row.get(*key) {
                if comma { self.push(",")?; } comma = true;
                self.key(key, depth + 1)?; self.push(":")?; self.value(value, depth + 1)?;
            }
        }
        if comma { self.push(",")?; }
        self.key("time", depth + 1)?; self.push(":")?;
        match row.get("time") {
            None | Some(Value::Null) => { self.visit(depth + 1)?; self.push("0")?; }
            Some(value) => self.value(value, depth + 1)?,
        }
        self.push("}")
    }

    fn rows(&mut self, rows: Rows<'_, '_>, full: bool) -> Result<()> {
        match rows {
            Rows::One(None) | Rows::One(Some(Value::Null | Value::Bool(false))) => self.value(&Value::Null, 0),
            Rows::One(Some(value)) if value.as_f64() == Some(0.0) || value.as_str() == Some("") => self.value(&Value::Null, 0),
            Rows::One(Some(row)) => self.row(row, false, 0),
            Rows::Many(rows) => {
                self.visit(0)?;
                self.children(rows.len(), 1)?;
                self.push("[")?;
                for (index, row) in rows.into_iter().enumerate() { if index > 0 { self.push(",")?; } self.row(row, full, 1)?; }
                self.push("]")
            }
        }
    }
}

fn array_index(key: &str) -> Option<u32> {
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && (key == "0" || !key.starts_with('0')) && key.bytes().all(|b| b.is_ascii_digit())).then_some(index)
}

/// The JSON text of rows an access scope read, charged to that scope.
pub(super) fn project(rows: &ReadResult<HistoryRows>, full: bool, limits: ValueLimits) -> Result<HistoryText> {
    let mut writer = Writer { text: Output::Charged(rows.text_writer(limits.utf8_bytes)?), nodes: 0, max_nodes: limits.nodes, max_depth: limits.depth };
    match rows.value() {
        HistoryRows::One(row) => writer.rows(Rows::One(row.as_ref()), full)?,
        HistoryRows::Many(rows) => writer.rows(Rows::Many(&mut rows.iter()), full)?,
    }
    let Output::Charged(text) = writer.text else { return Err(Error::Limit("encoded-history-output")) };
    Ok(HistoryText::Charged(text))
}

/// The JSON text of rows the caller owns.
pub(super) fn project_rows(rows: Rows<'_, '_>, full: bool, limits: ValueLimits, budget: &Budget) -> Result<HistoryText> {
    let text = OwnedVec::new(budget).map_err(|_| Error::Limit("encoded-history-quota"))?;
    let mut writer = Writer { text: Output::Request { text, budget: budget.clone(), limit: limits.utf8_bytes }, nodes: 0,
        max_nodes: limits.nodes, max_depth: limits.depth };
    writer.rows(rows, full)?;
    let Output::Request { text, .. } = writer.text else { return Err(Error::Limit("encoded-history-output")) };
    Ok(HistoryText::Request(text))
}
