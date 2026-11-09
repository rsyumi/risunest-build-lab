use super::{compat, units, Budget, Context, Units};
use std::mem::size_of;

#[derive(Clone, Debug)]
pub struct Limits {
    pub functions: usize,
    pub frames: usize,
    pub source_units: usize,
    pub expanded_units: usize,
    pub retained_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self { functions: 256, frames: 32, source_units: 2_097_152,
            expanded_units: 8_388_608, retained_bytes: 32 * 1024 * 1024 }
    }
}
impl Limits {
    pub(super) fn admit(&self, source: &Units) -> Result<(), String> {
        if [self.functions, self.frames, self.source_units, self.expanded_units, self.retained_bytes].contains(&0) {
            return Err("invalid-program-limits".into());
        }
        if source.len() > self.source_units || source.capacity().saturating_mul(2) > self.retained_bytes {
            return Err("program-input-limit".into());
        }
        Ok(())
    }
}

enum Kind {
    Root,
    Macro(usize),
    Preserved,
    Ignore,
    If(bool),
    When(bool, bool),
    Function(Units),
}
struct Group { kind: Kind, text: Units }
struct State<'a> {
    limits: &'a Limits,
    functions: Vec<(Units, Units)>,
    calls: usize,
    frames: usize,
    expanded: usize,
    bytes: usize,
    history: bool,
}
const FRAME_BYTES: usize = size_of::<Vec<Group>>() + size_of::<std::collections::HashMap<Units, Units>>() + size_of::<usize>() * 4;

pub(super) fn evaluate(
    source: &Units, limits: &Limits, history: bool, context: &mut Context,
    budget: &mut Budget, effects: &mut Option<compat::State<'_>>,
) -> Result<Units, String> {
    let mut state = State { limits, functions: Vec::new(), calls: 0, frames: 0,
        expanded: 0, bytes: source.capacity() * 2, history };
    state.invoke(source, context, budget, effects)
}

fn normalized(value: &[u16], budget: &mut Budget) -> Result<String, String> {
    let end = value.iter().position(|&u| u == 58).unwrap_or(value.len());
    let mut name = String::new();
    name.try_reserve_exact(end).map_err(|_| "program-allocation")?;
    for &u in &value[..end] {
        budget.tick()?;
        if super::ws(u) || u == 95 || u == 45 { continue; }
        if u > 127 { return Err("non-ascii-dispatch".into()); }
        name.push((u as u8 as char).to_ascii_lowercase());
    }
    Ok(name)
}
fn boundary(value: &[u16], start: usize, separator: &[u16], budget: &mut Budget) -> Result<usize, String> {
    for at in start..=value.len().saturating_sub(separator.len()) {
        if (at - start) % 256 == 0 { budget.tick()?; }
        if value.get(at..at + separator.len()) == Some(separator) { return Ok(at); }
    }
    Ok(value.len())
}
fn first_argument<'a>(value: &'a [u16], budget: &mut Budget) -> Result<Option<&'a [u16]>, String> {
    let at = boundary(value, 0, &[58], budget)?;
    if at == value.len() { return Ok(None); }
    let separator: &[u16] = if value.get(at + 1) == Some(&58) { &[58, 58] } else { &[58] };
    let start = at + separator.len();
    let end = boundary(value, start, separator, budget)?;
    Ok(Some(&value[start..end]))
}
fn same(left: &[u16], right: &[u16], budget: &mut Budget) -> Result<bool, String> {
    budget.tick()?;
    if left.len() != right.len() { return Ok(false); }
    for (a, b) in left.chunks(256).zip(right.chunks(256)) {
        budget.tick()?;
        if a != b { return Ok(false); }
    }
    Ok(true)
}
impl State<'_> {
    fn room(&self, bytes: usize) -> Result<(), String> {
        if bytes > self.limits.retained_bytes.saturating_sub(self.bytes) {
            Err("program-retained-limit".into())
        } else { Ok(()) }
    }
    fn charge(&mut self, bytes: usize) -> Result<(), String> {
        self.bytes = self.bytes.checked_add(bytes).ok_or("program-retained-limit")?;
        if self.bytes > self.limits.retained_bytes { Err("program-retained-limit".into()) } else { Ok(()) }
    }
    fn release(&mut self, value: &Units) { self.bytes -= value.capacity() * 2; }
    fn append(&mut self, out: &mut Units, value: &[u16], budget: &mut Budget) -> Result<(), String> {
        budget.tick()?;
        if value.len() > budget.max_output.saturating_sub(out.len()) { return Err("output-limit".into()); }
        let wanted = out.len().checked_add(value.len()).ok_or("program-retained-limit")?;
        if wanted > out.capacity() {
            self.room(wanted.saturating_sub(out.capacity()).saturating_mul(2))?;
            let old = out.capacity();
            out.try_reserve_exact(wanted - out.len()).map_err(|_| "program-allocation")?;
            self.charge((out.capacity() - old) * 2)?;
        }
        for chunk in value.chunks(256) {
            budget.tick()?;
            out.extend_from_slice(chunk);
        }
        Ok(())
    }
    fn copy(&mut self, value: &[u16], budget: &mut Budget) -> Result<Units, String> {
        let mut result = Vec::new();
        self.append(&mut result, value, budget)?;
        Ok(result)
    }
    fn preserve(&mut self, out: &mut Units, value: &[u16], budget: &mut Budget) -> Result<(), String> {
        self.append(out, &[123, 123], budget)?;
        self.append(out, value, budget)?;
        self.append(out, &[125, 125], budget)
    }
    fn push(&mut self, groups: &mut Vec<Group>, kind: Kind, budget: &mut Budget) -> Result<(), String> {
        budget.tick()?;
        if groups.len() >= 128 { return Err("depth-limit".into()); }
        if groups.len() == groups.capacity() {
            self.room(size_of::<Group>())?;
            let old = groups.capacity();
            groups.try_reserve_exact(1).map_err(|_| "program-allocation")?;
            self.charge((groups.capacity() - old) * size_of::<Group>())?;
        }
        groups.push(Group { kind, text: Vec::new() });
        Ok(())
    }
    fn index(&self, name: &[u16], budget: &mut Budget) -> Result<Option<usize>, String> {
        for (i, (key, _)) in self.functions.iter().enumerate() {
            if same(key, name, budget)? { return Ok(Some(i)); }
        }
        Ok(None)
    }
    fn define(&mut self, name: Units, body: &Units, budget: &mut Budget) -> Result<(), String> {
        let trimmed = super::trim(body);
        if trimmed.len() > self.limits.source_units { return Err("program-source-limit".into()); }
        let index = self.index(&name, budget)?;
        if index.is_none() && self.functions.len() >= self.limits.functions {
            return Err("program-function-limit".into());
        }
        let data = self.copy(trimmed, budget)?;
        if let Some(i) = index {
            let (old_name, old_body) = std::mem::replace(&mut self.functions[i], (name, data));
            self.release(&old_name);
            self.release(&old_body);
        } else {
            if self.functions.len() == self.functions.capacity() {
                self.room(size_of::<(Units, Units)>())?;
                let old = self.functions.capacity();
                self.functions.try_reserve_exact(1).map_err(|_| "program-allocation")?;
                self.charge((self.functions.capacity() - old) * size_of::<(Units, Units)>())?;
            }
            self.functions.push((name, data));
        }
        Ok(())
    }
    fn call(&mut self, data: &[u16], context: &mut Context, budget: &mut Budget,
        effects: &mut Option<compat::State<'_>>) -> Result<Option<Units>, String>
    {
        if self.calls > 20 { return self.copy(&units("ERROR: Call stack limit reached"), budget).map(Some); }
        let arguments = &data[6..];
        let end = boundary(arguments, 0, &[58, 58], budget)?;
        let Some(index) = self.index(&arguments[..end], budget)? else { return Ok(None); };
        self.room(self.functions[index].1.len().saturating_mul(2))?;
        let mut body = compat::copy_units(&self.functions[index].1, budget)?;
        self.charge(body.capacity() * 2)?;
        let mut start = 0;
        let mut i = 0;
        loop {
            budget.tick()?;
            let end = boundary(arguments, start, &[58, 58], budget)?;
            let token = units(&format!("{{{{arg::{i}}}}}"));
            let scratch = token.capacity() * 2 + token.len() * size_of::<usize>();
            self.charge(scratch)?;
            let available = self.limits.expanded_units.saturating_sub(self.expanded)
                .min(self.limits.retained_bytes.saturating_sub(self.bytes) / 2);
            let result = super::replace_all(&body, &token, &arguments[start..end],
                available.min(budget.max_output), budget, Some(available)).map_err(|error| {
                    if error == "output-limit" && available < budget.max_output {
                        if self.limits.expanded_units.saturating_sub(self.expanded) <= available {
                            "program-expanded-limit".into()
                        } else { "program-retained-limit".into() }
                    } else { error }
                })?;
            self.charge(result.capacity() * 2)?;
            self.bytes -= scratch;
            self.expanded = self.expanded.checked_add(result.len()).ok_or("program-expanded-limit")?;
            self.release(&body);
            body = result;
            if end == arguments.len() { break; }
            start = end + 2;
            i += 1;
        }
        if body.len() > self.limits.source_units { return Err("program-source-limit".into()); }
        for _ in body.chunks(256) { budget.tick()?; }
        super::source_domain(&body)?;
        let result = self.invoke(&body, context, budget, effects);
        self.release(&body);
        result.map(Some)
    }
    fn temp_bytes(context: &Context, budget: &mut Budget) -> Result<usize, String> {
        let mut bytes = context.temp.capacity().saturating_mul(size_of::<(Units, Units)>() + 1);
        for (key, value) in &context.temp {
            budget.tick()?;
            bytes = bytes.saturating_add(key.capacity().saturating_mul(2))
                .saturating_add(value.capacity().saturating_mul(2));
        }
        Ok(bytes)
    }
    fn invoke(&mut self, source: &[u16], context: &mut Context, budget: &mut Budget,
        effects: &mut Option<compat::State<'_>>) -> Result<Units, String>
    {
        budget.tick()?;
        self.calls += 1;
        if self.calls > 20 { return self.copy(&units("ERROR: Call stack limit reached"), budget); }
        if self.frames >= self.limits.frames { return Err("program-frame-limit".into()); }
        self.charge(FRAME_BYTES)?;
        self.frames += 1;
        let parent_temp = std::mem::take(&mut context.temp);
        let mut temp_bytes = 0;
        let mut groups = Vec::new();
        let result = self.push(&mut groups, Kind::Root, budget)
            .and_then(|()| self.scan(source, &mut groups, context, budget, effects, &mut temp_bytes))
            .and_then(|output| { budget.tick()?; Ok(output) });
        for group in groups.drain(..) {
            self.release(&group.text);
            if let Kind::Function(name) = group.kind { self.release(&name); }
        }
        self.bytes -= groups.capacity() * size_of::<Group>() + temp_bytes + FRAME_BYTES;
        context.temp = parent_temp;
        self.frames -= 1;
        result
    }
    fn scan(&mut self, source: &[u16], groups: &mut Vec<Group>, context: &mut Context,
        budget: &mut Budget, effects: &mut Option<compat::State<'_>>, temp_bytes: &mut usize,
    ) -> Result<Units, String> {
        let mut at = 0;
        while at < source.len() {
            budget.tick()?;
            if source[at..].starts_with(&[123, 123]) {
                self.push(groups, Kind::Macro(at + 2), budget)?;
                at += 2;
                continue;
            }
            if source[at..].starts_with(&[125, 125]) && matches!(groups.last().unwrap().kind, Kind::Macro(_)) {
                let group = groups.pop().unwrap();
                let Kind::Macro(start) = group.kind else { unreachable!() };
                let result = self.macro_end(&group.text, &source[start..at], groups, context, budget, effects, temp_bytes);
                self.release(&group.text);
                if let Some(returned) = result? { return Ok(returned); }
                at += 2;
                continue;
            }
            let start = at;
            at += 1;
            while at < source.len() && !source[at..].starts_with(&[123, 123])
                && !source[at..].starts_with(&[125, 125]) {
                if (at - start) % 256 == 0 { budget.tick()?; }
                at += 1;
            }
            self.append(&mut groups.last_mut().unwrap().text, &source[start..at], budget)?;
        }
        if groups.len() != 1 { return Err("unbalanced".into()); }
        Ok(std::mem::take(&mut groups[0].text))
    }
    fn when_output(&mut self, body: &[u16], yes: bool, keep: bool, budget: &mut Budget) -> Result<Units, String> {
        let first_line = boundary(body, 0, &[10], budget)?;
        let token = [123, 123, 58, 101, 108, 115, 101, 125, 125];
        if first_line == body.len() {
            let separator = boundary(body, 0, &token, budget)?;
            let chosen = if separator < body.len() {
                if yes { &body[..separator] } else { &body[separator + token.len()..] }
            } else if yes { body } else { &[] };
            return self.copy(chosen, budget);
        }
        let mut start = 0;
        let mut end = body.len();
        let mut line_start = 0;
        let mut found = false;
        loop {
            let line_end = boundary(body, line_start, &[10], budget)?;
            for _ in body[line_start..line_end].chunks(256) { budget.tick()?; }
            if super::trim(&body[line_start..line_end]) == token {
                found = true;
                if yes { end = line_start.saturating_sub(1); }
                else { start = (line_end + 1).min(body.len()); }
                break;
            }
            if line_end == body.len() { break; }
            line_start = line_end + 1;
        }
        if !found && !yes { return Ok(Vec::new()); }
        if !keep {
            while start < end {
                let line_end = boundary(&body[..end], start, &[10], budget)?;
                for _ in body[start..line_end].chunks(256) { budget.tick()?; }
                if !super::trim(&body[start..line_end]).is_empty() { break; }
                start = (line_end + 1).min(end);
            }
            while start < end {
                let mut line_start = end;
                while line_start > start && body[line_start - 1] != 10 {
                    if (end - line_start) % 256 == 0 { budget.tick()?; }
                    line_start -= 1;
                }
                if !super::trim(&body[line_start..end]).is_empty() { break; }
                end = line_start.saturating_sub(1).max(start);
            }
        }
        self.copy(&body[start..end], budget)
    }

    fn macro_end(&mut self, data: &[u16], raw: &[u16], groups: &mut Vec<Group>, context: &mut Context,
        budget: &mut Budget, effects: &mut Option<compat::State<'_>>, temp_bytes: &mut usize,
    ) -> Result<Option<Units>, String> {
        for _ in data.chunks(256) { budget.tick()?; }
        let mut pure = false;
        for group in groups.iter() {
            budget.tick()?;
            pure |= matches!(group.kind, Kind::Ignore | Kind::Function(_));
        }
        if data.first().is_some_and(|u| *u == 35 || *u == 58) {
            let prefixes = ["#if_pure ", "#if ", "#when ", "#when::", "#func "];
            let prefix = prefixes.iter().position(|prefix| data.starts_with(&units(prefix)));
            if data == units(":else") && raw == data {
                self.preserve(&mut groups.last_mut().unwrap().text, data, budget)?;
                return Ok(None);
            }
            let Some(prefix) = prefix else { return Err("block-form".into()); };
            if !raw.starts_with(&units(prefixes[prefix])) { return Err("dynamic-block".into()); }
            if pure {
                self.preserve(&mut groups.last_mut().unwrap().text, data, budget)?;
                self.push(groups, Kind::Preserved, budget)?;
                return Ok(None);
            }
            let condition = &data[prefixes[prefix].len()..];
            let kind = match prefix {
                0 | 1 => {
                    let end = condition.iter().position(|&u| u == 32).unwrap_or(condition.len());
                    if super::truth(&condition[..end]) { Kind::If(prefix == 0) } else { Kind::Ignore }
                }
                2 | 3 => {
                    let (yes, keep) = super::when(condition, prefix as u8, context, effects, budget)?;
                    Kind::When(yes, keep)
                }
                4 => {
                    let end = condition.iter().position(|&u| u == 32).unwrap_or(condition.len());
                    Kind::Function(self.copy(&condition[..end], budget)?)
                }
                _ => unreachable!(),
            };
            self.push(groups, kind, budget)?;
            return Ok(None);
        }
        if data.starts_with(&[47]) && !data.starts_with(&[47, 47]) {
            if raw != data || raw.contains(&123) { return Err("dynamic-block-close".into()); }
            if matches!(groups.last().unwrap().kind, Kind::Root | Kind::Macro(_)) {
                return Err("block-form".into());
            }
            let group = groups.pop().unwrap();
            for _ in group.text.chunks(256) { budget.tick()?; }
            let result = match group.kind {
                Kind::Function(name) => self.define(name, &group.text, budget),
                Kind::Ignore => Ok(()),
                Kind::Preserved => {
                    self.append(&mut groups.last_mut().unwrap().text, &group.text, budget)?;
                    self.preserve(&mut groups.last_mut().unwrap().text, data, budget)
                }
                Kind::If(true) => self.append(&mut groups.last_mut().unwrap().text, &group.text, budget),
                Kind::If(false) => {
                    let body = super::trim(&group.text);
                    for (i, line) in body.split(|&u| u == 10).enumerate() {
                        if i > 0 { self.append(&mut groups.last_mut().unwrap().text, &[10], budget)?; }
                        let start = line.iter().position(|&u| !super::ws(u)).unwrap_or(line.len());
                        self.append(&mut groups.last_mut().unwrap().text, &line[start..], budget)?;
                    }
                    Ok(())
                }
                Kind::When(yes, keep) => {
                    let output = self.when_output(&group.text, yes, keep, budget)?;
                    let result = self.append(&mut groups.last_mut().unwrap().text, &output, budget);
                    self.release(&output);
                    result
                }
                _ => unreachable!(),
            };
            self.release(&group.text);
            result?;
            return Ok(None);
        }
        // Raw calls precede preservation, including while a function body is captured.
        if data.starts_with(&units("call::")) {
            if let Some(output) = self.call(data, context, budget, effects)? {
                let result = self.append(&mut groups.last_mut().unwrap().text, &output, budget);
                self.release(&output);
                result?;
            } else {
                self.preserve(&mut groups.last_mut().unwrap().text, data, budget)?;
            }
            return Ok(None);
        }
        let name_end = boundary(data, 0, &[58], budget)?;
        self.room(name_end)?;
        let name = if data.starts_with(&units("? ")) { "?".into() } else { normalized(data, budget)? };
        self.charge(name.capacity())?;
        let result = (|| {
            if !matches!(name.as_str(), "arg" | "return" | "call" | "?" | "randint" | "dice" | "roll")
                && !super::callback_admitted(&name, true, self.history) {
                return Err("unsupported-operator".into());
            }
            if pure || name == "arg" || name == "call" {
                self.preserve(&mut groups.last_mut().unwrap().text, data, budget)?;
                return Ok(None);
            }
            if name == "return" {
                return self.copy(first_argument(data, budget)?.unwrap_or(&[110, 117, 108, 108]), budget).map(Some);
            }
            let output_limit = budget.max_output;
            let output = super::evaluate_callback(data, context, budget, effects, output_limit)?;
            let now = Self::temp_bytes(context, budget)?;
            if now > *temp_bytes { self.charge(now - *temp_bytes)?; } else { self.bytes -= *temp_bytes - now; }
            *temp_bytes = now;
            self.charge(output.capacity() * 2)?;
            let result = self.append(&mut groups.last_mut().unwrap().text, &output, budget);
            self.release(&output);
            result?;
            Ok(None)
        })();
        self.bytes -= name.capacity();
        result
    }
}
