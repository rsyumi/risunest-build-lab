use super::{number_string, units, ws, Budget, Units};

struct Work<'a> {
    budget: Option<&'a mut Budget>,
    count: usize,
}
impl Work<'_> {
    fn step(&mut self) -> Result<(), String> {
        if self.count % 256 == 0 {
            if let Some(budget) = self.budget.as_deref_mut() {
                budget.tick()?;
            }
        }
        self.count += 1;
        Ok(())
    }
}

fn trimmed<'a>(value: &'a [u16], work: &mut Work<'_>) -> Result<&'a [u16], String> {
    let mut start = 0;
    let mut end = value.len();
    while start < end && ws(value[start]) {
        work.step()?;
        start += 1;
    }
    while end > start && ws(value[end - 1]) {
        work.step()?;
        end -= 1;
    }
    Ok(&value[start..end])
}

fn digit(unit: u16) -> Option<u32> {
    match unit {
        48..=57 => Some(u32::from(unit - 48)),
        65..=70 => Some(u32::from(unit - 65 + 10)),
        97..=102 => Some(u32::from(unit - 97 + 10)),
        _ => None,
    }
}

// Power-of-two radices need only the leading significand, guard, and sticky bits.
fn radix(value: &[u16], width: u32, prefix: bool, work: &mut Work<'_>) -> Result<f64, String> {
    let mut significand = 0u64;
    let mut bits = 0usize;
    let mut guard = false;
    let mut sticky = false;
    let mut digits = 0usize;
    for &unit in value {
        work.step()?;
        let Some(digit) = digit(unit).filter(|&digit| digit < (1 << width)) else {
            if prefix { break; }
            return Ok(f64::NAN);
        };
        digits += 1;
        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1;
            if bits == 0 && bit == 0 { continue; }
            bits += 1;
            if bits <= 53 {
                significand = (significand << 1) | u64::from(bit);
            } else if bits == 54 {
                guard = bit != 0;
            } else {
                sticky |= bit != 0;
            }
        }
    }
    if digits == 0 { return Ok(f64::NAN); }
    if bits <= 53 { return Ok(significand as f64); }
    let mut exponent = bits - 1;
    if guard && (sticky || significand & 1 != 0) {
        significand += 1;
        if significand == 1 << 53 {
            significand >>= 1;
            exponent += 1;
        }
    }
    if exponent > 1023 { return Ok(f64::INFINITY); }
    Ok(f64::from_bits(((exponent as u64 + 1023) << 52) | (significand & ((1 << 52) - 1))))
}

pub(super) fn number(value: Option<&Units>, budget: Option<&mut Budget>) -> Result<f64, String> {
    let mut work = Work { budget, count: 0 };
    work.step()?;
    let Some(value) = value else { return Ok(f64::NAN); };
    let value = trimmed(value, &mut work)?;
    if value.is_empty() { return Ok(0.0); }
    if value.first() == Some(&48) {
        let width = match value.get(1) {
            Some(120 | 88) => Some(4),
            Some(98 | 66) => Some(1),
            Some(111 | 79) => Some(3),
            _ => None,
        };
        if let Some(width) = width { return radix(&value[2..], width, false, &mut work); }
    }
    // Every integer of up to 15 decimal digits, and each step towards it, is exact in a double.
    if value.len() <= 15 && value.iter().all(|c| (48..=57).contains(c)) {
        let mut n = 0.0;
        for &unit in value {
            work.step()?;
            n = n * 10.0 + f64::from(unit - 48);
        }
        return Ok(n);
    }
    let mut text = String::with_capacity(value.len());
    for &unit in value {
        work.step()?;
        if unit > 127 { return Ok(f64::NAN); }
        text.push(unit as u8 as char);
    }
    match text.as_str() {
        "Infinity" | "+Infinity" => Ok(f64::INFINITY),
        "-Infinity" => Ok(f64::NEG_INFINITY),
        _ if text.bytes().any(|c| !c.is_ascii_digit() && !b"+-.eE".contains(&c)) => Ok(f64::NAN),
        _ => Ok(text.parse().unwrap_or(f64::NAN)),
    }
}

pub(super) fn hexadecimal(value: Option<&Units>, budget: &mut Budget) -> Result<f64, String> {
    let mut work = Work { budget: Some(budget), count: 0 };
    work.step()?;
    let Some(value) = value else { return Ok(f64::NAN); };
    let mut value = trimmed(value, &mut work)?;
    let negative = value.first() == Some(&45);
    if matches!(value.first(), Some(43 | 45)) { value = &value[1..]; }
    if value.first() == Some(&48) && matches!(value.get(1), Some(120 | 88)) {
        value = &value[2..];
    }
    let result = radix(value, 4, true, &mut work)?;
    Ok(if negative { -result } else { result })
}

fn integer(value: f64) -> f64 {
    if value.is_nan() { 0.0 } else { value.trunc() }
}
fn code_unit(value: f64) -> u16 {
    if value.is_finite() { value.trunc().rem_euclid(65536.0) as u16 } else { 0 }
}
fn round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 { return value; }
    if value >= -0.5 && value < 0.0 { return -0.0; }
    let floor = value.floor();
    if value - floor < 0.5 { floor } else { floor + 1.0 }
}

// At most 403 bits are needed for abs(value) < 1e21 and 0..=100 fraction digits.
struct FixedInteger(Vec<u32>);
impl FixedInteger {
    fn new(value: u64) -> Self {
        let mut limbs = vec![value as u32, (value >> 32) as u32];
        while limbs.last() == Some(&0) { limbs.pop(); }
        Self(limbs)
    }
    fn multiply(&mut self, factor: u32) {
        let mut carry = 0u64;
        for limb in &mut self.0 {
            let product = u64::from(*limb) * u64::from(factor) + carry;
            *limb = product as u32;
            carry = product >> 32;
        }
        if carry != 0 { self.0.push(carry as u32); }
    }
    fn divide(&mut self, divisor: u32) -> u32 {
        let mut remainder = 0u64;
        for limb in self.0.iter_mut().rev() {
            let value = (remainder << 32) | u64::from(*limb);
            *limb = (value / u64::from(divisor)) as u32;
            remainder = value % u64::from(divisor);
        }
        while self.0.last() == Some(&0) { self.0.pop(); }
        remainder as u32
    }
    fn increment(&mut self) {
        for limb in &mut self.0 {
            let (next, carry) = limb.overflowing_add(1);
            *limb = next;
            if !carry { return; }
        }
        self.0.push(1);
    }
    fn decimal(mut self) -> String {
        let mut parts = Vec::new();
        while !self.0.is_empty() { parts.push(self.divide(1_000_000_000)); }
        let mut output = parts.pop().unwrap_or(0).to_string();
        while let Some(part) = parts.pop() { output.push_str(&format!("{part:09}")); }
        output
    }
}

fn fixed(value: f64, digits: f64, budget: &mut Budget) -> Result<Option<Units>, String> {
    let digits = integer(digits);
    if !(0.0..=100.0).contains(&digits) { return Ok(None); }
    if !value.is_finite() || value.abs() >= 1e21 { return Ok(Some(number_string(value))); }
    let digits = digits as usize;
    let negative = value < 0.0;
    let bits = value.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = (bits & ((1 << 52) - 1)) | if exponent == 0 { 0 } else { 1 << 52 };
    let exponent = if exponent == 0 { -1074 } else { exponent - 1023 - 52 };
    let mut scaled = FixedInteger::new(mantissa);
    for _ in 0..digits {
        budget.tick()?;
        scaled.multiply(5);
    }
    let shift = exponent + digits as i32;
    if shift >= 0 {
        for _ in 0..shift {
            budget.tick()?;
            scaled.multiply(2);
        }
    } else {
        let mut half = 0;
        for _ in 0..-shift {
            budget.tick()?;
            half = scaled.divide(2);
        }
        if half != 0 { scaled.increment(); }
    }
    let mut output = scaled.decimal();
    if digits > 0 {
        if output.len() <= digits { output = "0".repeat(digits + 1 - output.len()) + &output; }
        output.insert(output.len() - digits, '.');
    }
    if negative { output.insert(0, '-'); }
    Ok(Some(units(&output)))
}

pub(super) fn evaluate(name: &str, args: &[Units], budget: &mut Budget) -> Result<Option<Units>, String> {
    budget.tick()?;
    if name == "tonumber" {
        let mut output = Vec::new();
        let mut work = Work { budget: Some(budget), count: 0 };
        for &unit in &args[0] {
            work.step()?;
            if (48..=57).contains(&unit) || unit == 46 || ws(unit) {
                if output.len() >= work.budget.as_ref().unwrap().max_output { return Err("output-limit".into()); }
                output.push(unit);
            }
        }
        return Ok(Some(output));
    }
    if name == "unicodeencode" {
        let index = integer(number(args.get(1), Some(budget))?);
        let value = if index < 0.0 || index >= args[0].len() as f64 {
            f64::NAN
        } else {
            f64::from(args[0][index as usize])
        };
        return Ok(Some(number_string(value)));
    }
    if matches!(name, "u" | "ue" | "unicodedecodefromhex" | "unicodeencodefromhex") {
        return Ok(Some(vec![code_unit(hexadecimal(args.first(), budget)?)]));
    }
    let first = number(args.first(), Some(budget))?;
    let result = match name {
        "round" => number_string(round(first)),
        "floor" => number_string(first.floor()),
        "ceil" => number_string(first.ceil()),
        "abs" => number_string(first.abs()),
        "remaind" => number_string(first % number(args.get(1), Some(budget))?),
        "fixnum" | "fixnumber" => return fixed(first, number(args.get(1), Some(budget))?, budget),
        "unicodedecode" => vec![code_unit(first)],
        _ => return Err("unsupported-scalar-operator".into()),
    };
    Ok(Some(result))
}
