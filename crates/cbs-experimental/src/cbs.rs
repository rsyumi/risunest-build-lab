use std::collections::HashMap;
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
    if sep.is_empty() {
        return s.iter().map(|&c| vec![c]).collect();
    }
    let mut start = 0;
    let mut out = Vec::new();
    let mut p = 0;
    while p + sep.len() <= s.len() {
        if &s[p..p + sep.len()] == sep {
            out.push(s[start..p].to_vec());
            p += sep.len();
            start = p
        } else {
            p += 1
        }
    }
    out.push(s[start..].to_vec());
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
    join(&split(s, from), to)
}
fn truth(s: &[u16]) -> bool {
    s == units("1") || s == units("true")
}
pub fn number_string(n: f64) -> Units {
    units(ryu_js::Buffer::new().format(n))
}
fn number(s: Option<&Units>) -> Result<f64, String> {
    let Some(s) = s else { return Ok(f64::NAN) };
    let s = trim(s);
    if s.is_empty() {
        return Ok(0.0);
    }
    let Ok(s) = ascii(s) else { return Ok(f64::NAN) };
    for (p, b) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(v) = s.strip_prefix(p) {
            if v.is_empty() || v.chars().any(|c| c.to_digit(b).is_none()) {
                return Ok(f64::NAN);
            }
            let n = u64::from_str_radix(v, b).map_err(|_| "number-radix-range")?;
            if n > 9_007_199_254_740_991 {
                return Err("number-radix-range".into());
            }
            return Ok(n as f64);
        }
    }
    match s.as_str() {
        "Infinity" | "+Infinity" => Ok(f64::INFINITY),
        "-Infinity" => Ok(f64::NEG_INFINITY),
        _ => {
            if s.bytes()
                .any(|c| !c.is_ascii_digit() && !b"+-.eE".contains(&c))
            {
                return Ok(f64::NAN);
            }
            Ok(s.parse().unwrap_or(f64::NAN))
        }
    }
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
fn lazy_body_admitted(nodes: &[Node]) -> bool {
    nodes.iter().all(|node| match node {
        Node::Text(_) => true,
        Node::Block {
            condition, body, ..
        } => lazy_body_admitted(condition) && lazy_body_admitted(body),
        Node::Macro(children) => {
            if !lazy_body_admitted(children) {
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
            matches!(
                name.to_ascii_lowercase().as_str(),
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
                    | "split"
                    | "makearray"
                    | "array"
                    | "join"
                    | "spread"
                    | "arraylength"
                    | "arrayelement"
                    | "calc"
                    | "random"
                    | "blank"
                    | "br"
            )
        }
    })
}
pub fn compile(text: &[u16]) -> Result<Vec<Node>, String> {
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
    fn seq(s: &[u16], p: &mut usize, end: u8, depth: usize) -> Result<Vec<Node>, String> {
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
                let mut v = seq(s, p, 1, depth + 1)?;
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
                    let body = seq(s, p, 2, depth + 1)?;
                    if kind < 2 && !lazy_body_admitted(&body) {
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
    seq(text, &mut 0, 0, 0)
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
    pub globals: HashMap<Units, Units>,
    pub temp: HashMap<Units, Units>,
    pub index: i64,
    pub user: Units,
    pub state: u32,
    pub draws: usize,
    pub choices: Option<ChoiceTape>,
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
    #[cfg(test)]
    pub progress: Arc<std::sync::atomic::AtomicUsize>,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            remaining: 1_000_000,
            max_output: 4_194_304,
            cancel: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            progress: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}
impl Budget {
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
        Ok(())
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
                    ("null", Json::Null),
                    ("true", Json::Bool(true)),
                    ("false", Json::Bool(false)),
                ] {
                    let w = units(word);
                    if s[*p..].starts_with(&w) {
                        *p += w.len();
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
fn quote(s: &[u16]) -> Units {
    let mut o = vec![34];
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            34 => o.extend(units("\\\"")),
            92 => o.extend(units("\\\\")),
            8 => o.extend(units("\\b")),
            12 => o.extend(units("\\f")),
            10 => o.extend(units("\\n")),
            13 => o.extend(units("\\r")),
            9 => o.extend(units("\\t")),
            0..=31 => o.extend(units(&format!("\\u{c:04x}"))),
            0xd800..=0xdbff if s.get(i + 1).is_some_and(|c| (0xdc00..=0xdfff).contains(c)) => {
                o.push(c);
                i += 1;
                o.push(s[i]);
            }
            0xd800..=0xdfff => o.extend(units(&format!("\\u{c:04x}"))),
            _ => o.push(c),
        }
        i += 1
    }
    o.push(34);
    o
}
fn json_string(v: &Json) -> Units {
    match v {
        Json::Null => units("null"),
        Json::Bool(b) => units(if *b { "true" } else { "false" }),
        Json::Num(n) => {
            if n.is_finite() {
                number_string(*n)
            } else {
                units("null")
            }
        }
        Json::Str(s) => quote(s),
        Json::Arr(a) => {
            let mut o = units("[");
            o.extend(join(
                &a.iter().map(json_string).collect::<Vec<_>>(),
                &units(","),
            ));
            o.push(93);
            o
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
            let pairs = order
                .into_iter()
                .map(|i| {
                    let mut p = quote(&a[i].0);
                    p.push(58);
                    p.extend(json_string(&a[i].1));
                    p
                })
                .collect::<Vec<_>>();
            let mut o = units("{");
            o.extend(join(&pairs, &units(",")));
            o.push(125);
            o
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
    json_string(&Json::Arr(
        a.into_iter()
            .map(|s| Json::Str(replace_literal(&s, &units("::"), &units("\\u003A\\u003A"))))
            .collect(),
    ))
}
fn variable(map: &HashMap<Units, Units>, key: &[u16]) -> Units {
    map.get(key).cloned().unwrap_or_else(|| units("null"))
}
fn when(value: &[u16], kind: u8, c: &Context) -> Result<(bool, bool), String> {
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
                truth(&variable(&c.variables, &b))
            }
            "toggle" => {
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
                let v = evaluate(condition, c, b)?;
                if *kind < 2 {
                    if truth(&split(&v, &[32])[0]) {
                        let body = evaluate(body, c, b)?;
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
                    let (yes, keep) = when(&v, *kind, c)?;
                    let body = evaluate(body, c, b)?;
                    out.extend(block_output(&body, yes, keep))
                }
            }
            Node::Macro(nodes) => {
                let value = evaluate(nodes, c, b)?;
                if value.first().is_some_and(|c| [35, 47, 58].contains(c)) {
                    return Err("dynamic-block".into());
                }
                if value.starts_with(&units("? ")) {
                    let s = ascii(&value[2..])?;
                    out.extend(units(&super::arithmetic::evaluate(&s)?));
                    continue;
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
                    s.extend(&value);
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
                        | "replace"
                        | "spread"
                        | "calc"
                );
                let result = if needs_first && a.is_empty() {
                    preserve()
                } else {
                    match name.as_str() {
                        "getvar" => variable(&c.variables, get(0)),
                        "getglobalvar" => variable(&c.globals, get(0)),
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
                        "blank" => vec![],
                        "br" => vec![10],
                        "replace" => return Err("replace-substitution".into()),
                        _ => return Err("unsupported-operator".into()),
                    }
                };
                out.extend(result);
            }
        }
        if out.len() > b.max_output {
            return Err("output-limit".into());
        }
    }
    b.tick()?;
    Ok(out)
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
                    | "split"
                    | "array"
                    | "makearray"
                    | "join"
                    | "spread"
                    | "arraylength"
                    | "arrayelement"
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
