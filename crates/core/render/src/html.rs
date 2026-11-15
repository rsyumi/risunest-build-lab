//! The HTML regexes of markdown-it 14.3.2 as anchored matchers over UTF-16 units: `HTML_TAG_RE` and
//! `HTML_OPEN_CLOSE_TAG_RE` (`lib/common/html_re.mjs`) and the `HTML_SEQUENCES` of `lib/rules_block/html_block.mjs`.
//! Every alternative is deterministic once the regex backtracking is worked out, so each matcher returns the length the
//! regex match would have.

use crate::utils::{find, find_unit, is_ascii_alnum, is_ascii_alpha, is_js_space, starts_with, starts_with_ignore_case};

fn is_attr_name_start(unit: u16) -> bool { is_ascii_alpha(unit) || matches!(unit, 0x5f | 0x3a) }
fn is_attr_name(unit: u16) -> bool { is_ascii_alnum(unit) || matches!(unit, 0x3a | 0x2e | 0x5f | 0x2d) }
fn is_unquoted(unit: u16) -> bool { !(unit <= 0x20 || matches!(unit, 0x22 | 0x27 | 0x3d | 0x3c | 0x3e | 0x60)) }
fn skip_spaces(src: &[u16], mut at: usize) -> usize {
    while at < src.len() && is_js_space(src[at]) { at += 1; }
    at
}

/// `attr_value`: unquoted, single quoted or double quoted.
fn attr_value(src: &[u16], at: usize) -> Option<usize> {
    match *src.get(at)? {
        quote @ (0x22 | 0x27) => find_unit(src, quote, at + 1).map(|end| end + 1),
        _ => {
            let run = src[at..].iter().take_while(|unit| is_unquoted(**unit)).count();
            (run > 0).then_some(at + run)
        }
    }
}

/// `open_tag`: `<[A-Za-z][A-Za-z0-9\-]*` attributes `\s*\/?>`.
pub(crate) fn open_tag(src: &[u16]) -> Option<usize> {
    if src.first() != Some(&0x3c) || !src.get(1).is_some_and(|unit| is_ascii_alpha(*unit)) { return None; }
    let mut at = 2;
    while at < src.len() && (is_ascii_alnum(src[at]) || src[at] == 0x2d) { at += 1; }
    loop {
        // `\s+` attr_name `(?:\s*=\s*` attr_value `)?`
        let name = skip_spaces(src, at);
        if name == at || !src.get(name).is_some_and(|unit| is_attr_name_start(*unit)) { break; }
        let mut end = name + 1;
        while end < src.len() && is_attr_name(src[end]) { end += 1; }
        let equals = skip_spaces(src, end);
        if src.get(equals) == Some(&0x3d) {
            if let Some(value) = attr_value(src, skip_spaces(src, equals + 1)) { end = value; }
        }
        at = end;
    }
    at = skip_spaces(src, at);
    if src.get(at) == Some(&0x2f) { at += 1; }
    (src.get(at) == Some(&0x3e)).then_some(at + 1)
}

/// `close_tag`: `<\/[A-Za-z][A-Za-z0-9\-]*\s*>`.
pub(crate) fn close_tag(src: &[u16]) -> Option<usize> {
    if !starts_with(src, "</") || !src.get(2).is_some_and(|unit| is_ascii_alpha(*unit)) { return None; }
    let mut at = 3;
    while at < src.len() && (is_ascii_alnum(src[at]) || src[at] == 0x2d) { at += 1; }
    at = skip_spaces(src, at);
    (src.get(at) == Some(&0x3e)).then_some(at + 1)
}

/// `comment`: `<!---?>|<!--(?:[^-]|-[^-]|--[^>])*-->`. The three loop items exclude each other, so the loop is one
/// chain of steps and the greedy match ends at the last step boundary followed by `-->`.
fn comment(src: &[u16]) -> Option<usize> {
    if !starts_with(src, "<!--") { return None; }
    if src.get(4) == Some(&0x3e) { return Some(5); }
    if src.get(4) == Some(&0x2d) && src.get(5) == Some(&0x3e) { return Some(6); }
    let mut boundaries = vec![4];
    let mut at = 4;
    while at < src.len() {
        if src[at] != 0x2d {
            at += 1;
        } else if src.get(at + 1).is_some_and(|unit| *unit != 0x2d) {
            at += 2;
        } else if src.get(at + 1) == Some(&0x2d) && src.get(at + 2).is_some_and(|unit| *unit != 0x3e) {
            at += 3;
        } else {
            break;
        }
        boundaries.push(at);
    }
    boundaries.into_iter().rev().find(|&end| starts_with(&src[end..], "-->")).map(|end| end + 3)
}

/// `HTML_TAG_RE` at the start of `src`: the length of the match.
pub(crate) fn html_tag(src: &[u16]) -> Option<usize> {
    if let Some(end) = open_tag(src).or_else(|| close_tag(src)).or_else(|| comment(src)) { return Some(end); }
    // processing `<[?][\s\S]*?[?]>`
    if starts_with(src, "<?") { return find(src, "?>", 2).map(|end| end + 2); }
    // declaration `<![A-Za-z][^>]*>`
    if starts_with(src, "<!") && src.get(2).is_some_and(|unit| is_ascii_alpha(*unit)) {
        return find_unit(src, 0x3e, 3).map(|end| end + 1);
    }
    // cdata `<!\[CDATA\[[\s\S]*?\]\]>`
    if starts_with(src, "<![CDATA[") { return find(src, "]]>", 9).map(|end| end + 3); }
    None
}

const BLOCK_NAMES: &[&str] = &[
    "address", "article", "aside", "base", "basefont", "blockquote", "body", "caption", "center", "col", "colgroup",
    "dd", "details", "dialog", "dir", "div", "dl", "dt", "fieldset", "figcaption", "figure", "footer", "form", "frame",
    "frameset", "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hr", "html", "iframe", "legend", "li", "link",
    "main", "menu", "menuitem", "nav", "noframes", "ol", "optgroup", "option", "p", "param", "search", "section",
    "summary", "table", "tbody", "td", "tfoot", "th", "thead", "title", "tr", "track", "ul",
];

/// The opening patterns of `HTML_SEQUENCES`, in order: the index of the first that matches `line`.
pub(crate) fn block_start(line: &[u16]) -> Option<usize> {
    // /^<(script|pre|style|textarea)(?=(\s|>|$))/i
    let raw = ["script", "pre", "style", "textarea"].iter().any(|name| {
        line.first() == Some(&0x3c) && starts_with_ignore_case(&line[1..], name)
            && line.get(1 + name.len()).map_or(true, |unit| is_js_space(*unit) || *unit == 0x3e)
    });
    if raw { return Some(0); }
    if starts_with(line, "<!--") { return Some(1); }
    if starts_with(line, "<?") { return Some(2); }
    if starts_with(line, "<!") && line.get(2).is_some_and(|unit| is_ascii_alpha(*unit)) { return Some(3); }
    if starts_with(line, "<![CDATA[") { return Some(4); }
    // ^</?(block names)(?=(\s|/?>|$)) with the i flag
    if line.first() == Some(&0x3c) {
        let at = if line.get(1) == Some(&0x2f) { 2 } else { 1 };
        let named = BLOCK_NAMES.iter().any(|name| {
            starts_with_ignore_case(&line[at..], name) && {
                let next = at + name.len();
                match line.get(next) {
                    None => true,
                    Some(&unit) => is_js_space(unit) || unit == 0x3e || (unit == 0x2f && line.get(next + 1) == Some(&0x3e)),
                }
            }
        });
        if named { return Some(5); }
    }
    // HTML_OPEN_CLOSE_TAG_RE followed by `\s*$`
    let tag = open_tag(line).or_else(|| close_tag(line));
    if tag.is_some_and(|end| line[end..].iter().all(|unit| is_js_space(*unit))) { return Some(6); }
    None
}

/// The closing pattern of sequence `kind` tested on a line.
pub(crate) fn block_end(kind: usize, line: &[u16]) -> bool {
    match kind {
        0 => (0..line.len()).any(|at| {
            starts_with(&line[at..], "</") && ["script", "pre", "style", "textarea"].iter().any(|name| {
                starts_with_ignore_case(&line[at + 2..], name) && line.get(at + 2 + name.len()) == Some(&0x3e)
            })
        }),
        1 => find(line, "-->", 0).is_some(),
        2 => find(line, "?>", 0).is_some(),
        3 => line.contains(&0x3e),
        4 => find(line, "]]>", 0).is_some(),
        _ => line.is_empty(),
    }
}

/// Whether sequence `kind` may interrupt a paragraph (`HTML_SEQUENCES[i][2]`).
pub(crate) fn block_interrupts(kind: usize) -> bool { kind != 6 }

/// Whether sequence `kind` also ends on a blank line (`HTML_SEQUENCES[i][1].test('')`).
pub(crate) fn block_ends_on_blank(kind: usize) -> bool { kind >= 5 }
