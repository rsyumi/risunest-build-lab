//! The safe Rust IR planner.
//!
//! Ports the lowering of `src/ts/process/regexSafePlan.ts` from pattern source and the compile step of
//! `src-tauri/src/regex_shadow.rs`. A pattern qualifies when its flags are `g`, `u` or both, its source is ASCII,
//! and it uses only literals (raw or escaped punctuation), positive ASCII classes, non-capturing groups, unnamed
//! captures outside quantifiers and greedy repeats bounded by 64, with no nullable alternative. Such a pattern is
//! valid JavaScript with or without `u`, never matches empty, and the `regex` crate's leftmost-first search returns
//! the match and captures a backtracking engine returns, so its replacement can run on UTF-8 text. Anything else
//! returns `None` and stays on the general engine.

use crate::replacement::{substitute_str, Token};
use regex::{Regex, RegexBuilder};

const NEST_LIMIT: usize = 29;
// regex-syntax also counts concat, alternation, bracketed class and class union nodes.
const COMPILE_NEST_LIMIT: u32 = NEST_LIMIT as u32 * 3 + 4;
const MAX_REPEAT: u32 = 64;
const MAX_CAPTURES: usize = 99;

#[derive(Debug)]
enum Atom {
    Literal(u8),
    Class(Vec<(u8, u8)>),
    Group(Vec<Vec<Atom>>),
    Capture(Vec<Vec<Atom>>),
    Repeat { min: u32, max: u32, atom: Box<Atom> },
}

struct Parser<'a> { source: &'a [u8], position: usize, unicode: bool, captures: usize }

const PUNCTUATION: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";
const SYNTAX: &[u8] = b"^$\\.*+?()[]{}|/";

impl Parser<'_> {
    fn peek(&self) -> Option<u8> { self.source.get(self.position).copied() }
    fn next(&mut self) -> Option<u8> { let value = self.peek()?; self.position += 1; Some(value) }

    fn disjunction(&mut self) -> Option<Vec<Vec<Atom>>> {
        let mut alternatives = vec![self.alternative()?];
        while self.peek() == Some(b'|') { self.position += 1; alternatives.push(self.alternative()?); }
        Some(alternatives)
    }

    fn alternative(&mut self) -> Option<Vec<Atom>> {
        let mut atoms = Vec::new();
        while let Some(value) = self.peek() {
            if value == b'|' || value == b')' { break; }
            atoms.push(self.term()?);
        }
        (!atoms.is_empty()).then_some(atoms)
    }

    fn term(&mut self) -> Option<Atom> {
        let atom = self.atom()?;
        let (min, max) = match self.peek() {
            Some(b'?') => { self.position += 1; (0, 1) }
            Some(b'{') => { self.position += 1; self.braces()? }
            Some(b'*' | b'+') => return None,
            _ => return Some(atom),
        };
        if self.peek() == Some(b'?') || min > max || max > MAX_REPEAT { return None; }
        Some(Atom::Repeat { min, max, atom: Box::new(atom) })
    }

    /// `{n}` or `{n,m}`; `{n,}` and anything else that is not a bounded quantifier is left to the general engine.
    fn braces(&mut self) -> Option<(u32, u32)> {
        let number = |parser: &mut Self| -> Option<u32> {
            let start = parser.position;
            while parser.peek().is_some_and(|value| value.is_ascii_digit()) { parser.position += 1; }
            if start == parser.position { return None; }
            let digits = std::str::from_utf8(&parser.source[start..parser.position]).ok()?;
            Some(digits.parse::<u64>().unwrap_or(u64::MAX).min(u32::MAX as u64) as u32)
        };
        let min = number(self)?;
        let max = if self.peek() == Some(b',') { self.position += 1; number(self)? } else { min };
        (self.next()? == b'}').then_some((min, max))
    }

    fn atom(&mut self) -> Option<Atom> {
        match self.next()? {
            b'(' => {
                let capture = if self.peek() == Some(b'?') {
                    self.position += 1;
                    if self.next()? != b':' { return None; }
                    false
                } else { self.captures += 1; true };
                let alternatives = self.disjunction()?;
                if self.next()? != b')' { return None; }
                Some(if capture { Atom::Capture(alternatives) } else { Atom::Group(alternatives) })
            }
            b'[' => self.class(),
            b'\\' => self.escape(false).map(Atom::Literal),
            b'^' | b'$' | b'.' | b'*' | b'+' | b'?' | b'{' | b'}' | b']' | b')' | b'|' => None,
            value => Some(Atom::Literal(value)),
        }
    }

    /// An identity escape of ASCII punctuation: any punctuation without `u`; with `u`, the syntax characters and `/`,
    /// plus `-` inside a class.
    fn escape(&mut self, in_class: bool) -> Option<u8> {
        let value = self.next()?;
        let allowed = if self.unicode { SYNTAX.contains(&value) || (in_class && value == b'-') } else { PUNCTUATION.contains(&value) };
        allowed.then_some(value)
    }

    fn class_atom(&mut self) -> Option<u8> {
        match self.next()? {
            b'\\' => self.escape(true),
            value => Some(value),
        }
    }

    fn class(&mut self) -> Option<Atom> {
        if self.peek() == Some(b'^') { return None; }
        let mut ranges = Vec::new();
        loop {
            if self.peek()? == b']' { self.position += 1; break; }
            let start = self.class_atom()?;
            if self.peek() == Some(b'-') && self.source.get(self.position + 1).is_some_and(|value| *value != b']') {
                self.position += 1;
                let end = self.class_atom()?;
                if start > end { return None; }
                ranges.push((start, end));
            } else { ranges.push((start, start)); }
        }
        (!ranges.is_empty()).then_some(Atom::Class(ranges))
    }
}

fn nullable(atom: &Atom) -> bool {
    match atom {
        Atom::Literal(_) | Atom::Class(_) => false,
        Atom::Group(alternatives) | Atom::Capture(alternatives) => alternatives.iter().any(|atoms| atoms.iter().all(nullable)),
        Atom::Repeat { min, atom, .. } => *min == 0 || nullable(atom),
    }
}

/// The lowering checks of `regexSafePlan.ts`: nesting depth, captures under a quantifier, nested quantifiers and
/// nullable groups.
fn validate(alternatives: &[Vec<Atom>], inside_quantifier: bool, depth: usize) -> bool {
    alternatives.iter().flatten().all(|atom| validate_atom(atom, inside_quantifier, depth))
}

fn validate_atom(atom: &Atom, inside_quantifier: bool, depth: usize) -> bool {
    match atom {
        Atom::Literal(_) | Atom::Class(_) => true,
        Atom::Group(inner) | Atom::Capture(inner) => {
            depth < NEST_LIMIT && !(matches!(atom, Atom::Capture(_)) && inside_quantifier)
                && validate(inner, inside_quantifier, depth + 1) && !inner.iter().any(|atoms| atoms.iter().all(nullable))
        }
        Atom::Repeat { atom, .. } => !inside_quantifier && depth < NEST_LIMIT && validate_atom(atom, true, depth + 1),
    }
}

fn build(alternatives: &[Vec<Atom>], output: &mut String) {
    for (index, atoms) in alternatives.iter().enumerate() {
        if index > 0 { output.push('|'); }
        for atom in atoms { build_atom(atom, output); }
    }
}

fn build_atom(atom: &Atom, output: &mut String) {
    match atom {
        Atom::Literal(value) => output.push_str(&format!(r"\x{value:02X}")),
        Atom::Class(ranges) => {
            output.push('[');
            for (start, end) in ranges {
                output.push_str(&format!(r"\x{start:02X}"));
                if start != end { output.push_str(&format!(r"-\x{end:02X}")); }
            }
            output.push(']');
        }
        Atom::Group(alternatives) => { output.push_str("(?:"); build(alternatives, output); output.push(')'); }
        Atom::Capture(alternatives) => { output.push('('); build(alternatives, output); output.push(')'); }
        Atom::Repeat { min, max, atom } => { build_atom(atom, output); output.push_str(&format!("{{{min},{max}}}")); }
    }
}

#[derive(Debug)]
pub struct SafeRegex {
    regex: Regex,
    global: bool,
    captures: usize,
}

/// The safe-IR compile of a pattern, or `None` when it is outside the subset.
pub fn compile(pattern: &[u16], flags: &str) -> Option<SafeRegex> {
    if !matches!(flags, "g" | "u" | "gu" | "ug") || pattern.iter().any(|unit| *unit > 0x7f) { return None; }
    let source: Vec<u8> = pattern.iter().map(|unit| *unit as u8).collect();
    let mut parser = Parser { source: &source, position: 0, unicode: flags.contains('u'), captures: 0 };
    let alternatives = parser.disjunction()?;
    if parser.position != source.len() || parser.captures > MAX_CAPTURES { return None; }
    if !validate(&alternatives, false, 0) || alternatives.iter().any(|atoms| atoms.iter().all(nullable)) { return None; }
    let mut built = String::new();
    build(&alternatives, &mut built);
    let regex = RegexBuilder::new(&built).nest_limit(COMPILE_NEST_LIMIT).build().ok()?;
    (regex.captures_len() == parser.captures + 1).then_some(SafeRegex { regex, global: flags.contains('g'), captures: parser.captures })
}

impl SafeRegex {
    pub fn captures(&self) -> usize { self.captures }
    pub fn is_match(&self, input: &str) -> bool { self.regex.is_match(input) }

    /// `input.replace(regex, template)`; every match is nonempty, so no empty-match advance applies.
    pub fn replace(&self, input: &str, tokens: &[Token]) -> String {
        let mut output = String::with_capacity(input.len());
        let mut end = 0;
        let mut groups = Vec::with_capacity(self.captures + 1);
        for found in self.regex.captures_iter(input) {
            groups.clear();
            groups.extend((0..=self.captures).map(|index| found.get(index).map(|group| group.range())));
            let whole = groups[0].clone().expect("whole match");
            output.push_str(&input[end..whole.start]);
            substitute_str(tokens, input, &groups, &mut output);
            end = whole.end;
            if !self.global { break; }
        }
        output.push_str(&input[end..]);
        output
    }
}
