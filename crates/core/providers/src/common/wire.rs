//! `JSON.stringify` for request bodies, as UTF-8 bytes, through `ir::json::stringify`.

use super::js::J;
use risunest_ir::{json, Budget, Error, OwnedVec, Value};

pub(crate) fn to_value(value: &J, budget: &Budget) -> Result<Value, Error> {
    Ok(match value {
        J::Undefined => Value::Undefined,
        J::Null => Value::Null,
        J::Bool(flag) => Value::Bool(*flag),
        J::Num(number) => Value::Number(*number),
        J::Str(text) => Value::Text(text.copy(budget)?),
        J::Arr(items) => {
            let mut array = OwnedVec::new(budget)?;
            for item in items { array.push(to_value(item, budget)?)?; }
            Value::Array(array)
        }
        J::Obj(entries) => {
            let mut object = OwnedVec::new(budget)?;
            for (key, item) in entries { object.push((key.copy(budget)?, to_value(item, budget)?))?; }
            Value::Object(object)
        }
    })
}

/// `JSON.stringify(value)` as bytes; `None` where JavaScript returns `undefined`.
pub(crate) fn stringify(value: &J, budget: &Budget) -> Result<Option<Vec<u8>>, Error> {
    let Some(text) = json::stringify(&to_value(value, budget)?, budget)? else { return Ok(None) };
    // The output escapes lone surrogates, so it is always valid Unicode.
    Ok(Some(match text.as_unicode() {
        Some(plain) => plain.as_bytes().to_vec(),
        None => String::from_utf16_lossy(&text.units().collect::<Vec<u16>>()).into_bytes(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::Text;
    use crate::common::js::{new_object, set_str, str_value, tests::budget};

    #[test]
    fn serializes_like_json_stringify() {
        let budget = budget();
        let mut object = new_object();
        set_str(&mut object, "a", str_value("x\"\\\n\u{1}\u{2028}é", &budget).unwrap(), &budget).unwrap();
        set_str(&mut object, "skip", J::Undefined, &budget).unwrap();
        set_str(&mut object, "n", J::Arr(vec![J::Num(1.5), J::Num(-0.0), J::Num(f64::NAN), J::Num(1e21), J::Num(0.000001), J::Undefined, J::Bool(true)]), &budget).unwrap();
        let lone = Text::from_units(&[0x61, 0xd800, 0x62, 0xdc00, 0xd83d, 0xde00], &budget).unwrap();
        set_str(&mut object, "s", J::Str(lone), &budget).unwrap();
        let bytes = stringify(&object, &budget).unwrap().unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), "{\"a\":\"x\\\"\\\\\\n\\u0001\u{2028}é\",\"n\":[1.5,0,null,1e+21,0.000001,null,true],\"s\":\"a\\ud800b\\udc00\u{1f600}\"}");
        assert!(stringify(&J::Undefined, &budget).unwrap().is_none());
    }
}
