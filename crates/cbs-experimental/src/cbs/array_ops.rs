use super::{array, json_into, number, quote_into, replace_literal, Json, Units};

// '\\u003A\\u003A', which makeArray writes in place of '::' before JSON.stringify.
const ESCAPED_COLONS: &[u16] = &[92, 117, 48, 48, 51, 65, 92, 117, 48, 48, 51, 65];
fn has_colons(text: &[u16]) -> bool {
    text.windows(2).any(|w| w == [58, 58])
}
fn quote_element(o: &mut Units, text: &[u16]) {
    if has_colons(text) {
        quote_into(o, &replace_literal(text, &[58, 58], ESCAPED_COLONS));
    } else {
        quote_into(o, text);
    }
}
pub(super) fn make_array(values: Vec<Json>) -> Units {
    let mut o = vec![91];
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            o.push(44);
        }
        match value {
            Json::Str(text) => quote_element(&mut o, text),
            value => json_into(&mut o, value),
        }
    }
    o.push(93);
    o
}
// make_array of strings.
pub(super) fn make_text_array<'a>(values: impl IntoIterator<Item = &'a [u16]>) -> Units {
    let mut o = vec![91];
    for (i, text) in values.into_iter().enumerate() {
        if i > 0 {
            o.push(44);
        }
        quote_element(&mut o, text);
    }
    o.push(93);
    o
}

fn integer(value: Option<&Units>) -> Result<f64, String> {
    let value = number(value)?;
    Ok(if value.is_nan() { 0.0 } else { value.trunc() })
}

pub(super) fn evaluate(name: &str, args: &[Units]) -> Result<Units, String> {
    let input = args.first().ok_or("missing-array-input")?;
    let mut values = array(input);
    let inserted = |index: usize| args.get(index).cloned().map(Json::Str).unwrap_or(Json::Null);
    match name {
        "arrayshift" => {
            if !values.is_empty() {
                values.remove(0);
            }
        }
        "arraypop" => {
            values.pop();
        }
        "arraypush" => values.push(inserted(1)),
        "arraysplice" => {
            let start = integer(args.get(1))?;
            let length = values.len() as f64;
            let start = if start < 0.0 {
                (length + start).max(0.0)
            } else {
                start.min(length)
            } as usize;
            let delete = integer(args.get(2))?.max(0.0).min((values.len() - start) as f64) as usize;
            values.splice(start..start + delete, [inserted(3)]);
        }
        _ => return Err("unsupported-array-operator".into()),
    }
    Ok(make_array(values))
}
