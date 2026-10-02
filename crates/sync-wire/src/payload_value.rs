//! Semantic comparison encoding for locally authored JavaScript JSON values.
use crate::{Result, WireError};
use serde::{de::{self, MapAccess, SeqAccess, Visitor}, Deserialize, Deserializer};
use serde_json::{Map, Value};
use std::{cmp::Ordering, fmt};

struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("finite JavaScript JSON without duplicate keys") }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Strict, E> { Ok(Strict(Value::Bool(v))) }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Strict, E> { Ok(Strict(Value::Null)) }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Strict, E> { Ok(Strict(Value::String(v.into()))) }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Strict, E> { Ok(Strict(Value::String(v))) }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Strict, E> { self.visit_f64(v as f64) }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Strict, E> { self.visit_f64(v as f64) }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Strict, E> {
                serde_json::Number::from_f64(v).map(|n| Strict(Value::Number(n))).ok_or_else(|| E::custom("non-finite-number"))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Strict, A::Error> {
                let mut values = Vec::new();
                while let Some(Strict(value)) = a.next_element()? { values.push(value); }
                Ok(Strict(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Strict, A::Error> {
                let mut values = Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if values.contains_key(&key) { return Err(de::Error::custom("duplicate-key")); }
                    let Strict(value) = a.next_value()?;
                    values.insert(key, value);
                }
                Ok(Strict(Value::Object(values)))
            }
        }
        d.deserialize_any(JsonVisitor)
    }
}
pub fn canonicalize(bytes: &[u8]) -> Result<Vec<u8>> {
    let Strict(value) = serde_json::from_slice(bytes).map_err(|_| WireError("invalid-payload-json"))?;
    encode(&value)
}
pub fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write(value, &mut out)?;
    Ok(out)
}
fn array_index(key: &str) -> Option<u32> {
    let value = key.parse::<u32>().ok()?;
    (value < u32::MAX && value.to_string() == key).then_some(value)
}
fn key_order(a: &str, b: &str) -> Ordering {
    match (array_index(a), array_index(b)) {
        (Some(a), Some(b)) => a.cmp(&b), (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater, _ => a.encode_utf16().cmp(b.encode_utf16()),
    }
}
fn write(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Object(values) => {
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_by(|a, b| key_order(a, b));
            out.push(b'{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 { out.push(b','); }
                out.extend(serde_json::to_vec(key).map_err(|_| WireError("invalid-payload-json"))?);
                out.push(b':'); write(&values[key], out)?;
            }
            out.push(b'}');
        }
        Value::Array(values) => {
            out.push(b'[');
            for (i, value) in values.iter().enumerate() { if i > 0 { out.push(b','); } write(value, out)?; }
            out.push(b']');
        }
        Value::Number(number) => {
            let number = number.as_f64().filter(|v| v.is_finite()).ok_or(WireError("unsupported-payload-number"))?;
            out.extend(ryu_js::Buffer::new().format(if number == 0.0 { 0.0 } else { number }).as_bytes());
        }
        _ => out.extend(serde_json::to_vec(value).map_err(|_| WireError("invalid-payload-json"))?),
    }
    Ok(())
}
