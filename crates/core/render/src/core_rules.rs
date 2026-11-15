//! The core rules of markdown-it 14.3.2 that run around the block and inline parsers: `normalize`, `replacements`,
//! `smartquotes` and `text_join` (`lib/rules_core/`). `linkify` is a no-op with `linkify: false`.

use crate::error::Result;
use crate::meter::Meter;
use crate::token::{Kind, Token};
use crate::utils::{find, find_unit, is_ascii_alpha, is_js_space, is_md_ascii_punct, is_punct_code, is_white_space, Units};
use std::collections::BTreeMap;

/// rules_core/normalize.mjs: `\r\n?` becomes `\n` and NUL becomes U+FFFD.
pub(crate) fn normalize(src: &[u16]) -> Units {
    let mut out = Units::with_capacity(src.len());
    let mut at = 0;
    while at < src.len() {
        match src[at] {
            0x0d => {
                out.push(0x0a);
                if src.get(at + 1) == Some(&0x0a) { at += 1; }
            }
            0 => out.push(0xfffd),
            unit => out.push(unit),
        }
        at += 1;
    }
    out
}

fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

/// `RARE_RE` /\+-|\.\.|\?\?\?\?|!!!!|,,|--/
fn has_rare(text: &[u16]) -> bool {
    ["+-", "..", "????", "!!!!", ",,", "--"].iter().any(|needle| find(text, needle, 0).is_some())
}

/// `SCOPED_ABBR_TEST_RE` /\((c|tm|r)\)/i at `at`: the replacement and the match length.
fn scoped_at(text: &[u16], at: usize) -> Option<(u16, usize)> {
    if text.get(at) != Some(&0x28) { return None; }
    let lower = |index: usize| text.get(index).filter(|unit| is_ascii_alpha(**unit)).map(|unit| unit | 0x20);
    match lower(at + 1) {
        Some(0x63) if text.get(at + 2) == Some(&0x29) => Some((0x00a9, 3)),
        Some(0x72) if text.get(at + 2) == Some(&0x29) => Some((0x00ae, 3)),
        Some(0x74) if lower(at + 2) == Some(0x6d) && text.get(at + 3) == Some(&0x29) => Some((0x2122, 4)),
        _ => None,
    }
}

fn has_scoped(text: &[u16]) -> bool {
    let mut from = 0;
    while let Some(at) = find_unit(text, 0x28, from) {
        if scoped_at(text, at).is_some() { return true; }
        from = at + 1;
    }
    false
}

/// `content.replace(SCOPED_ABBR_RE, replaceFn)`.
fn replace_scoped_text(text: &[u16]) -> Units {
    let mut out = Units::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if let Some((replacement, length)) = scoped_at(text, at) {
            out.push(replacement);
            at += length;
        } else {
            out.push(text[at]);
            at += 1;
        }
    }
    out
}

/// One left-to-right global replace with a matcher that returns `(match length, replacement)` at a position.
fn replace_all(text: &[u16], mut matcher: impl FnMut(&[u16], usize) -> Option<(usize, Units)>) -> Units {
    let mut out = Units::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if let Some((length, replacement)) = matcher(text, at) {
            out.extend_from_slice(&replacement);
            at += length;
        } else {
            out.push(text[at]);
            at += 1;
        }
    }
    out
}

fn run_of(text: &[u16], at: usize, accept: impl Fn(u16) -> bool) -> usize {
    text[at..].iter().take_while(|unit| accept(**unit)).count()
}

/// `^` with the `m` flag.
fn line_start(text: &[u16], at: usize) -> bool { at == 0 || is_line_terminator(text[at - 1]) }

/// `(^|<class>)<dashes>(?=<lookahead>)` with the `m` flag, replaced by `$1<dash>`.
fn dash_rule(text: &[u16], dashes: usize, dash: u16, class: impl Fn(u16) -> bool, ahead: impl Fn(&[u16], usize) -> bool) -> Units {
    let is_dashes = |text: &[u16], at: usize| text.len() >= at + dashes && text[at..at + dashes].iter().all(|unit| *unit == 0x2d);
    replace_all(text, |text, at| {
        if line_start(text, at) && is_dashes(text, at) && ahead(text, at + dashes) { return Some((dashes, vec![dash])); }
        if class(text[at]) && is_dashes(text, at + 1) && ahead(text, at + 1 + dashes) { return Some((1 + dashes, vec![text[at], dash])); }
        None
    })
}

/// The `replace_rare` chain of rules_core/replacements.mjs.
fn replace_rare_text(text: &[u16]) -> Units {
    // .replace(/\+-/g, '±')
    let text = replace_all(text, |text, at| (text[at] == 0x2b && text.get(at + 1) == Some(&0x2d)).then(|| (2, vec![0x00b1])));
    // .replace(/\.{2,}/g, '…')
    let text = replace_all(&text, |text, at| {
        let run = run_of(text, at, |unit| unit == 0x2e);
        (run >= 2).then(|| (run, vec![0x2026]))
    });
    // .replace(/([?!])…/g, '$1..')
    let text = replace_all(&text, |text, at| {
        (matches!(text[at], 0x3f | 0x21) && text.get(at + 1) == Some(&0x2026)).then(|| (2, vec![text[at], 0x2e, 0x2e]))
    });
    // .replace(/([?!]){4,}/g, '$1$1$1')
    let text = replace_all(&text, |text, at| {
        let run = run_of(text, at, |unit| matches!(unit, 0x3f | 0x21));
        (run >= 4).then(|| (run, vec![text[at + run - 1]; 3]))
    });
    // .replace(/,{2,}/g, ',')
    let text = replace_all(&text, |text, at| {
        let run = run_of(text, at, |unit| unit == 0x2c);
        (run >= 2).then(|| (run, vec![0x2c]))
    });
    // .replace(/(^|[^-])---(?=[^-]|$)/mg, '$1—')
    let text = dash_rule(&text, 3, 0x2014, |unit| unit != 0x2d, |text, at| at == text.len() || text[at] != 0x2d);
    // .replace(/(^|\s)--(?=\s|$)/mg, '$1–')
    let text = dash_rule(&text, 2, 0x2013, is_js_space, |text, at| at == text.len() || is_js_space(text[at]));
    // .replace(/(^|[^-\s])--(?=[^-\s]|$)/mg, '$1–')
    let not_dash_or_space = |unit: u16| unit != 0x2d && !is_js_space(unit);
    dash_rule(&text, 2, 0x2013, not_dash_or_space, |text, at| {
        at == text.len() || is_line_terminator(text[at]) || not_dash_or_space(text[at])
    })
}

/// `replace_scoped` and `replace_rare`: text outside autolinks, walked from the end.
fn replace_children(children: &mut [Token], replace: impl Fn(&[u16]) -> Option<Units>) {
    let mut inside_autolink = 0i32;
    for token in children.iter_mut().rev() {
        if token.kind == Kind::Text && inside_autolink == 0 {
            if let Some(replaced) = replace(&token.content) { token.content = replaced; }
        }
        if token.kind == Kind::LinkOpen && token.auto { inside_autolink -= 1; }
        if token.kind == Kind::LinkClose && token.auto { inside_autolink += 1; }
    }
}

/// rules_core/replacements.mjs.
pub(crate) fn replacements(tokens: &mut [Token], meter: &Meter) -> Result<()> {
    for token in tokens.iter_mut().rev() {
        if token.kind != Kind::Inline { continue; }
        meter.work(token.content.len())?;
        if has_scoped(&token.content) { replace_children(&mut token.children, |text| Some(replace_scoped_text(text))); }
        if has_rare(&token.content) {
            replace_children(&mut token.children, |text| has_rare(text).then(|| replace_rare_text(text)));
        }
    }
    Ok(())
}

struct Opener {
    token: usize,
    position: usize,
    single: bool,
    level: i32,
    previous_same: isize,
}

struct Heads {
    single: isize,
    double: isize,
}

const MAX_OPENERS: usize = 1000;
const APOSTROPHE: u16 = 0x2019;

fn truncate_stack(stack: &mut Vec<Opener>, heads: &mut Heads, length: usize) {
    while stack.len() > length {
        let item = stack.pop().expect("longer than length");
        if item.single { heads.single = item.previous_same; } else { heads.double = item.previous_same; }
    }
}

/// `process_inlines` (rules_core/smartquotes.mjs). `quotes` is `options.quotes`.
fn process_inlines(tokens: &mut [Token], quotes: [u16; 4], meter: &Meter) -> Result<()> {
    let mut stack: Vec<Opener> = Vec::new();
    let mut heads = Heads { single: -1, double: -1 };
    let mut replacements: BTreeMap<usize, Vec<(usize, u16)>> = BTreeMap::new();
    for index in 0..tokens.len() {
        let this_level = tokens[index].level;
        let mut j = stack.len() as isize - 1;
        while j >= 0 {
            if stack[j as usize].level <= this_level { break; }
            j -= 1;
        }
        truncate_stack(&mut stack, &mut heads, (j + 1) as usize);
        if tokens[index].kind != Kind::Text { continue; }
        let text = &tokens[index].content;
        meter.work(text.len())?;
        let max = text.len();
        let mut pos = 0;
        'outer: while pos < max {
            let Some(found) = text[pos..].iter().position(|unit| *unit == 0x27 || *unit == 0x22).map(|offset| pos + offset) else { break };
            let mut can_open = true;
            let mut can_close = true;
            pos = found + 1;
            let single = text[found] == 0x27;
            let mut last_char: u32 = 0x20;
            if found >= 1 {
                last_char = text[found - 1] as u32;
            } else {
                for previous in tokens[..index].iter().rev() {
                    if matches!(previous.kind, Kind::Softbreak | Kind::Hardbreak) { break; }
                    let Some(&unit) = previous.content.last() else { continue };
                    last_char = unit as u32;
                    break;
                }
            }
            let mut next_char: u32 = 0x20;
            if pos < max {
                next_char = text[pos] as u32;
            } else {
                for next in &tokens[index + 1..] {
                    if matches!(next.kind, Kind::Softbreak | Kind::Hardbreak) { break; }
                    let Some(&unit) = next.content.first() else { continue };
                    next_char = unit as u32;
                    break;
                }
            }
            let last_punct = is_md_ascii_punct(last_char) || is_punct_code(last_char);
            let next_punct = is_md_ascii_punct(next_char) || is_punct_code(next_char);
            let last_white = is_white_space(last_char);
            let next_white = is_white_space(next_char);
            if next_white {
                can_open = false;
            } else if next_punct && !(last_white || last_punct) {
                can_open = false;
            }
            if last_white {
                can_close = false;
            } else if last_punct && !(next_white || next_punct) {
                can_close = false;
            }
            if next_char == 0x22 && text[found] == 0x22 && (0x30..=0x39).contains(&last_char) {
                can_close = false;
                can_open = false;
            }
            if can_open && can_close {
                can_open = last_punct;
                can_close = next_punct;
            }
            if !can_open && !can_close {
                if single { replacements.entry(index).or_default().push((found, APOSTROPHE)); }
                continue;
            }
            if can_close {
                let head = if single { heads.single } else { heads.double };
                if head >= 0 && stack[head as usize].level == this_level {
                    let item = &stack[head as usize];
                    let (open, close) = if single { (quotes[2], quotes[3]) } else { (quotes[0], quotes[1]) };
                    replacements.entry(index).or_default().push((found, close));
                    replacements.entry(item.token).or_default().push((item.position, open));
                    truncate_stack(&mut stack, &mut heads, head as usize);
                    continue 'outer;
                }
            }
            if can_open {
                if stack.len() >= MAX_OPENERS { return Ok(()); }
                let previous_same = if single { heads.single } else { heads.double };
                stack.push(Opener { token: index, position: found, single, level: this_level, previous_same });
                if single { heads.single = stack.len() as isize - 1; } else { heads.double = stack.len() as isize - 1; }
            } else if can_close && single {
                replacements.entry(index).or_default().push((found, APOSTROPHE));
            }
        }
    }
    for (index, mut list) in replacements {
        list.sort_by_key(|(position, _)| *position);
        let text = &tokens[index].content;
        let mut result = Units::with_capacity(text.len());
        let mut last = 0;
        for (position, unit) in list {
            result.extend_from_slice(&text[last..position]);
            result.push(unit);
            last = position + 1;
        }
        result.extend_from_slice(&text[last..]);
        tokens[index].content = result;
    }
    Ok(())
}

/// rules_core/smartquotes.mjs.
pub(crate) fn smartquotes(tokens: &mut [Token], quotes: [u16; 4], meter: &Meter) -> Result<()> {
    for token in tokens.iter_mut().rev() {
        if token.kind != Kind::Inline || !token.content.iter().any(|unit| *unit == 0x27 || *unit == 0x22) { continue; }
        process_inlines(&mut token.children, quotes, meter)?;
    }
    Ok(())
}

/// rules_core/text_join.mjs: `text_special` becomes `text` and adjacent text tokens merge, in inline children only.
pub(crate) fn text_join(tokens: &mut [Token]) {
    for token in tokens.iter_mut() {
        if token.kind != Kind::Inline { continue; }
        let children = &mut token.children;
        for child in children.iter_mut() {
            if child.kind == Kind::TextSpecial { child.kind = Kind::Text; }
        }
        let max = children.len();
        let mut last = 0;
        for current in 0..max {
            if children[current].kind == Kind::Text && current + 1 < max && children[current + 1].kind == Kind::Text {
                let mut joined = std::mem::take(&mut children[current].content);
                joined.extend_from_slice(&children[current + 1].content);
                children[current + 1].content = joined;
            } else {
                if current != last { children.swap(last, current); }
                last += 1;
            }
        }
        children.truncate(last);
    }
}
