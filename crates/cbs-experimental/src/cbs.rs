mod array_ops;
mod aggregates;
pub mod compat;
pub mod history;
pub mod program;
mod deterministic_hash;
mod scalar;
mod calc;
mod families;
mod host;
mod local_time;
#[cfg(test)]
mod fixtures;
pub mod parser;
pub mod processor;
mod text;
pub use calc::{calc_string, js_string};
pub use deterministic_hash::{pick_hash_rand, Sfc32};

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
pub type Units = Vec<u16>;
pub fn units(s: &str) -> Units {
    s.encode_utf16().collect()
}
fn ascii(s: &[u16]) -> Result<String, String> {
    if s.iter().any(|&c| c > 127) {
        return Err("non-ascii-dispatch".into());
    }
    Ok(s.iter().map(|&c| c as u8 as char).collect())
}
pub fn ws(c: u16) -> bool {
    matches!(c, 9..=13 | 32 | 160 | 5760 | 8192..=8202 | 8232 | 8233 | 8239 | 8287 | 12288 | 65279)
}
fn trim(s: &[u16]) -> &[u16] {
    let a = s.iter().position(|&c| !ws(c)).unwrap_or(s.len());
    let b = s.iter().rposition(|&c| !ws(c)).map(|n| n + 1).unwrap_or(a);
    &s[a..b]
}
fn split(s: &[u16], sep: &[u16]) -> Vec<Units> {
    split_with(s, sep, <[u16]>::to_vec)
}
// `split` without copying the parts.
fn split_slices<'a>(s: &'a [u16], sep: &[u16]) -> Vec<&'a [u16]> {
    split_with(s, sep, |part| part)
}
fn split_with<'a, T>(s: &'a [u16], sep: &[u16], mut part: impl FnMut(&'a [u16]) -> T) -> Vec<T> {
    if sep.is_empty() {
        return s.chunks(1).map(part).collect();
    }
    let mut start = 0;
    let mut out = Vec::new();
    let mut p = 0;
    while p + sep.len() <= s.len() {
        if &s[p..p + sep.len()] == sep {
            out.push(part(&s[start..p]));
            p += sep.len();
            start = p
        } else {
            p += 1
        }
    }
    out.push(part(&s[start..]));
    out
}
fn join(a: &[Units], sep: &[u16]) -> Units {
    let mut o = Vec::new();
    for (i, s) in a.iter().enumerate() {
        if i > 0 {
            o.extend(sep)
        }
        o.extend(s)
    }
    o
}
fn replace_literal(s: &[u16], from: &[u16], to: &[u16]) -> Units {
    if from.is_empty() {
        return join(&split(s, from), to);
    }
    let mut o = Vec::with_capacity(s.len());
    let (mut start, mut p) = (0, 0);
    while p + from.len() <= s.len() {
        if &s[p..p + from.len()] == from {
            o.extend_from_slice(&s[start..p]);
            o.extend_from_slice(to);
            p += from.len();
            start = p;
        } else {
            p += 1;
        }
    }
    o.extend_from_slice(&s[start..]);
    o
}
fn append_replacement(
    out: &mut Units,
    value: &[u16],
    limit: usize,
    b: &mut Budget,
    capacity_limit: Option<usize>,
) -> Result<(), String> {
    if value.len() > limit.saturating_sub(out.len()) {
        return Err("output-limit".into());
    }
    if let Some(capacity_limit) = capacity_limit {
        out.try_reserve_exact(value.len()).map_err(|_| "program-allocation")?;
        if out.capacity() > capacity_limit { return Err("program-retained-limit".into()); }
    }
    for chunk in value.chunks(1024) {
        b.tick()?;
        out.extend_from_slice(chunk);
    }
    Ok(())
}
fn replacement_work(work: &mut usize, b: &mut Budget) -> Result<(), String> {
    if *work % 1024 == 0 {
        b.tick()?;
    }
    *work += 1;
    Ok(())
}
fn replace_all(
    input: &[u16],
    search: &[u16],
    replacement: &[u16],
    limit: usize,
    b: &mut Budget,
    capacity_limit: Option<usize>,
) -> Result<Units, String> {
    fn substitute(
        out: &mut Units,
        input: &[u16],
        start: usize,
        end: usize,
        replacement: &[u16],
        limit: usize,
        b: &mut Budget,
        capacity_limit: Option<usize>,
    ) -> Result<(), String> {
        let mut literal = 0;
        let mut at = 0;
        let mut work = 0;
        while at < replacement.len() {
            replacement_work(&mut work, b)?;
            let value = if replacement[at] == 36 {
                match replacement.get(at + 1) {
                    Some(36) => Some(&replacement[at..at + 1]),
                    Some(38) => Some(&input[start..end]),
                    Some(96) => Some(&input[..start]),
                    Some(39) => Some(&input[end..]),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(value) = value {
                append_replacement(out, &replacement[literal..at], limit, b, capacity_limit)?;
                append_replacement(out, value, limit, b, capacity_limit)?;
                at += 2;
                literal = at;
            } else {
                at += 1;
            }
        }
        append_replacement(out, &replacement[literal..], limit, b, capacity_limit)
    }
    let mut out = Vec::new();
    if search.is_empty() {
        for at in 0..=input.len() {
            b.tick()?;
            substitute(&mut out, input, at, at, replacement, limit, b, capacity_limit)?;
            if at < input.len() {
                append_replacement(&mut out, &input[at..at + 1], limit, b, capacity_limit)?;
            }
        }
        return Ok(out);
    }
    if search.len() > input.len() {
        append_replacement(&mut out, input, limit, b, capacity_limit)?;
        return Ok(out);
    }
    // Prefix links keep repeated partial matches linear without changing UTF-16 boundaries.
    let mut prefix = if capacity_limit.is_some() {
        let mut prefix = Vec::new();
        prefix.try_reserve_exact(search.len()).map_err(|_| "program-allocation")?;
        prefix.resize(search.len(), 0);
        prefix
    } else { vec![0; search.len()] };
    let mut matched = 0;
    let mut work = 0;
    for at in 1..search.len() {
        replacement_work(&mut work, b)?;
        while matched > 0 && search[at] != search[matched] {
            replacement_work(&mut work, b)?;
            matched = prefix[matched - 1];
        }
        if search[at] == search[matched] {
            matched += 1;
        }
        prefix[at] = matched;
    }
    matched = 0;
    let mut tail = 0;
    for (at, &unit) in input.iter().enumerate() {
        replacement_work(&mut work, b)?;
        while matched > 0 && unit != search[matched] {
            replacement_work(&mut work, b)?;
            matched = prefix[matched - 1];
        }
        if unit == search[matched] {
            matched += 1;
        }
        if matched == search.len() {
            let end = at + 1;
            let start = end - search.len();
            append_replacement(&mut out, &input[tail..start], limit, b, capacity_limit)?;
            substitute(&mut out, input, start, end, replacement, limit, b, capacity_limit)?;
            tail = end;
            matched = 0;
        }
    }
    append_replacement(&mut out, &input[tail..], limit, b, capacity_limit)?;
    Ok(out)
}
fn truth(s: &[u16]) -> bool {
    s == units("1") || s == units("true")
}
pub fn number_string(n: f64) -> Units {
    units(ryu_js::Buffer::new().format(n))
}
fn number(s: Option<&Units>) -> Result<f64, String> {
    scalar::number(s, None)
}

fn float(s: &[u16]) -> f64 {
    let s = trim(s);
    let s: String = s
        .iter()
        .take_while(|&&c| c <= 127)
        .map(|&c| c as u8 as char)
        .collect();
    if s.starts_with("Infinity") || s.starts_with("+Infinity") {
        return f64::INFINITY;
    }
    if s.starts_with("-Infinity") {
        return f64::NEG_INFINITY;
    }
    for end in (1..=s.len()).rev() {
        let p = &s[..end];
        if p.bytes()
            .all(|c| c.is_ascii_digit() || b"+-.eE".contains(&c))
        {
            if let Ok(v) = p.parse() {
                return v;
            }
        }
    }
    f64::NAN
}
#[derive(Clone, Debug)]
pub enum Node {
    Text(Units),
    Macro(Vec<Node>),
    Block {
        kind: u8,
        condition: Vec<Node>,
        body: Vec<Node>,
    },
}
// Function calls can execute even inside a false if block in the original parser.
fn lazy_body_admitted(nodes: &[Node], writers: bool, history: bool) -> bool {
    nodes.iter().all(|node| match node {
        Node::Text(_) => true,
        Node::Block {
            condition, body, ..
        } => lazy_body_admitted(condition, writers, history) && lazy_body_admitted(body, writers, history),
        Node::Macro(children) => {
            if !lazy_body_admitted(children, writers, history) {
                return false;
            }
            let Some(Node::Text(first)) = children.first() else {
                return false;
            };
            if first.starts_with(&units("? ")) {
                return true;
            }
            let end = match first.iter().position(|&c| c == 58) {
                Some(end) => end,
                None if children.len() == 1 => first.len(),
                None => return false,
            };
            let name: Units = first[..end]
                .iter()
                .copied()
                .filter(|&c| !ws(c) && c != 95 && c != 45)
                .collect();
            let Ok(name) = ascii(&name) else {
                return false;
            };
            callback_admitted(&name.to_ascii_lowercase(), writers, history)
        }
    })
}
fn callback_admitted(name: &str, writers: bool, history: bool) -> bool {
            (history && matches!(name, "lastmessage" | "lastmessageid" | "lastmessageindex" | "previouschatlog" | "role")) ||
            (writers && matches!(name, "setvar" | "addvar" | "setdefaultvar")) || matches!(
                name,
                "getvar"
                    | "getglobalvar"
                    | "chatindex"
                    | "user"
                    | "tempvar"
                    | "gettempvar"
                    | "settempvar"
                    | "equal"
                    | "notequal"
                    | "greater"
                    | "less"
                    | "greaterequal"
                    | "lessequal"
                    | "and"
                    | "or"
                    | "not"
                    | "length"
                    | "trim"
                    | "contains"
                    | "startswith"
                    | "endswith"
                    | "replace"
                    | "split"
                    | "makearray"
                    | "array"
                    | "join"
                    | "spread"
                    | "arraylength"
                    | "arrayelement"
                    | "arrayshift"
                    | "arraypop"
                    | "arraypush"
                    | "arraysplice"
                    | "min" | "max" | "sum" | "average"
                    | "calc"
                    | "hash"
                    | "round"
                    | "floor"
                    | "ceil"
                    | "abs"
                    | "remaind"
                    | "tonumber"
                    | "fixnum"
                    | "fixnumber"
                    | "unicodeencode"
                    | "unicodedecode"
                    | "u"
                    | "ue"
                    | "unicodedecodefromhex"
                    | "unicodeencodefromhex"
                    | "random"
                    | "blank"
                    | "br"
            )
}
pub fn compile(text: &[u16]) -> Result<Vec<Node>, String> {
    compile_mode(text, false, false)
}
fn source_domain(text: &[u16]) -> Result<(), String> {
    if text.len() > 2_097_152 {
        return Err("source-limit".into());
    }
    if text
        .windows(2)
        .enumerate()
        .any(|(i, s)| s == [35, 125] || (s == [123, 35] && (i == 0 || text[i - 1] != 123)))
        || ["<user>", "<char>", "<bot>"].iter().any(|token| {
            text.windows(token.len()).any(|part| {
                part.iter()
                    .zip(token.bytes())
                    .all(|(&a, b)| a == b as u16 || (a >= 65 && a <= 90 && a + 32 == b as u16))
            })
        })
    {
        return Err("legacy".into());
    }
    Ok(())
}
fn compile_mode(text: &[u16], writers: bool, history: bool) -> Result<Vec<Node>, String> {
    source_domain(text)?;
    fn seq(s: &[u16], p: &mut usize, end: u8, depth: usize, writers: bool, history: bool) -> Result<Vec<Node>, String> {
        if depth > 128 {
            return Err("depth-limit".into());
        }
        let mut out = Vec::new();
        while *p < s.len() {
            if end == 1 && s[*p..].starts_with(&[125, 125]) {
                *p += 2;
                return Ok(out);
            }
            if end == 2 && s[*p..].starts_with(&[123, 123, 47]) && s.get(*p + 3) != Some(&47) {
                let Some(n) = s[*p..].windows(2).position(|w| w == [125, 125]) else {
                    return Err("unclosed-block".into());
                };
                if s[*p + 3..*p + n].contains(&123) {
                    return Err("dynamic-block-close".into());
                }
                *p += n + 2;
                return Ok(out);
            }
            if s[*p..].starts_with(&units("{{:else}}")) {
                out.push(Node::Text(units("{{:else}}")));
                *p += 9;
                continue;
            }
            if s[*p..].starts_with(&[123, 123]) {
                *p += 2;
                let mut v = seq(s, p, 1, depth + 1, writers, history)?;
                let mut block = None;
                if let Some(Node::Text(first)) = v.first_mut() {
                    for (prefix, kind) in
                        [("#if_pure ", 0), ("#if ", 1), ("#when ", 2), ("#when::", 3)]
                    {
                        let pre = units(prefix);
                        if first.starts_with(&pre) {
                            *first = first[pre.len()..].to_vec();
                            block = Some(kind);
                            break;
                        }
                    }
                    if block.is_none() && first.first().is_some_and(|c| [35, 47, 58].contains(c)) {
                        return Err("block-form".into());
                    }
                }
                if let Some(kind) = block {
                    let body = seq(s, p, 2, depth + 1, writers, history)?;
                    if kind < 2 && !lazy_body_admitted(&body, writers, history) {
                        return Err("lazy-body-domain".into());
                    }
                    out.push(Node::Block {
                        kind,
                        condition: v,
                        body,
                    })
                } else {
                    out.push(Node::Macro(v))
                }
            } else {
                let start = *p;
                *p += 1;
                while *p < s.len()
                    && !s[*p..].starts_with(&[123, 123])
                    && !(end == 1 && s[*p..].starts_with(&[125, 125]))
                {
                    *p += 1
                }
                out.push(Node::Text(s[start..*p].to_vec()))
            }
        }
        if end > 0 {
            return Err("unbalanced".into());
        }
        Ok(out)
    }
    seq(text, &mut 0, 0, 0, writers, history)
}
#[derive(Clone, Default)]
pub struct ChoiceTape {
    pub indices: Vec<usize>,
    pub position: usize,
    pub next_arity: Option<usize>,
}
#[derive(Clone, Default)]
pub struct Context {
    pub variables: HashMap<Units, Units>,
    // Keys whose stored value getChatVar's `toString()` throws on: a read declines, and a write replaces the value.
    pub unreadable: HashSet<Units>,
    // The getChatVar defaults for a key `variables` does not hold.
    pub defaults: HashMap<Units, Units>,
    pub globals: HashMap<Units, Units>,
    pub temp: HashMap<Units, Units>,
    pub index: i64,
    pub user: Units,
    pub state: u32,
    pub draws: usize,
    pub choices: Option<ChoiceTape>,
    // The captured database the host registrations read; None declines them.
    pub host: Option<Box<host::Host>>,
    // Registry ids the caller declines.
    pub declined: Vec<&'static str>,
}
fn finite_choice(c: &mut Context, arity: usize) -> Result<usize, String> {
    if arity == 0 {
        return Ok(0);
    }
    if arity > 16 {
        return Err("rng-choice-arity".into());
    }
    let tape = c.choices.as_mut().ok_or("rng-mode")?;
    let Some(&index) = tape.indices.get(tape.position) else {
        tape.next_arity = Some(arity);
        return Err("rng-choice-needed".into());
    };
    if index >= arity {
        return Err("rng-choice-index".into());
    }
    tape.position += 1;
    Ok(index)
}
fn integer_domain(n: f64, low: f64, high: f64) -> bool {
    n.is_finite() && n.fract() == 0.0 && n >= low && n <= high
}
fn finite_rng(name: &str, args: &[Units], c: &mut Context) -> Result<Units, String> {
    if c.choices.is_none() {
        return Err("unsupported-operator".into());
    }
    if name == "randint" {
        if args.len() != 2 {
            return Err("rng-integer-domain".into());
        }
        let lo = number(args.first())?;
        let hi = number(args.get(1))?;
        if !integer_domain(lo, -9_007_199_254_740_991.0, 9_007_199_254_740_991.0)
            || !integer_domain(hi, lo, 9_007_199_254_740_991.0)
            || hi - lo + 1.0 > 16.0
        {
            return Err("rng-integer-domain".into());
        }
        return Ok(number_string(
            lo + finite_choice(c, (hi - lo + 1.0) as usize)? as f64,
        ));
    }
    if name == "roll" && args.is_empty() {
        return Ok(units("1"));
    }
    let Some(notation) = args.first() else {
        return Err("rng-dice-domain".into());
    };
    let parts = split(notation, &[100]);
    let (count, sides) = if name == "roll" {
        match parts.len() {
            1 => (1.0, number(parts.first())?),
            2 => (
                if parts[0].is_empty() {
                    1.0
                } else {
                    number(parts.first())?
                },
                if parts[1].is_empty() {
                    6.0
                } else {
                    number(parts.get(1))?
                },
            ),
            _ => return Err("rng-dice-domain".into()),
        }
    } else {
        if parts.len() != 2 {
            return Err("rng-dice-domain".into());
        }
        (number(parts.first())?, number(parts.get(1))?)
    };
    if !integer_domain(count, if name == "roll" { 1.0 } else { 0.0 }, 8.0)
        || !integer_domain(sides, 1.0, 16.0)
    {
        return Err("rng-dice-domain".into());
    }
    let mut total = 0usize;
    for _ in 0..count as usize {
        total += finite_choice(c, sides as usize)? + 1;
    }
    Ok(number_string(total as f64))
}
pub struct Budget {
    pub remaining: usize,
    pub max_output: usize,
    pub cancel: Arc<AtomicBool>,
    // A stage budget this evaluation also charges, one work unit per tick in chunks of SHARED_CHUNK.
    pub(crate) shared: Option<risunest_ir::Budget>,
    pub(crate) pending: usize,
    #[cfg(test)]
    pub progress: Arc<std::sync::atomic::AtomicUsize>,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            remaining: 1_000_000,
            max_output: 4_194_304,
            cancel: Arc::new(AtomicBool::new(false)),
            shared: None,
            pending: 0,
            #[cfg(test)]
            progress: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}
const SHARED_CHUNK: usize = 256;
// An ir budget or random-source failure as an evaluation reason that keeps the limit or invariant name.
fn shared_error(error: risunest_ir::Error) -> String {
    match error {
        risunest_ir::Error::Cancelled => "cancelled".into(),
        risunest_ir::Error::Limit(name) => format!("limit:{name}"),
        risunest_ir::Error::Invariant(name) => format!("invalid:{name}"),
        other => format!("invalid:{other:?}"),
    }
}
impl Budget {
    // An evaluation bounded by a stage budget: its work limit, cancellation and text-unit limit.
    pub fn shared(shared: &risunest_ir::Budget) -> Self {
        Self { remaining: usize::MAX, max_output: shared.limits().max_text_units, shared: Some(shared.clone()), ..Self::default() }
    }
    fn tick(&mut self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if self.remaining == 0 {
            return Err("budget".into());
        }
        self.remaining -= 1;
        #[cfg(test)]
        self.progress.fetch_add(1, Ordering::Relaxed);
        if let Some(shared) = &self.shared {
            self.pending += 1;
            if self.pending == SHARED_CHUNK {
                self.pending = 0;
                shared.work(SHARED_CHUNK).map_err(shared_error)?;
            }
        }
        Ok(())
    }
    // Charges the ticks not yet passed to the stage budget.
    pub(crate) fn settle(&mut self) -> Result<(), String> {
        let pending = std::mem::take(&mut self.pending);
        match &self.shared {
            Some(shared) => shared.work(pending).map_err(shared_error),
            None => Ok(()),
        }
    }
}
#[derive(Clone, Debug)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(Units),
    Arr(Vec<Json>),
    Obj(Vec<(Units, Json)>),
}
fn json_parse(s: &[u16]) -> Option<Json> {
    fn ws_json(s: &[u16], p: &mut usize) {
        while *p < s.len() && matches!(s[*p], 9 | 10 | 13 | 32) {
            *p += 1
        }
    }
    fn value(s: &[u16], p: &mut usize, d: usize) -> Option<Json> {
        if d > 64 {
            return None;
        }
        ws_json(s, p);
        let c = *s.get(*p)?;
        *p += 1;
        match c {
            34 => {
                let mut o = Vec::new();
                loop {
                    let c = *s.get(*p)?;
                    *p += 1;
                    if c == 34 {
                        return Some(Json::Str(o));
                    }
                    if c < 32 {
                        return None;
                    }
                    if c != 92 {
                        o.push(c);
                        continue;
                    }
                    let e = *s.get(*p)?;
                    *p += 1;
                    o.push(match e {
                        34 | 92 | 47 => e,
                        98 => 8,
                        102 => 12,
                        110 => 10,
                        114 => 13,
                        116 => 9,
                        117 => {
                            let h = s.get(*p..*p + 4)?;
                            let t = ascii(h).ok()?;
                            if !t.bytes().all(|c| c.is_ascii_hexdigit()) {
                                return None;
                            }
                            *p += 4;
                            u16::from_str_radix(&t, 16).ok()?
                        }
                        _ => return None,
                    })
                }
            }
            91 => {
                let mut o = Vec::new();
                ws_json(s, p);
                if s.get(*p) == Some(&93) {
                    *p += 1;
                    return Some(Json::Arr(o));
                }
                loop {
                    o.push(value(s, p, d + 1)?);
                    ws_json(s, p);
                    let c = *s.get(*p)?;
                    *p += 1;
                    if c == 93 {
                        break;
                    }
                    if c != 44 {
                        return None;
                    }
                }
                Some(Json::Arr(o))
            }
            123 => {
                let mut o: Vec<(Units, Json)> = Vec::new();
                ws_json(s, p);
                if s.get(*p) == Some(&125) {
                    *p += 1;
                    return Some(Json::Obj(o));
                }
                loop {
                    let Json::Str(k) = value(s, p, d + 1)? else {
                        return None;
                    };
                    ws_json(s, p);
                    if s.get(*p) != Some(&58) {
                        return None;
                    }
                    *p += 1;
                    let v = value(s, p, d + 1)?;
                    if let Some(pair) = o.iter_mut().find(|(key, _)| *key == k) {
                        pair.1 = v
                    } else {
                        o.push((k, v))
                    }
                    ws_json(s, p);
                    let c = *s.get(*p)?;
                    *p += 1;
                    if c == 125 {
                        break;
                    }
                    if c != 44 {
                        return None;
                    }
                }
                Some(Json::Obj(o))
            }
            _ => {
                *p -= 1;
                for (word, v) in [
                    (&[110, 117, 108, 108][..], Json::Null),
                    (&[116, 114, 117, 101][..], Json::Bool(true)),
                    (&[102, 97, 108, 115, 101][..], Json::Bool(false)),
                ] {
                    if s[*p..].starts_with(word) {
                        *p += word.len();
                        return Some(v);
                    }
                }
                let start = *p;
                if s.get(*p) == Some(&45) {
                    *p += 1
                }
                if s.get(*p) == Some(&48) {
                    *p += 1
                } else {
                    if !s.get(*p).is_some_and(|c| (49..=57).contains(c)) {
                        return None;
                    }
                    while s.get(*p).is_some_and(|c| (48..=57).contains(c)) {
                        *p += 1
                    }
                }
                if s.get(*p) == Some(&46) {
                    *p += 1;
                    let a = *p;
                    while s.get(*p).is_some_and(|c| (48..=57).contains(c)) {
                        *p += 1
                    }
                    if a == *p {
                        return None;
                    }
                }
                if s.get(*p).is_some_and(|c| *c == 101 || *c == 69) {
                    *p += 1;
                    if s.get(*p).is_some_and(|c| *c == 43 || *c == 45) {
                        *p += 1
                    }
                    let a = *p;
                    while s.get(*p).is_some_and(|c| (48..=57).contains(c)) {
                        *p += 1
                    }
                    if a == *p {
                        return None;
                    }
                }
                Some(Json::Num(ascii(&s[start..*p]).ok()?.parse().ok()?))
            }
        }
    }
    let mut p = 0;
    let v = value(s, &mut p, 0)?;
    ws_json(s, &mut p);
    if p == s.len() {
        Some(v)
    } else {
        None
    }
}
// JSON.stringify of a string, appended to `o`.
fn quote_into(o: &mut Units, s: &[u16]) {
    fn escape(o: &mut Units, c: u16) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        o.extend_from_slice(&[92, 117]);
        o.extend([12, 8, 4, 0].map(|shift| u16::from(HEX[usize::from((c >> shift) & 15)])));
    }
    o.push(34);
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            34 | 92 => o.extend_from_slice(&[92, c]),
            8 => o.extend_from_slice(&[92, 98]),
            12 => o.extend_from_slice(&[92, 102]),
            10 => o.extend_from_slice(&[92, 110]),
            13 => o.extend_from_slice(&[92, 114]),
            9 => o.extend_from_slice(&[92, 116]),
            0..=31 => escape(o, c),
            0xd800..=0xdbff if s.get(i + 1).is_some_and(|c| (0xdc00..=0xdfff).contains(c)) => {
                o.push(c);
                i += 1;
                o.push(s[i]);
            }
            0xd800..=0xdfff => escape(o, c),
            _ => o.push(c),
        }
        i += 1
    }
    o.push(34);
}
fn json_string(v: &Json) -> Units {
    let mut o = Vec::new();
    json_into(&mut o, v);
    o
}
// JSON.stringify(v), appended to `o`.
fn json_into(o: &mut Units, v: &Json) {
    match v {
        Json::Null => o.extend_from_slice(&[110, 117, 108, 108]),
        Json::Bool(b) => o.extend((if *b { "true" } else { "false" }).encode_utf16()),
        Json::Num(n) => {
            if n.is_finite() {
                o.extend(ryu_js::Buffer::new().format(*n).encode_utf16())
            } else {
                o.extend_from_slice(&[110, 117, 108, 108])
            }
        }
        Json::Str(s) => quote_into(o, s),
        Json::Arr(a) => {
            o.push(91);
            for (i, value) in a.iter().enumerate() {
                if i > 0 {
                    o.push(44);
                }
                json_into(o, value);
            }
            o.push(93);
        }
        Json::Obj(a) => {
            let mut order: Vec<_> = (0..a.len()).collect();
            let key = |i: usize| -> Option<u32> {
                let s = ascii(&a[i].0).ok()?;
                let n: u32 = s.parse().ok()?;
                if n != u32::MAX && n.to_string() == s {
                    Some(n)
                } else {
                    None
                }
            };
            order.sort_by_key(|&i| key(i).map(|n| (0, n as usize)).unwrap_or((1, i)));
            o.push(123);
            for (n, i) in order.into_iter().enumerate() {
                if n > 0 {
                    o.push(44);
                }
                quote_into(o, &a[i].0);
                o.push(58);
                json_into(o, &a[i].1);
            }
            o.push(125);
        }
    }
}
fn array(s: &[u16]) -> Vec<Json> {
    match json_parse(s) {
        Some(Json::Arr(a)) => a,
        _ => split(s, &[167]).into_iter().map(Json::Str).collect(),
    }
}
fn to_string(v: &Json) -> Units {
    match v {
        Json::Str(s) => s.clone(),
        Json::Null => units("null"),
        Json::Bool(v) => units(if *v { "true" } else { "false" }),
        Json::Num(n) => number_string(*n),
        Json::Obj(_) => units("[object Object]"),
        Json::Arr(a) => array_join(a, &[44]),
    }
}
fn array_join(a: &[Json], sep: &[u16]) -> Units {
    join(
        &a.iter()
            .map(|v| {
                if matches!(v, Json::Null) {
                    vec![]
                } else {
                    to_string(v)
                }
            })
            .collect::<Vec<_>>(),
        sep,
    )
}
fn make_array(a: Vec<Units>) -> Units {
    array_ops::make_array(a.into_iter().map(Json::Str).collect())
}
fn variable(map: &HashMap<Units, Units>, key: &[u16]) -> Units {
    map.get(key).cloned().unwrap_or_else(|| units("null"))
}
fn when(value: &[u16], kind: u8, c: &Context, effects: &mut Option<compat::State<'_>>, budget: &mut Budget) -> Result<(bool, bool), String> {
    if kind == 2 {
        return Ok((truth(&split(value, &[32])[0]), false));
    }
    let mut a = split(value, &units("::"));
    let mut keep = false;
    while a.len() > 1 {
        let b = a.pop().unwrap();
        let op = ascii(&a.pop().unwrap())?;
        let yes = match op.as_str() {
            "keep" => {
                keep = true;
                a.push(b);
                continue;
            }
            "not" => !truth(&b),
            "var" => {
                if [
                    "__proto__",
                    "constructor",
                    "toString",
                    "valueOf",
                    "toLocaleString",
                    "hasOwnProperty",
                    "isPrototypeOf",
                    "propertyIsEnumerable",
                    "__defineGetter__",
                    "__defineSetter__",
                    "__lookupGetter__",
                    "__lookupSetter__",
                ]
                .iter()
                .any(|s| b == units(s))
                {
                    return Err("variable-prototype-key".into());
                }
                if let Some(state) = effects {
                    truth(&state.read(Some(&b), false, budget)?.expect("conditional reads propagate errors"))
                } else {
                    truth(&variable(&c.variables, &b))
                }
            }
            "toggle" => {
                if let Some(state) = effects { state.untracked = true; }
                let mut key = units("toggle_");
                key.extend(b);
                truth(&variable(&c.globals, &key))
            }
            _ => {
                let left = a.pop().ok_or("when-args")?;
                match op.as_str() {
                    "and" => truth(&left) && truth(&b),
                    "or" => truth(&left) || truth(&b),
                    "is" => left == b,
                    "isnot" => left != b,
                    ">" => float(&left) > float(&b),
                    "<" => float(&left) < float(&b),
                    ">=" => float(&left) >= float(&b),
                    "<=" => float(&left) <= float(&b),
                    _ => return Err("when-mode".into()),
                }
            }
        };
        a.push(units(if yes { "1" } else { "0" }))
    }
    Ok((truth(&a[0]), keep))
}
fn block_output(body: &[u16], yes: bool, keep: bool) -> Units {
    let mut lines = split(body, &[10]);
    if lines.len() == 1 {
        let token = units("{{:else}}");
        if let Some(at) = body.windows(token.len()).position(|w| w == token) {
            return if yes {
                body[..at].to_vec()
            } else {
                body[at + token.len()..].to_vec()
            };
        }
        return if yes { body.to_vec() } else { vec![] };
    }
    if let Some(at) = lines.iter().position(|s| trim(s) == units("{{:else}}")) {
        if yes {
            lines.truncate(at)
        } else {
            lines.drain(..at + 1);
        }
    } else if !yes {
        return vec![];
    }
    if !keep {
        while lines.first().is_some_and(|s| trim(s).is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|s| trim(s).is_empty()) {
            lines.pop();
        }
    }
    join(&lines, &[10])
}
pub fn evaluate(nodes: &[Node], c: &mut Context, b: &mut Budget) -> Result<Units, String> {
    evaluate_inner(nodes, c, b, &mut None)
}
fn evaluate_inner(nodes: &[Node], c: &mut Context, b: &mut Budget, effects: &mut Option<compat::State<'_>>) -> Result<Units, String> {
    let mut out = Vec::new();
    for node in nodes {
        b.tick()?;
        match node {
            Node::Text(v) => out.extend(v),
            Node::Block {
                kind,
                condition,
                body,
            } => {
                let v = evaluate_inner(condition, c, b, effects)?;
                if *kind < 2 {
                    if truth(&split(&v, &[32])[0]) {
                        let body = evaluate_inner(body, c, b, effects)?;
                        if *kind == 0 {
                            out.extend(body)
                        } else {
                            let lines = split(trim(&body), &[10])
                                .into_iter()
                                .map(|s| {
                                    let n = s.iter().position(|&c| !ws(c)).unwrap_or(s.len());
                                    s[n..].to_vec()
                                })
                                .collect::<Vec<_>>();
                            out.extend(join(&lines, &[10]));
                        }
                    }
                } else {
                    let (yes, keep) = when(&v, *kind, c, effects, b)?;
                    let body = evaluate_inner(body, c, b, effects)?;
                    out.extend(block_output(&body, yes, keep))
                }
            }
            Node::Macro(nodes) => {
                let value = evaluate_inner(nodes, c, b, effects)?;
                let output_limit = b.max_output.saturating_sub(out.len());
                let result = evaluate_callback(&value, c, b, effects, output_limit)?;
                out.extend(result);
                if value.starts_with(&units("? ")) { continue; }
            }
        }
        if out.len() > b.max_output {
            return Err("output-limit".into());
        }
    }
    b.tick()?;
    Ok(out)
}

fn evaluate_callback(value: &[u16], c: &mut Context, b: &mut Budget, effects: &mut Option<compat::State<'_>>, output_limit: usize) -> Result<Units, String> {
    if value.first().is_some_and(|c| [35, 47, 58].contains(c)) {
        return Err("dynamic-block".into());
    }
    if value.starts_with(&units("? ")) {
        let s = ascii(&value[2..])?;
        return Ok(units(&super::arithmetic::evaluate(&s)?));
    }
    let first_colon = value.iter().position(|&c| c == 58);
    let sep = if first_colon.is_some_and(|i| value.get(i + 1) == Some(&58)) {
        units("::")
    } else {
        units(":")
    };
    let parts = split(&value, &sep);
    let name_units: Units = parts[0]
        .iter()
        .copied()
        .filter(|&c| !ws(c) && c != 95 && c != 45)
        .collect();
    let name = ascii(&name_units)?.to_ascii_lowercase();
    let a = &parts[1..];
    let undefined = units("undefined");
    let get = |i: usize| a.get(i).unwrap_or(&undefined);
    let bit = |v: bool| units(if v { "1" } else { "0" });
    if matches!(
        name.as_str(),
        "join" | "spread" | "arraylength" | "arrayelement" | "random"
            | "arrayshift" | "arraypop" | "arraypush" | "arraysplice"
    ) && a.first().is_some_and(|s| json_depth_exceeds(s, 64))
    {
        return Err("json-depth".into());
    }
    if matches!(name.as_str(), "getvar" | "getglobalvar")
        && [
            "__proto__",
            "constructor",
            "toString",
            "valueOf",
            "toLocaleString",
            "hasOwnProperty",
            "isPrototypeOf",
            "propertyIsEnumerable",
            "__defineGetter__",
            "__defineSetter__",
            "__lookupGetter__",
            "__lookupSetter__",
        ]
        .iter()
        .any(|s| get(0) == &units(s))
    {
        return Err("variable-prototype-key".into());
    }
    let preserve = || {
        let mut s = units("{{");
        s.extend(value);
        s.extend(units("}}"));
        s
    };
    let needs_first = matches!(
        name.as_str(),
        "length"
            | "trim"
            | "startswith"
            | "endswith"
            | "contains"
            | "split"
            | "join"
            | "arraylength"
            | "arrayelement"
            | "arrayshift"
            | "arraypop"
            | "arraypush"
            | "arraysplice"
            | "replace"
            | "spread"
            | "hash"
            | "tonumber"
            | "unicodeencode"
            | "calc"
    );
    let handled = effects.as_mut().and_then(|state| state.callback(&name, a, b));
    let result = if let Some(result) = handled {
        result?.unwrap_or_else(preserve)
    } else if needs_first && a.is_empty() {
        preserve()
    } else {
        match name.as_str() {
            "getvar" => variable(&c.variables, get(0)),
            "getglobalvar" => {
                if let Some(state) = effects { state.untracked = true; }
                variable(&c.globals, get(0))
            }
            "chatindex" => units(&c.index.to_string()),
            "user" => c.user.clone(),
            "tempvar" | "gettempvar" | "settempvar" => {
                if [
                    "__proto__",
                    "constructor",
                    "prototype",
                    "__force_return__",
                    "__return__",
                    "__defineGetter__",
                    "__defineSetter__",
                    "__lookupGetter__",
                    "__lookupSetter__",
                    "hasOwnProperty",
                    "isPrototypeOf",
                    "propertyIsEnumerable",
                    "toLocaleString",
                    "toString",
                    "valueOf",
                ]
                .iter()
                .any(|s| get(0) == &units(s))
                {
                    return Err("temp-reserved-key".into());
                }
                if name == "settempvar" {
                    if let Some(v) = a.get(1) {
                        c.temp.insert(get(0).clone(), v.clone());
                    } else {
                        c.temp.remove(get(0));
                    }
                    vec![]
                } else {
                    c.temp.get(get(0)).cloned().unwrap_or_default()
                }
            }
            "equal" => bit(a.first() == a.get(1)),
            "notequal" => bit(a.first() != a.get(1)),
            "greater" => bit(number(a.first())? > number(a.get(1))?),
            "less" => bit(number(a.first())? < number(a.get(1))?),
            "greaterequal" => bit(number(a.first())? >= number(a.get(1))?),
            "lessequal" => bit(number(a.first())? <= number(a.get(1))?),
            "and" => bit(get(0) == &units("1") && get(1) == &units("1")),
            "or" => bit(get(0) == &units("1") || get(1) == &units("1")),
            "not" => bit(get(0) != &units("1")),
            "length" => units(&get(0).len().to_string()),
            "trim" => trim(get(0)).to_vec(),
            "contains" => {
                bit(get(1).is_empty()
                    || get(0).windows(get(1).len()).any(|s| s == get(1)))
            }
            "startswith" => bit(get(0).starts_with(get(1))),
            "endswith" => bit(get(0).ends_with(get(1))),
            "split" => make_array(if a.len() < 2 {
                vec![get(0).clone()]
            } else {
                split(get(0), get(1))
            }),
            "array" | "makearray" => make_array(a.to_vec()),
            "arrayshift" | "arraypop" | "arraypush" | "arraysplice" => {
                array_ops::evaluate(&name, a)?
            }
            "join" | "spread" => array_join(
                &array(get(0)),
                if name == "spread" {
                    &[58, 58]
                } else if a.len() < 2 {
                    &[44]
                } else {
                    get(1)
                },
            ),
            "arraylength" => units(&array(get(0)).len().to_string()),
            "arrayelement" => {
                let ar = array(get(0));
                let n = number(a.get(1))?;
                let n = if n.is_nan() { 0.0 } else { n.trunc() };
                let at = if n < 0.0 { ar.len() as f64 + n } else { n };
                if at < 0.0 || at >= ar.len() as f64 {
                    units("null")
                } else {
                    match &ar[at as usize] {
                        Json::Null => units("null"),
                        v @ Json::Arr(_) | v @ Json::Obj(_) => json_string(v),
                        v => to_string(v),
                    }
                }
            }
            "calc" => units(&super::arithmetic::evaluate(&ascii(get(0))?)?),
            "random" => {
                let rand = if c.choices.is_none() {
                    c.state = c.state.wrapping_mul(1664525).wrapping_add(1013904223);
                    c.draws += 1;
                    Some(c.state as f64 / 4294967296.0)
                } else {
                    None
                };
                if a.is_empty() {
                    number_string(rand.ok_or("rng-continuous-domain")?)
                } else {
                    let ar = if a.len() > 1 {
                        a.iter().cloned().map(Json::Str).collect::<Vec<_>>()
                    } else if get(0).starts_with(&[91]) && get(0).ends_with(&[93]) {
                        array(get(0))
                    } else {
                        let v = replace_literal(get(0), &units("\\,"), &units("§X"));
                        let v = replace_literal(&v, &[58], &[44]);
                        split(&v, &[44]).into_iter().map(Json::Str).collect()
                    };
                    let at = match rand {
                        Some(r) => (r * ar.len() as f64) as usize,
                        None => finite_choice(c, ar.len())?,
                    };
                    match ar.get(at) {
                        Some(Json::Str(s)) => replace_literal(s, &units("§X"), &[44]),
                        Some(v) => json_string(v),
                        None => vec![],
                    }
                }
            }
            "randint" | "dice" | "roll" => finite_rng(&name, a, c)?,
            "min" | "max" | "sum" | "average" => {
                aggregates::evaluate(&name, a, b)?.unwrap_or_else(preserve)
            }
            "hash" => deterministic_hash::evaluate(get(0), b)?,
            "round" | "floor" | "ceil" | "abs" | "remaind" | "tonumber"
            | "fixnum" | "fixnumber" | "unicodeencode" | "unicodedecode"
            | "u" | "ue" | "unicodedecodefromhex" | "unicodeencodefromhex" => {
                scalar::evaluate(&name, a, b)?.unwrap_or_else(preserve)
            }
            "blank" => vec![],
            "br" => vec![10],
            "replace" => replace_all(
                get(0), get(1), get(2), output_limit, b, None,
            )?,
            _ => return Err("unsupported-operator".into()),
        }
    };
    Ok(result)
}

pub fn retained_bytes(nodes: &Vec<Node>) -> usize {
    nodes.capacity() * std::mem::size_of::<Node>()
        + nodes
            .iter()
            .map(|n| match n {
                Node::Text(s) => s.capacity() * 2,
                Node::Macro(v) => retained_bytes(v),
                Node::Block {
                    condition, body, ..
                } => retained_bytes(condition) + retained_bytes(body),
            })
            .sum::<usize>()
}

pub fn fold_constants(nodes: &mut Vec<Node>) -> usize {
    let mut folded = 0;
    for node in nodes {
        match node {
            Node::Macro(children) => folded += fold_constants(children),
            Node::Block {
                condition, body, ..
            } => {
                folded += fold_constants(condition);
                folded += fold_constants(body);
            }
            Node::Text(_) => {}
        }
        let Node::Macro(children) = node else {
            continue;
        };
        let mut expression = Vec::new();
        if !children.iter().all(|n| matches!(n, Node::Text(_))) {
            continue;
        }
        for n in children.iter() {
            if let Node::Text(s) = n {
                expression.extend(s)
            }
        }
        let end = expression
            .iter()
            .position(|&c| c == 58)
            .unwrap_or(expression.len());
        let name: Units = expression[..end]
            .iter()
            .copied()
            .filter(|&c| !ws(c) && c != 95 && c != 45)
            .collect();
        let name = ascii(&name).unwrap_or_default().to_ascii_lowercase();
        if !expression.starts_with(&units("? "))
            && !matches!(
                name.as_str(),
                "calc"
                    | "equal"
                    | "notequal"
                    | "greater"
                    | "less"
                    | "greaterequal"
                    | "lessequal"
                    | "and"
                    | "or"
                    | "not"
                    | "length"
                    | "trim"
                    | "contains"
                    | "startswith"
                    | "endswith"
                    | "replace"
                    | "split"
                    | "array"
                    | "makearray"
                    | "join"
                    | "spread"
                    | "arraylength"
                    | "arrayelement"
                    | "arrayshift"
                    | "arraypop"
                    | "arraypush"
                    | "arraysplice"
                    | "min" | "max" | "sum" | "average"
                    | "hash"
                    | "round"
                    | "floor"
                    | "ceil"
                    | "abs"
                    | "remaind"
                    | "tonumber"
                    | "fixnum"
                    | "fixnumber"
                    | "unicodeencode"
                    | "unicodedecode"
                    | "u"
                    | "ue"
                    | "unicodedecodefromhex"
                    | "unicodeencodefromhex"
                    | "blank"
                    | "br"
            )
        {
            continue;
        }
        let mut context = Context::default();
        let mut budget = Budget::default();
        if let Ok(value) = evaluate(std::slice::from_ref(node), &mut context, &mut budget) {
            *node = Node::Text(value);
            folded += 1;
        }
    }
    folded
}

fn json_depth_exceeds(s: &[u16], limit: usize) -> bool {
    let mut depth = 0usize;
    let mut string = false;
    let mut escaped = false;
    for &c in s {
        if string {
            if escaped {
                escaped = false
            } else if c == 92 {
                escaped = true
            } else if c == 34 {
                string = false
            }
            continue;
        }
        match c {
            34 => string = true,
            91 | 123 => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            93 | 125 => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}
