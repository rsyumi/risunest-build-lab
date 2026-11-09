//! Static proof for the configured CBS in an output replacement.

use risunest_cbs_experimental::cbs::{compile, units, ws, Node, Units};
use risunest_cbs_experimental::cbs::processor::{Snapshot, Source};
use risunest_cbs_experimental::cbs::parser::has_cbs_syntax;
use risunest_ir::stage::StageError;
use risunest_ir::{Budget, Charge};
use std::borrow::Cow;

const MAX_ALTERNATIVES: usize = 64;
const MAX_PROOF_UNITS: usize = 131_072;
const PROTOTYPE_KEYS: &[&str] = &["__defineGetter__", "__defineSetter__", "__lookupGetter__", "__lookupSetter__", "__proto__",
    "constructor", "hasOwnProperty", "isPrototypeOf", "propertyIsEnumerable", "toLocaleString", "toString", "valueOf"];

fn declined() -> StageError { StageError::Unsupported(Cow::Borrowed("regex.replacementCbs")) }
fn unreadable() -> StageError { StageError::Unsupported(Cow::Borrowed("cbs.snapshot")) }

// Numeric results contain no parser delimiters, but cannot select a statically known variable key.
enum Values { Finite(Vec<Units>), Numeric }

struct Proof<'a> {
    snapshot: &'a Snapshot,
    budget: &'a Budget,
    charges: Vec<Charge>,
}

pub(super) fn admit(snapshot: &Snapshot, replacement: &[u16], budget: &Budget) -> Result<(), StageError> {
    if replacement.len() > MAX_PROOF_UNITS { return Err(declined()); }
    budget.work(replacement.len())?;
    let _tree = budget.reserve_owned(replacement.len().saturating_mul(64).saturating_add(128))?;
    let nodes = compile(replacement).map_err(|_| declined())?;
    let mut proof = Proof { snapshot, budget, charges: Vec::new() };
    for node in &nodes {
        match node {
            Node::Text(value) if !has_cbs_syntax(value) => {}
            Node::Text(_) => return Err(declined()),
            Node::Macro(children) => { proof.call(children)?; }
            Node::Block { .. } => return Err(declined()),
        }
    }
    Ok(())
}

impl Proof<'_> {
    fn copy(&mut self, value: &[u16]) -> Result<Units, StageError> {
        if value.len() > MAX_PROOF_UNITS { return Err(declined()); }
        self.budget.work(value.len().saturating_add(1))?;
        self.charges.push(self.budget.reserve_owned(value.len().saturating_mul(2).saturating_add(128))?);
        Ok(value.to_vec())
    }

    fn inert(&mut self, value: &[u16]) -> Result<Units, StageError> {
        // A returned delimiter can combine with a surrounding literal at the next script/current-chat parse.
        self.budget.work(value.len())?;
        if value.iter().any(|unit| matches!(unit, 123 | 125 | 60)) { return Err(declined()); }
        self.copy(value)
    }

    fn expand(&mut self, nodes: &[Node]) -> Result<Vec<Units>, StageError> {
        let mut combinations = vec![self.copy(&[])?];
        for node in nodes {
            let choices = match node {
                Node::Text(value) => {
                    // Regex substitutions run before CBS and can change names, keys and argument boundaries.
                    if value.contains(&36) { return Err(declined()); }
                    vec![self.copy(value)?]
                }
                Node::Macro(children) => match self.call(children)? {
                    Values::Finite(values) => values,
                    Values::Numeric => return Err(declined()),
                },
                Node::Block { .. } => return Err(declined()),
            };
            if combinations.len().saturating_mul(choices.len()) > MAX_ALTERNATIVES { return Err(declined()); }
            let mut next = Vec::new();
            for prefix in &combinations {
                for suffix in &choices {
                    let length = prefix.len().saturating_add(suffix.len());
                    if length > MAX_PROOF_UNITS { return Err(declined()); }
                    self.budget.work(length)?;
                    self.charges.push(self.budget.reserve_owned(length.saturating_mul(2).saturating_add(128))?);
                    let mut value = Vec::with_capacity(length);
                    value.extend_from_slice(prefix);
                    value.extend_from_slice(suffix);
                    next.push(value);
                }
            }
            combinations = next;
        }
        Ok(combinations)
    }

    fn call(&mut self, nodes: &[Node]) -> Result<Values, StageError> {
        let Some(Node::Text(first)) = nodes.first() else { return Err(declined()) };
        let end = match first.iter().position(|unit| *unit == 58) {
            Some(end) => end,
            None if nodes.len() == 1 => first.len(),
            None => return Err(declined()),
        };
        let name = &first[..end];
        if self.snapshot.tailored_case && name.iter().any(|unit| matches!(unit, 0x49 | 0x130)) { return Err(declined()); }
        let mut normalized = String::new();
        for unit in name.iter().copied().filter(|unit| !ws(*unit) && !matches!(unit, 95 | 45)) {
            if unit > 127 { return Err(declined()); }
            normalized.push((unit as u8).to_ascii_lowercase() as char);
        }
        let id = match normalized.as_str() {
            "char" | "bot" => "char",
            "user" => "user",
            "chatindex" => "chatindex",
            "getvar" => "getvar",
            "getglobalvar" => "getglobalvar",
            "random" => "random",
            "blank" | "none" => "blank",
            "br" | "newline" => "br",
            _ => return Err(declined()),
        };
        if self.snapshot.declined.contains(&id) { return Err(unreadable()); }
        let expanded = self.expand(nodes)?;
        let mut output = Vec::new();
        for body in &expanded {
            let arguments = arguments(body);
            if matches!(id, "random" | "chatindex") && arguments.is_empty() { return Ok(Values::Numeric); }
            let values = match id {
                "getvar" | "getglobalvar" if arguments.len() <= 1 => {
                    let value = self.variable(id == "getglobalvar", arguments.first().copied())?;
                    vec![value]
                }
                "char" | "user" | "blank" | "br" if arguments.is_empty() => vec![self.name(id)?],
                "random" => self.choices(&arguments)?,
                _ => return Err(declined()),
            };
            if output.len().saturating_add(values.len()) > MAX_ALTERNATIVES { return Err(declined()); }
            output.extend(values);
        }
        Ok(Values::Finite(output))
    }

    fn name(&mut self, id: &str) -> Result<Units, StageError> {
        match id {
            "blank" => self.copy(&[]),
            "br" => self.copy(&[10]),
            "user" if self.snapshot.consistant_char => self.copy(&units("username")),
            "user" => self.inert(&self.snapshot.user),
            "char" if self.snapshot.consistant_char => self.copy(&units("botname")),
            "char" => {
                let Source::Database(database) = &self.snapshot.source else { return Err(declined()) };
                let Some(character) = &database.character else { return Err(declined()) };
                self.inert(if character.nickname.is_empty() { &character.name } else { &character.nickname })
            }
            _ => Err(declined()),
        }
    }

    fn variable(&mut self, global: bool, key: Option<&[u16]>) -> Result<Units, StageError> {
        let missing = units("undefined");
        let lookup = key.unwrap_or(&missing);
        let prototype = || PROTOTYPE_KEYS.iter().any(|candidate| lookup.iter().copied().eq(candidate.encode_utf16()));
        let value = match &self.snapshot.source {
            Source::Reads(reads) => {
                if prototype() { return Err(declined()); }
                if global { reads.global_variables.get(lookup) }
                else { reads.chat_variables.get(lookup).or_else(|| key.and_then(|key| reads.default_variables.get(key))) }
                    .map(Vec::as_slice)
            }
            Source::Database(database) => {
                if global {
                    if prototype() { return Err(declined()); }
                    database.character.as_ref().and_then(|character| character.chat.local_variables.get(lookup))
                        .filter(|value| !value.is_empty() && value.as_slice() != [110, 117, 108, 108])
                        .or_else(|| database.global_variables.get(lookup)).map(Vec::as_slice)
                } else if let Some(character) = &database.character {
                    if character.chat.unreadable_variables.contains(lookup) { return Err(unreadable()); }
                    self.budget.work(character.default_variables.len().saturating_add(database.template_default_variables.len()))?;
                    character.chat.variables.get(lookup).map(Vec::as_slice).or_else(|| key.and_then(|key| {
                        default_value(&character.default_variables, key).or_else(|| default_value(&database.template_default_variables, key))
                    }))
                } else { None }
            }
        };
        match value { Some(value) => self.inert(value), None => self.copy(&units("null")) }
    }

    fn choices(&mut self, args: &[&[u16]]) -> Result<Vec<Units>, StageError> {
        // JSON decoding and escaped choices can manufacture parser delimiters, outside this finite literal subset.
        if args.is_empty() || args.iter().any(|arg| arg.contains(&92) || arg.windows(2).any(|pair| pair == [167, 88])) {
            return Err(declined());
        }
        let choices = if args.len() == 1 {
            let value = args[0];
            if value.first() == Some(&91) && value.last() == Some(&93) { return Err(declined()); }
            value.split(|unit| matches!(unit, 58 | 44)).take(MAX_ALTERNATIVES + 1).collect::<Vec<_>>()
        } else { args.to_vec() };
        if choices.len() > MAX_ALTERNATIVES { return Err(declined()); }
        choices.into_iter().map(|choice| self.inert(choice)).collect()
    }
}

fn arguments(body: &[u16]) -> Vec<&[u16]> {
    let Some(at) = body.iter().position(|unit| *unit == 58) else { return Vec::new() };
    if body.get(at + 1) != Some(&58) { return body[at + 1..].split(|unit| *unit == 58).collect(); }
    let mut remaining = &body[at + 2..];
    let mut args = Vec::new();
    while let Some(at) = remaining.windows(2).position(|pair| pair == [58, 58]) {
        args.push(&remaining[..at]);
        remaining = &remaining[at + 2..];
    }
    args.push(remaining);
    args
}

fn default_value<'a>(source: &'a [u16], key: &[u16]) -> Option<&'a [u16]> {
    source.split(|unit| *unit == 10).find_map(|line| {
        let mut parts = line.split(|unit| *unit == 61);
        let (candidate, value) = (parts.next()?, parts.next()?);
        (!candidate.is_empty() && !value.is_empty() && candidate == key).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_cbs_experimental::cbs::processor::{Character, Database};
    use risunest_ir::Limits;
    use std::sync::{Arc, atomic::AtomicBool};

    fn budget() -> Budget {
        Budget::new(Limits { max_text_units: 1_000_000, max_json_bytes: 1_000_000, max_values: 10_000, max_depth: 128,
            max_owned_bytes: 32_000_000, max_work: 10_000_000 },
            Arc::new(AtomicBool::new(false))).unwrap()
    }

    fn snapshot() -> Snapshot {
        let mut character = Character { name: units("Synthetic char"), default_variables: units("mood=default\nmood=later\nfallback=first=ignored"), ..Character::default() };
        character.chat.variables.insert(units("mood"), units("calm"));
        character.chat.variables.insert(units("key"), units("mood"));
        character.chat.variables.insert(units("unused"), units("{{screenwidth}}"));
        character.chat.unreadable_variables.insert(units("broken"));
        let database = Database { character: Some(character), template_default_variables: units("mood=template\nother=second"), ..Database::default() };
        Snapshot { user: units("Synthetic user"), source: Source::Database(Box::new(database)), ..Snapshot::default() }
    }

    #[test]
    fn admits_every_choice_and_resolves_nested_reads_without_drawing() {
        let snapshot = snapshot();
        for source in ["{", "}}", "$1 $& $<word>", "{{char}} {{user}} {{chatindex}} {{chat_index}} {{random}}",
            "{{getvar::{{getvar::key}}}}", "{{random::{{char}}::{{user}}}}", "{{random::one,two}}",
            "{{getvar::{{random::mood::fallback::other}}}}"] {
            assert!(admit(&snapshot, &units(source), &budget()).is_ok(), "{source}");
        }
    }

    #[test]
    fn refuses_unmatched_configured_host_reads_and_every_generated_choice() {
        let snapshot = snapshot();
        for source in ["{{screenwidth}}", "{{data}}", "{{getvar::unused}}", "{{getvar::broken}}", "{{{{getvar::key}}}}",
            "{{getvar::$1}}", "{{getvar::{{data}}}}", "{{getvar::{{random::mood::broken}}}}",
            "{{random::[\"safe\",\"\\u007b\\u007bscreenwidth\\u007d\\u007d\"]}}", "{{description}}",
            "{{#func f}}{{screenwidth}}{{/func}}{{call::f}}", "{{u::007b007b}}", "{{#if 0}}{{screenwidth}}{{/if}}"] {
            assert!(admit(&snapshot, &units(source), &budget()).is_err(), "{source}");
        }
    }

    #[test]
    fn read_precedence_is_lazy_and_preserves_exact_default_keys() {
        let mut snapshot = snapshot();
        let Source::Database(database) = &mut snapshot.source else { unreachable!() };
        let character = database.character.as_mut().unwrap();
        character.default_variables = units("mood={{screenwidth}}\nfallback=first=ignored\n spaced=literal\nempty=\nempty=safe");
        database.global_variables.insert(units("setting"), units("{{screenwidth}}"));
        character.chat.local_variables.insert(units("setting"), units("safe"));
        let budget = budget();
        let mut proof = Proof { snapshot: &snapshot, budget: &budget, charges: Vec::new() };
        for (global, key, expected) in [(false, "mood", "calm"), (false, "fallback", "first"),
            (false, " spaced", "literal"), (false, "spaced", "null"), (false, "empty", "safe"), (true, "setting", "safe")] {
            assert_eq!(proof.variable(global, Some(&units(key))).unwrap(), units(expected));
        }
        assert!(proof.variable(true, Some(&units("toString"))).is_err());
    }

    #[test]
    fn returned_names_and_values_cannot_create_a_later_macro() {
        let mut snapshot = snapshot();
        snapshot.user = units("{");
        assert!(admit(&snapshot, &units("{ {{user}}"), &budget()).is_err());
        snapshot.user = units("<user>");
        assert!(admit(&snapshot, &units("{{user}}"), &budget()).is_err());
        snapshot.declined.push("user");
        snapshot.user = units("unused placeholder");
        assert!(matches!(admit(&snapshot, &units("{{user}}"), &budget()), Err(StageError::Unsupported(name)) if name == "cbs.snapshot"));
    }
}
