//! Replacement tokens.
//!
//! `GetSubstitution` for a string replacement: `$$`, `$&`, `` $` ``, `$'`, `$n` and `$nn` (a two-digit reference
//! beyond the capture count reads as one digit and a literal), and `$<name>` when the pattern has named groups.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Literal(Vec<u16>),
    Match,
    Prefix,
    Suffix,
    /// 1-based capture group.
    Capture(usize),
    /// `$<name>`; only produced when the pattern has named groups.
    Named(Vec<u16>),
}

fn push_literal(tokens: &mut Vec<Token>, value: &[u16]) {
    if value.is_empty() { return; }
    if let Some(Token::Literal(previous)) = tokens.last_mut() { previous.extend_from_slice(value); }
    else { tokens.push(Token::Literal(value.to_vec())); }
}

fn digit(unit: Option<&u16>) -> Option<usize> { unit.filter(|unit| (48..=57).contains(*unit)).map(|unit| (*unit - 48) as usize) }

pub fn tokenize(template: &[u16], captures: usize, named: bool) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < template.len() {
        if template[index] != 36 || index + 1 == template.len() {
            push_literal(&mut tokens, &template[index..index + 1]);
            index += 1;
            continue;
        }
        match template[index + 1] {
            36 => { push_literal(&mut tokens, &[36]); index += 2; }
            38 => { tokens.push(Token::Match); index += 2; }
            96 => { tokens.push(Token::Prefix); index += 2; }
            39 => { tokens.push(Token::Suffix); index += 2; }
            48..=57 => {
                let first = digit(template.get(index + 1)).expect("digit");
                let (mut count, mut reference) = match digit(template.get(index + 2)) {
                    Some(second) => (2, first * 10 + second),
                    None => (1, first),
                };
                if reference > captures && count == 2 { count = 1; reference = first; }
                if (1..=captures).contains(&reference) { tokens.push(Token::Capture(reference)); }
                else { push_literal(&mut tokens, &template[index..index + 1 + count]); }
                index += 1 + count;
            }
            60 if named => match template[index..].iter().position(|unit| *unit == 62) {
                Some(end) => { tokens.push(Token::Named(template[index + 2..index + end].to_vec())); index += end + 1; }
                None => { push_literal(&mut tokens, &template[index..index + 2]); index += 2; }
            },
            _ => { push_literal(&mut tokens, &[36]); index += 1; }
        }
    }
    tokens
}

/// A match over UTF-16 input: the whole range first, then one entry per capture group.
pub struct UnitMatch<'a> {
    pub input: &'a [u16],
    pub groups: &'a [Option<Range<usize>>],
    /// `(name, group)` for every named group, in pattern order; a name can repeat.
    pub names: &'a [(Vec<u16>, usize)],
}

impl UnitMatch<'_> {
    /// The `groups` object entry for `name`: the participating group among those with that name.
    pub fn named(&self, name: &[u16]) -> Option<Range<usize>> {
        self.names.iter().filter(|(candidate, _)| candidate == name)
            .find_map(|(_, group)| self.groups.get(*group).cloned().flatten())
    }

    /// The slices the substitution appends, in order.
    fn pieces<'t>(&'t self, tokens: &'t [Token]) -> impl Iterator<Item = &'t [u16]> + 't {
        let whole = self.groups[0].clone().expect("whole match");
        tokens.iter().map(move |token| match token {
            Token::Literal(value) => &value[..],
            Token::Match => &self.input[whole.clone()],
            Token::Prefix => &self.input[..whole.start],
            Token::Suffix => &self.input[whole.end.min(self.input.len())..],
            Token::Capture(index) => self.groups.get(*index).cloned().flatten().map_or(&[][..], |range| &self.input[range]),
            Token::Named(name) => self.named(name).map_or(&[][..], |range| &self.input[range]),
        })
    }

    /// The length [`Self::substitute`] appends.
    pub fn substituted_len(&self, tokens: &[Token]) -> usize { self.pieces(tokens).map(<[u16]>::len).sum() }

    pub fn substitute(&self, tokens: &[Token], output: &mut Vec<u16>) {
        for piece in self.pieces(tokens) { output.extend_from_slice(piece); }
    }
}

/// [`UnitMatch::substitute`] over UTF-8 input with byte ranges, for a template that is well-formed UTF-16.
pub fn substitute_str(tokens: &[Token], input: &str, groups: &[Option<Range<usize>>], output: &mut String) {
    let whole = groups[0].clone().expect("whole match");
    for token in tokens {
        match token {
            Token::Literal(value) => output.push_str(&String::from_utf16_lossy(value)),
            Token::Match => output.push_str(&input[whole.clone()]),
            Token::Prefix => output.push_str(&input[..whole.start]),
            Token::Suffix => output.push_str(&input[whole.end.min(input.len())..]),
            Token::Capture(index) => {
                if let Some(range) = groups.get(*index).cloned().flatten() { output.push_str(&input[range]); }
            }
            // Patterns with named groups never take this path.
            Token::Named(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }
    fn apply(template: &str, input: &str, groups: &[Option<Range<usize>>], names: &[(&str, usize)]) -> String {
        let names: Vec<(Vec<u16>, usize)> = names.iter().map(|(name, group)| (units(name), *group)).collect();
        let input = units(input);
        let tokens = tokenize(&units(template), groups.len() - 1, !names.is_empty());
        let mut output = Vec::new();
        let found = UnitMatch { input: &input, groups, names: &names };
        found.substitute(&tokens, &mut output);
        assert_eq!(found.substituted_len(&tokens), output.len(), "{template}");
        String::from_utf16(&output).unwrap()
    }

    #[test]
    fn substitutes_like_get_substitution() {
        let groups = [Some(1..3), Some(1..2), None];
        assert_eq!(apply("$0:$00:$01:$02:$03:$10:$12:$99:$100:$", "xaby", &groups, &[]), "$0:$00:a::$03:a0:a2:$99:a00:$");
        assert_eq!(apply("$$-$&-$`-$'-$<x>", "xaby", &groups, &[]), "$-ab-x-y-$<x>");
        assert_eq!(apply("$<x>|$<y>|$<>|$<|$<x", "xaby", &groups, &[("x", 1), ("y", 2)]), "a|||$<|$<x");
        assert_eq!(apply("$<d>", "ab", &[Some(1..2), None, Some(1..2)], &[("d", 1), ("d", 2)]), "b");
    }
}
