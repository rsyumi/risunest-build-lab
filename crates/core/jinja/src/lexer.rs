//! `lexer.ts`: `preprocess` with `lstrip_blocks` and `trim_blocks`, then `tokenize`.

use crate::error::{syntax, JinjaError};
use crate::units::{is_digit, is_word, is_ws, lossy, s, trim_end};
use risunest_ir::Budget;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    NumericLiteral,
    StringLiteral,
    Identifier,
    Equals,
    OpenParen,
    CloseParen,
    OpenStatement,
    CloseStatement,
    OpenExpression,
    CloseExpression,
    OpenSquareBracket,
    CloseSquareBracket,
    OpenCurlyBracket,
    CloseCurlyBracket,
    Comma,
    Dot,
    Colon,
    Pipe,
    AdditiveBinaryOperator,
    MultiplicativeBinaryOperator,
    ExponentiationBinaryOperator,
    ComparisonBinaryOperator,
    UnaryOperator,
    Comment,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) value: Vec<u16>,
}

const TABLE: &[(&str, Kind)] = &[
    ("{%", Kind::OpenStatement),
    ("%}", Kind::CloseStatement),
    ("{{", Kind::OpenExpression),
    ("}}", Kind::CloseExpression),
    ("(", Kind::OpenParen),
    (")", Kind::CloseParen),
    ("{", Kind::OpenCurlyBracket),
    ("}", Kind::CloseCurlyBracket),
    ("[", Kind::OpenSquareBracket),
    ("]", Kind::CloseSquareBracket),
    (",", Kind::Comma),
    (".", Kind::Dot),
    (":", Kind::Colon),
    ("|", Kind::Pipe),
    ("<=", Kind::ComparisonBinaryOperator),
    (">=", Kind::ComparisonBinaryOperator),
    ("==", Kind::ComparisonBinaryOperator),
    ("!=", Kind::ComparisonBinaryOperator),
    ("<", Kind::ComparisonBinaryOperator),
    (">", Kind::ComparisonBinaryOperator),
    ("+", Kind::AdditiveBinaryOperator),
    ("-", Kind::AdditiveBinaryOperator),
    ("~", Kind::AdditiveBinaryOperator),
    ("**", Kind::ExponentiationBinaryOperator),
    ("*", Kind::MultiplicativeBinaryOperator),
    ("//", Kind::MultiplicativeBinaryOperator),
    ("/", Kind::MultiplicativeBinaryOperator),
    ("%", Kind::MultiplicativeBinaryOperator),
    ("=", Kind::Equals),
];

const LBRACE: u16 = 0x7b;
const RBRACE: u16 = 0x7d;
const PERCENT: u16 = 0x25;
const HASH: u16 = 0x23;
const DASH: u16 = 0x2d;
const NEWLINE: u16 = 0x0a;

fn starts(units: &[u16], at: usize, ascii: &str) -> bool {
    let bytes = ascii.as_bytes();
    at <= units.len() && units.len() - at >= bytes.len() && bytes.iter().zip(&units[at..]).all(|(byte, unit)| *byte as u16 == *unit)
}

/// `preprocess(template, { lstrip_blocks: true, trim_blocks: true })`.
pub(crate) fn preprocess(source: &[u16], budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    budget.work(source.len().saturating_mul(3))?;
    let mut template: &[u16] = source;
    if template.last() == Some(&NEWLINE) { template = &template[..template.len() - 1]; }

    // /^[ \t]*({[#%-])/gm -> "$1": a line start is the input start or the unit after a line terminator.
    let mut stripped = Vec::with_capacity(template.len());
    let mut at = 0;
    let mut line_start = true;
    while at < template.len() {
        if line_start {
            let blank = template[at..].iter().take_while(|unit| matches!(**unit, 0x20 | 0x09)).count();
            let brace = at + blank;
            if template.get(brace) == Some(&LBRACE) && matches!(template.get(brace + 1), Some(&HASH | &PERCENT | &DASH)) {
                at = brace;
            }
            line_start = false;
            continue;
        }
        let unit = template[at];
        stripped.push(unit);
        at += 1;
        line_start = matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029);
    }

    // /([#%-]})\n/g -> "$1"
    let mut trimmed = Vec::with_capacity(stripped.len());
    let mut at = 0;
    while at < stripped.len() {
        if matches!(stripped[at], HASH | PERCENT | DASH) && stripped.get(at + 1) == Some(&RBRACE) && stripped.get(at + 2) == Some(&NEWLINE) {
            trimmed.extend([stripped[at], RBRACE]);
            at += 3;
            continue;
        }
        trimmed.push(stripped[at]);
        at += 1;
    }

    // /(\s*){%(-?)\s*(?:end)?generation\s*(-?)%}(\s*)/gs -> (lstrip ? "" : before) + (rstrip ? "" : after)
    let source = trimmed;
    let mut out = Vec::with_capacity(source.len());
    let mut at = 0;
    let skip_ws = |from: usize| from + source[from..].iter().take_while(|unit| is_ws(**unit)).count();
    while at < source.len() {
        let open = skip_ws(at);
        if let Some((lstrip, rstrip, close, end)) = generation_tag(&source, open, &skip_ws) {
            if !lstrip { out.extend_from_slice(&source[at..open]); }
            if !rstrip { out.extend_from_slice(&source[close..end]); }
            at = end;
            continue;
        }
        // Every start inside the same whitespace run reaches the same `open`, so none of them matches either.
        let next = if open > at { open } else { at + 1 };
        out.extend_from_slice(&source[at..next]);
        at = next;
    }
    Ok(out)
}

// Matches `{%(-?)\s*(?:end)?generation\s*(-?)%}(\s*)` at `open`: (lstrip, rstrip, end of `%}`, end of the trailing run).
fn generation_tag(source: &[u16], open: usize, skip_ws: &dyn Fn(usize) -> usize) -> Option<(bool, bool, usize, usize)> {
    if !starts(source, open, "{%") { return None; }
    let mut at = open + 2;
    let lstrip = source.get(at) == Some(&DASH);
    if lstrip { at += 1; }
    at = skip_ws(at);
    if starts(source, at, "endgeneration") { at += 13; } else if starts(source, at, "generation") { at += 10; } else { return None; }
    at = skip_ws(at);
    let rstrip = source.get(at) == Some(&DASH) && starts(source, at + 1, "%}");
    if rstrip { at += 1; }
    if !starts(source, at, "%}") { return None; }
    let close = at + 2;
    Some((lstrip, rstrip, close, skip_ws(close)))
}

struct Lexer<'s> {
    src: &'s [u16],
    pos: usize,
    tokens: Vec<Token>,
}

impl Lexer<'_> {
    fn at(&self, index: usize) -> Option<u16> { self.src.get(index).copied() }

    fn consume_while(&mut self, predicate: &dyn Fn(Option<u16>) -> bool) -> Result<Vec<u16>, JinjaError> {
        let mut out = Vec::new();
        while predicate(self.at(self.pos)) {
            if self.at(self.pos) == Some(0x5c) {
                self.pos += 1;
                if self.pos >= self.src.len() { return Err(syntax("Unexpected end of input")); }
                let escaped = self.src[self.pos];
                self.pos += 1;
                let unescaped = match escaped {
                    0x6e => 0x0a,
                    0x74 => 0x09,
                    0x72 => 0x0d,
                    0x62 => 0x08,
                    0x66 => 0x0c,
                    0x76 => 0x0b,
                    0x27 | 0x22 | 0x5c => escaped,
                    _ => return Err(syntax(format!("Unexpected escaped character: {}", lossy(&[escaped])))),
                };
                out.push(unescaped);
                continue;
            }
            // Past the end JS appends the string "undefined" and then fails the bound check below.
            let Some(unit) = self.at(self.pos) else { return Err(syntax("Unexpected end of input")) };
            out.push(unit);
            self.pos += 1;
            if self.pos >= self.src.len() { return Err(syntax("Unexpected end of input")); }
        }
        Ok(out)
    }

    fn strip_trailing_whitespace(&mut self) {
        if let Some(last) = self.tokens.last_mut() {
            if last.kind == Kind::Text {
                let length = trim_end(&last.value).len();
                last.value.truncate(length);
                if last.value.is_empty() { self.tokens.pop(); }
            }
        }
    }

    fn skip_leading_whitespace(&mut self) {
        while self.pos < self.src.len() && is_ws(self.src[self.pos]) { self.pos += 1; }
    }

    fn push(&mut self, kind: Kind, value: Vec<u16>) { self.tokens.push(Token { kind, value }); }
}

/// `tokenize(source, { lstrip_blocks: true, trim_blocks: true })`.
pub(crate) fn tokenize(source: &[u16], budget: &Budget) -> Result<Vec<Token>, JinjaError> {
    let src = preprocess(source, budget)?;
    let mut lexer = Lexer { src: &src, pos: 0, tokens: Vec::new() };
    let mut curly_depth: i64 = 0;
    let digit = |unit: Option<u16>| unit.is_some_and(is_digit);
    let whitespace = |unit: Option<u16>| unit.is_some_and(is_ws);
    let word = |unit: Option<u16>| unit.is_none_or(is_word);
    'main: while lexer.pos < src.len() {
        budget.work(1)?;
        let last = lexer.tokens.last().map(|token| token.kind);
        if matches!(last, None | Some(Kind::CloseStatement | Kind::CloseExpression | Kind::Comment)) {
            let start = lexer.pos;
            while lexer.pos < src.len()
                && !(src[lexer.pos] == LBRACE && matches!(lexer.at(lexer.pos + 1), Some(PERCENT | LBRACE | HASH))) {
                lexer.pos += 1;
            }
            if lexer.pos > start {
                budget.work(lexer.pos - start)?;
                lexer.push(Kind::Text, src[start..lexer.pos].to_vec());
                continue;
            }
        }

        if src[lexer.pos] == LBRACE && lexer.at(lexer.pos + 1) == Some(HASH) {
            lexer.pos += 2;
            let strip_before = lexer.at(lexer.pos) == Some(DASH);
            if strip_before { lexer.pos += 1; }
            let start = lexer.pos;
            while lexer.at(lexer.pos) != Some(HASH) || lexer.at(lexer.pos + 1) != Some(RBRACE) {
                if lexer.pos + 2 >= src.len() { return Err(syntax("Missing end of comment tag")); }
                lexer.pos += 1;
            }
            let mut comment = src[start..lexer.pos].to_vec();
            budget.work(comment.len())?;
            let strip_after = comment.last() == Some(&DASH);
            if strip_after { comment.pop(); }
            if strip_before { lexer.strip_trailing_whitespace(); }
            lexer.push(Kind::Comment, comment);
            lexer.pos += 2;
            if strip_after { lexer.skip_leading_whitespace(); }
            continue;
        }

        if starts(&src, lexer.pos, "{%-") {
            lexer.strip_trailing_whitespace();
            lexer.push(Kind::OpenStatement, s("{%"));
            lexer.pos += 3;
            continue;
        }
        if starts(&src, lexer.pos, "{{-") {
            lexer.strip_trailing_whitespace();
            lexer.push(Kind::OpenExpression, s("{{"));
            curly_depth = 0;
            lexer.pos += 3;
            continue;
        }

        lexer.consume_while(&whitespace)?;

        if starts(&src, lexer.pos, "-%}") {
            lexer.push(Kind::CloseStatement, s("%}"));
            lexer.pos += 3;
            lexer.skip_leading_whitespace();
            continue;
        }
        if starts(&src, lexer.pos, "-}}") {
            lexer.push(Kind::CloseExpression, s("}}"));
            lexer.pos += 3;
            lexer.skip_leading_whitespace();
            continue;
        }

        let Some(character) = lexer.at(lexer.pos) else { return Err(syntax("Unexpected end of input")) };

        if character == DASH || character == 0x2b {
            let last = lexer.tokens.last().map(|token| token.kind);
            match last {
                None | Some(Kind::Text) => return Err(syntax(format!("Unexpected character: {}", lossy(&[character])))),
                Some(Kind::Identifier | Kind::NumericLiteral | Kind::StringLiteral | Kind::CloseParen | Kind::CloseSquareBracket) => {}
                Some(_) => {
                    lexer.pos += 1;
                    let mut number = lexer.consume_while(&digit)?;
                    if !number.is_empty() && lexer.at(lexer.pos) == Some(0x2e) && lexer.at(lexer.pos + 1).is_some_and(is_digit) {
                        lexer.pos += 1;
                        let fraction = lexer.consume_while(&digit)?;
                        number.push(0x2e);
                        number.extend(fraction);
                    }
                    let kind = if number.is_empty() { Kind::UnaryOperator } else { Kind::NumericLiteral };
                    let mut value = vec![character];
                    value.extend(number);
                    lexer.push(kind, value);
                    continue;
                }
            }
        }

        for (sequence, kind) in TABLE {
            if *sequence == "}}" && curly_depth > 0 { continue; }
            if starts(&src, lexer.pos, sequence) {
                lexer.push(*kind, s(sequence));
                match kind {
                    Kind::OpenExpression => curly_depth = 0,
                    Kind::OpenCurlyBracket => curly_depth += 1,
                    Kind::CloseCurlyBracket => curly_depth -= 1,
                    _ => {}
                }
                lexer.pos += sequence.len();
                continue 'main;
            }
        }

        if character == 0x27 || character == 0x22 {
            lexer.pos += 1;
            let value = lexer.consume_while(&|unit: Option<u16>| unit != Some(character))?;
            budget.work(value.len())?;
            lexer.push(Kind::StringLiteral, value);
            lexer.pos += 1;
            continue;
        }

        if is_digit(character) {
            let mut number = lexer.consume_while(&digit)?;
            let after_dot = lexer.tokens.last().map(|token| token.kind) == Some(Kind::Dot);
            if !after_dot && lexer.at(lexer.pos) == Some(0x2e) && lexer.at(lexer.pos + 1).is_some_and(is_digit) {
                lexer.pos += 1;
                let fraction = lexer.consume_while(&digit)?;
                number.push(0x2e);
                number.extend(fraction);
            }
            lexer.push(Kind::NumericLiteral, number);
            continue;
        }
        if is_word(character) {
            let value = lexer.consume_while(&word)?;
            budget.work(value.len())?;
            lexer.push(Kind::Identifier, value);
            continue;
        }

        return Err(syntax(format!("Unexpected character: {}", lossy(&[character]))));
    }
    Ok(lexer.tokens)
}
