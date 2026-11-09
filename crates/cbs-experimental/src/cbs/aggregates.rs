use super::{array, json_depth_exceeds, number_string, scalar, units, Budget, Json, Units};

const MAX_SPREAD_VALUES: usize = 8192;

#[derive(Clone, Copy)]
enum Kind { Min, Max, Sum, Average }

struct Aggregate {
    kind: Kind,
    value: f64,
    count: usize,
}
impl Aggregate {
    fn new(name: &str) -> Result<Self, String> {
        let (kind, value) = match name {
            "min" => (Kind::Min, f64::INFINITY),
            "max" => (Kind::Max, f64::NEG_INFINITY),
            "sum" => (Kind::Sum, 0.0),
            "average" => (Kind::Average, 0.0),
            _ => return Err("unsupported-aggregate-operator".into()),
        };
        Ok(Self { kind, value, count: 0 })
    }

    fn admit(&self, count: usize) -> Result<(), String> {
        if matches!(self.kind, Kind::Min | Kind::Max) && count > MAX_SPREAD_VALUES {
            return Err("aggregate-spread-limit".into());
        }
        Ok(())
    }

    fn add(&mut self, value: f64) {
        let value = if value.is_nan() { 0.0 } else { value };
        self.value = match self.kind {
            Kind::Min if value < self.value
                || (value == 0.0 && self.value == 0.0 && value.is_sign_negative()) => value,
            Kind::Max if value > self.value
                || (value == 0.0 && self.value == 0.0 && value.is_sign_positive()) => value,
            Kind::Min | Kind::Max => self.value,
            Kind::Sum | Kind::Average => self.value + value,
        };
        self.count += 1;
    }

    fn finish(&self) -> f64 {
        if matches!(self.kind, Kind::Average) { self.value / self.count as f64 } else { self.value }
    }
}

fn append(output: &mut Units, value: &[u16], budget: &mut Budget) -> Result<(), String> {
    if value.len() > budget.max_output.saturating_sub(output.len()) {
        return Err("output-limit".into());
    }
    for chunk in value.chunks(256) {
        budget.tick()?;
        output.extend_from_slice(chunk);
    }
    Ok(())
}

fn own_to_string(fields: &[(Units, Json)], budget: &mut Budget) -> Result<bool, String> {
    for (key, _) in fields {
        budget.tick()?;
        if key.as_slice() == [116, 111, 83, 116, 114, 105, 110, 103] {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn append_string(value: &Json, output: &mut Units, budget: &mut Budget) -> Result<bool, String> {
    budget.tick()?;
    match value {
        Json::Str(value) => append(output, value, budget)?,
        Json::Null => append(output, &units("null"), budget)?,
        Json::Bool(value) => append(output, &units(if *value { "true" } else { "false" }), budget)?,
        Json::Num(value) => append(output, &number_string(*value), budget)?,
        Json::Arr(values) => {
            for (index, value) in values.iter().enumerate() {
                budget.tick()?;
                if index != 0 { append(output, &[44], budget)?; }
                if !matches!(value, Json::Null) && !append_string(value, output, budget)? {
                    return Ok(false);
                }
            }
        }
        Json::Obj(fields) => {
            // JSON data cannot supply callable methods; an own toString blocks the inherited method.
            if own_to_string(fields, budget)? { return Ok(false); }
            append(output, &units("[object Object]"), budget)?;
        }
    }
    Ok(true)
}

pub(super) fn json_number(value: &Json, budget: &mut Budget) -> Result<Option<f64>, String> {
    budget.tick()?;
    let number = match value {
        Json::Null => 0.0,
        Json::Bool(value) => if *value { 1.0 } else { 0.0 },
        Json::Num(value) => *value,
        Json::Str(value) => scalar::number(Some(value), Some(budget))?,
        Json::Obj(fields) => {
            if own_to_string(fields, budget)? { return Ok(None); }
            f64::NAN
        }
        Json::Arr(_) => {
            let mut primitive = Vec::new();
            if !append_string(value, &mut primitive, budget)? { return Ok(None); }
            scalar::number(Some(&primitive), Some(budget))?
        }
    };
    Ok(Some(number))
}

pub(super) fn evaluate(name: &str, args: &[Units], budget: &mut Budget) -> Result<Option<Units>, String> {
    budget.tick()?;
    if args.is_empty() { return Ok(None); }
    let mut aggregate = Aggregate::new(name)?;
    if args.len() > 1 {
        aggregate.admit(args.len())?;
        for value in args {
            budget.tick()?;
            aggregate.add(scalar::number(Some(value), Some(budget))?);
        }
    } else {
        if json_depth_exceeds(&args[0], 64) { return Err("json-depth".into()); }
        let values = array(&args[0]);
        aggregate.admit(values.len())?;
        for value in &values {
            let Some(number) = json_number(value, budget)? else { return Ok(None); };
            aggregate.add(number);
        }
    }
    Ok(Some(number_string(aggregate.finish())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extrema_select_the_javascript_zero_sign() {
        for values in [[0.0, -0.0], [-0.0, 0.0]] {
            let mut min = Aggregate::new("min").unwrap();
            let mut max = Aggregate::new("max").unwrap();
            for value in values { min.add(value); max.add(value); }
            assert!(min.finish().is_sign_negative());
            assert!(max.finish().is_sign_positive());
        }
    }
}
