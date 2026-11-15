use super::{Failure, Input, RemoteTarget, Request};
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::{json, Budget, Charge, OwnedVec, Text, Value};
use std::mem::size_of;

type Leases = OwnedVec<Charge>;
fn limit() -> Failure { Failure::Limit("owned-bytes") }

fn raw_buffer<T>(length: usize, leases: &mut Leases, budget: &Budget) -> Result<Vec<T>, Failure> {
    let bytes = length.checked_mul(size_of::<T>()).ok_or_else(limit)?;
    leases.push(budget.reserve_owned(bytes)?)?;
    let mut output = Vec::new();
    output.try_reserve_exact(length).map_err(|_| Failure::Limit("allocation"))?;
    let actual = output.capacity().checked_mul(size_of::<T>()).ok_or_else(limit)?;
    if actual > bytes { leases.push(budget.reserve_owned(actual - bytes)?)?; }
    Ok(output)
}
fn bytes(value: &[u8], leases: &mut Leases, budget: &Budget) -> Result<Vec<u8>, Failure> {
    let mut output = raw_buffer(value.len(), leases, budget)?;
    for block in value.chunks(4096) { budget.work(block.len())?; output.extend_from_slice(block); }
    Ok(output)
}
fn literal(value: &str, leases: &mut Leases, budget: &Budget) -> Result<String, Failure> {
    String::from_utf8(bytes(value.as_bytes(), leases, budget)?).map_err(|_| Failure::Invalid("embedding-literal"))
}
/// WebIDL USVString conversion, with isolated UTF-16 replaced before a future owner parses the URL.
fn usv(value: &Text, leases: &mut Leases, budget: &Budget) -> Result<String, Failure> {
    let mut length = 0usize;
    for scalar in char::decode_utf16(value.units()) {
        budget.work(1)?;
        length = length.checked_add(scalar.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8()).ok_or_else(limit)?;
    }
    let mut output = String::from_utf8(raw_buffer(length, leases, budget)?).map_err(|_| Failure::Invalid("embedding-usv"))?;
    for scalar in char::decode_utf16(value.units()) {
        budget.work(1)?;
        let mut buffer = [0; 4];
        output.push_str(scalar.unwrap_or(char::REPLACEMENT_CHARACTER).encode_utf8(&mut buffer));
    }
    Ok(output)
}
fn ends_with(value: &Text, suffix: &[u8], budget: &Budget) -> Result<bool, Failure> {
    if value.len_units() < suffix.len() { return Ok(false); }
    let start = value.len_units() - suffix.len();
    let mut matches = true;
    for (index, unit) in value.units().enumerate() {
        budget.work(1)?;
        if index >= start && unit != suffix[index - start] as u16 { matches = false; }
    }
    Ok(matches)
}
fn endpoint(value: &Text, budget: &Budget) -> Result<Text, Failure> {
    if value.len_units() == 0 { return Err(Failure::Invalid("embedding-custom-url")); }
    if ends_with(value, b"/embeddings", budget)? { return Ok(value.copy(budget)?); }
    let mut result = if ends_with(value, b"/", budget)? { value.slice(0, value.len_units() - 1, budget)? } else { value.copy(budget)? };
    result.append(&Text::from_str("/embeddings", budget)?)?;
    Ok(result)
}
fn input_value(input: &Input, budget: &Budget) -> Result<Value, Failure> {
    Ok(match input {
        Input::Scalar(value) => Value::Text(value.copy(budget)?),
        Input::Batch(values) => {
            let mut output = OwnedVec::new(budget)?;
            for value in values.as_slice() { budget.work(1)?; output.push(Value::Text(value.copy(budget)?))?; }
            Value::Array(output)
        }
    })
}
fn bearer(value: Option<&Text>, budget: &Budget) -> Result<Text, Failure> {
    let mut output = Text::from_str("Bearer ", budget)?;
    match value { Some(value) => output.append(value)?, None => output.append(&Text::from_str("undefined", budget)?)? }
    Ok(output)
}

pub(super) fn build(request: Request, budget: &Budget) -> Result<(HttpRequestSpec, Leases), Failure> {
    let mut leases = OwnedVec::new(budget)?;
    let mut body = Value::Object(OwnedVec::new(budget)?);
    body.define(Text::from_str("input", budget)?, input_value(&request.input, budget)?, budget)?;
    let (url_text, authorization) = match &request.target {
        RemoteTarget::Custom { endpoint: custom_url } => {
            if let Some(model) = request.config.custom_model.as_ref().filter(|value| value.len_units() > 0) {
                body.define(Text::from_str("model", budget)?, Value::Text(model.copy(budget)?), budget)?;
            }
            (endpoint(custom_url, budget)?, request.config.custom_key.0.as_ref().filter(|value| value.len_units() > 0).map(|value| bearer(Some(value), budget)).transpose()?)
        }
        target => {
            let model = match target { RemoteTarget::Ada => "text-embedding-ada-002", RemoteTarget::OpenAi3Small => "text-embedding-3-small", RemoteTarget::OpenAi3Large => "text-embedding-3-large", RemoteTarget::Custom { .. } => unreachable!() };
            body.define(Text::from_str("model", budget)?, Value::Text(Text::from_str(model, budget)?), budget)?;
            let key = request.config.openai_override.0.as_ref().filter(|value| value.len_units() > 0).or(request.config.openai_fallback.0.as_ref());
            (Text::from_str("https://api.openai.com/v1/embeddings", budget)?, Some(bearer(key, budget)?))
        }
    };
    let url = usv(&url_text, &mut leases, budget)?;
    let mut headers = raw_buffer(if authorization.is_some() { 2 } else { 1 }, &mut leases, budget)?;
    headers.push((literal("Content-Type", &mut leases, budget)?, literal("application/json", &mut leases, budget)?));
    if let Some(value) = authorization { headers.push((literal("Authorization", &mut leases, budget)?, usv(&value, &mut leases, budget)?)); }
    let serialized = json::stringify(&body, budget)?.ok_or(Failure::Invalid("embedding-body"))?;
    let body = bytes(serialized.as_unicode().ok_or(Failure::Invalid("embedding-body-unicode"))?.as_bytes(), &mut leases, budget)?;
    budget.check()?;
    Ok((HttpRequestSpec { method: HttpMethod::Post, url, headers, body, response: ResponseKind::Buffered }, leases))
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::Limits;
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn usv_conversion_counts_both_scans_and_releases_scratch() {
        for work in [224, 1000] {
            let b = Budget::new(Limits { max_owned_bytes: 4096, max_json_bytes: 4096, max_text_units: 4096,
                max_values: 1000, max_depth: 8, max_work: work }, Arc::new(AtomicBool::new(false))).unwrap();
            let value = Text::from_str(&"a".repeat(64), &b).unwrap();
            let mut leases = OwnedVec::new(&b).unwrap();
            let result = usv(&value, &mut leases, &b);
            if work == 224 { assert!(matches!(result, Err(Failure::Limit("work")))); }
            else { assert_eq!(result.as_ref().unwrap(), &"a".repeat(64)); }
            drop(result); drop(leases); drop(value);
            assert_eq!(b.owned_bytes(), Budget::context_bytes());
        }
    }
}
