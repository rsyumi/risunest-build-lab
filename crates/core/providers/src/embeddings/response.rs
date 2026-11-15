use super::{Failure, Output};
use risunest_ir::{json, Budget, Error, OwnedVec, Text, Utf8Decoder, Value};

fn malformed(category: &'static str, budget: &Budget) -> Result<Failure, Failure> {
    Ok(Failure::Decode { diagnostic: Text::from_str(category, budget)? })
}
pub(super) fn decode(status: u16, body: Vec<u8>, budget: &Budget) -> Result<Output, Failure> {
    let _received = budget.reserve_owned(body.capacity())?;
    let text = Utf8Decoder::new().decode(&body, true, budget)?;
    let parsed = match json::parse_text(&text, budget) {
        Ok(value) => value,
        Err(Error::Syntax { .. }) => return Err(malformed("embedding-response-json", budget)?),
        Err(error) => return Err(error.into()),
    };
    if !(200..300).contains(&status) {
        let diagnostic = json::stringify(&parsed, budget)?.ok_or(Failure::Invalid("embedding-error-json"))?;
        budget.check()?;
        return Err(Failure::Remote { status, diagnostic });
    }
    vectors(&parsed, budget)
}

fn vectors(parsed: &Value, budget: &Budget) -> Result<Output, Failure> {
    let Some(Value::Array(rows)) = parsed.get(b"data", budget)? else { return Err(malformed("embedding-response-data", budget)?); };
    let mut output = OwnedVec::new(budget)?;
    for row in rows.as_slice() {
        budget.work(1)?;
        if !matches!(row, Value::Object(_)) { return Err(malformed("embedding-response-row", budget)?); }
        let vector = match row.get(b"embedding", budget)? {
            None | Some(Value::Null) => None,
            Some(Value::Array(values)) => {
                let mut vector = OwnedVec::new(budget)?;
                for value in values.as_slice() {
                    budget.work(1)?;
                    let Value::Number(number) = value else { return Err(malformed("embedding-response-number", budget)?); };
                    vector.push(*number)?;
                }
                Some(vector)
            }
            _ => return Err(malformed("embedding-response-vector", budget)?),
        };
        output.push(vector)?;
    }
    budget.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::Limits;
    use std::mem::size_of;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn parsed_response_and_output_share_the_owned_limit() {
        const ROWS: usize = 64;
        for generous in [false, true] {
            let b = Budget::new(Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
                max_values: 10_000, max_depth: 16, max_work: 1_000_000 }, Arc::new(AtomicBool::new(false))).unwrap();
            let body = format!("{{\"data\":[{}]}}", vec![r#"{"embedding":[1.00000001]}"#; ROWS].join(","));
            let parsed = json::parse(&body, &b).unwrap();
            let parsed_owned = b.owned_bytes();
            let headroom = if generous { 3 * ROWS * size_of::<Option<OwnedVec<f64>>>() + ROWS * size_of::<f64>() }
                else { size_of::<Option<OwnedVec<f64>>>() };
            let caller = b.reserve_owned(b.limits().max_owned_bytes - b.owned_bytes() - headroom).unwrap();
            let before = b.owned_bytes();
            let result = vectors(&parsed, &b);
            if generous {
                let output = result.as_ref().unwrap();
                assert_eq!(output.as_slice().len(), ROWS);
                assert!(output.as_slice().iter().all(|row| row.as_ref().unwrap().as_slice() == [1.00000001]));
                assert!(b.owned_bytes() > before);
            } else { assert!(matches!(result, Err(Failure::Limit("owned-bytes")))); }
            drop(result);
            assert_eq!(b.owned_bytes(), before);
            drop(caller);
            assert_eq!(b.owned_bytes(), parsed_owned);
            drop(parsed);
            assert_eq!(b.owned_bytes(), Budget::context_bytes());
        }
    }
}
