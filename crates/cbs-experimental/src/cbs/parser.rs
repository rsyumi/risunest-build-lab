// Stream port of risuChatParser (src/ts/parser/parser.svelte.ts) over UTF-16 code units: the same
// pointer walk, nested text stack, 512-entry stack-type table, block matchers and call stack.
use super::families::{self, ChatVar, Env, Out, Temp};
use super::{calc, replace_all, replace_literal, split, text, trim, units, Budget, Context, Units};
use risunest_ir::rng::Rng;
use std::borrow::Cow;
use std::collections::HashMap;

// stackType is a Uint8Array(512) indexed by nested.length; writes past it are dropped.
const STACK_TYPES: usize = 512;
const CALL_STACK_LIMIT: u32 = 20;
const MAX_FOLD_UNITS: usize = 65_536;
const CALL_STACK_ERROR: &str = "ERROR: Call stack limit reached";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    // matcherArg.displaying (risuChatParser's visualize argument).
    pub displaying: bool,
    // The default locale's language is az, el, lt or tr, whose toLocaleLowerCase and
    // toLocaleUpperCase rules differ from the root mapping the engine implements.
    pub tailored_case: bool,
    // The Unicode tables of the JS engine's toLowerCase and toUpperCase.
    pub case_tables: CaseTables,
    // matcherArg.tokenizeAccurate: random takes its first choice.
    pub tokenize_accurate: bool,
}

/// The Unicode tables of the JS engine's `toLowerCase` and `toUpperCase`, as the host probes them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseTables {
    /// Not probed, or a version this crate does not reproduce: `lower`, `upper` and `capitalize` decline as
    /// `case-table` for text they would map beyond ASCII.
    #[default]
    Foreign,
    /// Unicode 16, as in Node 24.12 (ICU 77.1): Rust's tables, except the code points where they differ, which
    /// decline as `case-table`.
    Unicode16,
    /// Rust's own tables (Unicode 17.0 with the current toolchain), for an engine known to share them: the Chromium the
    /// `benchmarks/rust-cbs` fixtures record, and a Unicode 17 engine such as the WebView2 one the `case-table-webview`
    /// fixture records.
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Ignore,
    Parse,
    IfPure,
    Pure,
    Each,
    Function,
    PureDisplay,
    Normalize,
    Escape,
    NewIf,
    NewIfFalsy,
}
impl Kind {
    fn pure(self) -> bool {
        matches!(self, Kind::Ignore | Kind::Pure | Kind::Each | Kind::Function | Kind::PureDisplay | Kind::Escape)
    }
}
#[derive(Clone, Debug)]
struct Block {
    kind: Kind,
    // #each source after the optional ::keep and as prefixes.
    each: Units,
    // type2 'keep' for #when, mode 'keep' for #each and #escape.
    keep: bool,
    func_args: Vec<Units>,
}
impl Block {
    fn new(kind: Kind) -> Self {
        Self { kind, each: Vec::new(), keep: false, func_args: Vec::new() }
    }
}
struct Level {
    text: Units,
    // stackType entry: 1 macro, 5 block, 6 nested block start inside pure mode, 0 unset.
    kind: u8,
    block: Option<Block>,
}

#[derive(Clone, Debug)]
struct Fold {
    // Distance of the `{{` from the end of the text, which #each injection before it keeps.
    distance: usize,
    length: usize,
    value: Units,
}

// A parse prepared once and evaluated against many contexts.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    source: Units,
    folds: Vec<Fold>,
    options: Options,
}
impl Prepared {
    pub fn folded(&self) -> usize {
        self.folds.len()
    }
    pub fn retained_bytes(&self) -> usize {
        self.source.capacity() * 2
            + self.folds.capacity() * std::mem::size_of::<Fold>()
            + self.folds.iter().map(|f| f.value.capacity() * 2).sum::<usize>()
    }
}

fn starts(s: &[u16], prefix: &str) -> bool {
    prefix.encode_utf16().enumerate().all(|(i, c)| s.get(i) == Some(&c))
}
fn equals(s: &[u16], other: &str) -> bool {
    s.iter().copied().eq(other.encode_utf16())
}
fn ascii_ci(s: &[u16], word: &str) -> bool {
    s.len() == word.len() && s.iter().zip(word.bytes()).all(|(&c, w)| c < 128 && (c as u8).to_ascii_lowercase() == w)
}
// An ASCII word of up to eight units; any other text is "", which names no operator.
fn ascii_word<'b>(s: &[u16], buffer: &'b mut [u8; 8]) -> &'b str {
    if s.len() > buffer.len() || s.iter().any(|&c| c > 127) {
        return "";
    }
    for (b, &c) in buffer.iter_mut().zip(s) {
        *b = c as u8;
    }
    std::str::from_utf8(&buffer[..s.len()]).unwrap_or("")
}
const ELSE_MACRO: &[u16] = &[123, 123, 58, 101, 108, 115, 101, 125, 125];
fn macro_text(dat: &[u16]) -> Units {
    [&[123, 123][..], dat, &[125, 125]].concat()
}

// /\{[{#]|<(?:user|char|bot)>/i
pub fn has_cbs_syntax(s: &[u16]) -> bool {
    (0..s.len()).any(|i| {
        (s[i] == 123 && matches!(s.get(i + 1), Some(123 | 35))) || (s[i] == 60 && angle_word(s, i).is_some())
    })
}
fn angle_word(s: &[u16], at: usize) -> Option<usize> {
    for word in ["user", "char", "bot"] {
        let end = at + 1 + word.len();
        if s.get(end) == Some(&62) && ascii_ci(&s[at + 1..end], word) {
            return Some(word.len());
        }
    }
    None
}
// da.replace(/\<(user|char|bot)\>/gi, '{{$1}}')
fn replace_angle(s: &[u16]) -> Units {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == 60 {
            if let Some(n) = angle_word(s, i) {
                out.extend_from_slice(&[123, 123]);
                out.extend_from_slice(&s[i + 1..i + 1 + n]);
                out.extend_from_slice(&[125, 125]);
                i += n + 2;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn split_limit2(s: &[u16]) -> Vec<Units> {
    split(s, &[32]).into_iter().take(2).collect()
}
fn truthy_state(s: Option<&Units>) -> bool {
    s.is_some_and(|s| equals(s, "true") || equals(s, "1"))
}
fn trim_lines(s: &[u16]) -> Units {
    let lines: Vec<Units> = split(s, &[10]).iter().map(|l| text::trim_start(l).to_vec()).collect();
    trim(&lines.join(&10u16)).to_vec()
}
// #code: \uXXXX first, then one-character backslash escapes ('.' excludes line terminators).
fn normalize(s: &[u16]) -> Units {
    let s: Units = trim(s).iter().copied().filter(|&c| c != 10 && c != 9).collect();
    let hex = |c: u16| c < 128 && (c as u8).is_ascii_hexdigit();
    let mut first = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == 92 && s.get(i + 1) == Some(&117) && s.len() >= i + 6 && s[i + 2..i + 6].iter().all(|&c| hex(c)) {
            let digits: String = s[i + 2..i + 6].iter().map(|&c| c as u8 as char).collect();
            first.push(u16::from_str_radix(&digits, 16).unwrap_or(0));
            i += 6;
        } else {
            first.push(s[i]);
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(first.len());
    let mut i = 0;
    while i < first.len() {
        if first[i] == 92 && first.get(i + 1).is_some_and(|&c| !matches!(c, 10 | 13 | 0x2028 | 0x2029)) {
            let c = first[i + 1];
            out.push(match c {
                110 => 10,
                114 => 13,
                116 => 9,
                98 => 8,
                102 => 12,
                118 => 11,
                120 => 0,
                _ => c,
            });
            i += 2;
        } else {
            out.push(first[i]);
            i += 1;
        }
    }
    out
}
fn risu_escape(s: &[u16]) -> Units {
    s.iter()
        .map(|&c| match c {
            123 => 0xe9b8,
            125 => 0xe9b9,
            40 => 0xe9ba,
            41 => 0xe9bb,
            c => c,
        })
        .collect()
}
// legacyBlockMatcher for `{#...#}`.
fn legacy_block(p1: &[u16]) -> Option<Units> {
    let bn = p1.iter().position(|&c| c == 10)?;
    let statement = split_limit2(&p1[..bn]);
    if equals(&statement[0], "if") {
        if statement.get(1).is_some_and(|s| s.is_empty() || equals(s, "0") || equals(s, "-1")) {
            return Some(Vec::new());
        }
        return Some(trim(&p1[bn + 1..]).to_vec());
    }
    None
}
fn block_end(p1: &[u16], b: &Block) -> Units {
    let trimmed = trim(p1);
    match b.kind {
        Kind::Pure | Kind::PureDisplay | Kind::Function => trimmed.to_vec(),
        Kind::Parse => trim_lines(trimmed),
        Kind::Each => {
            if b.keep {
                p1.to_vec()
            } else {
                trim_lines(trimmed)
            }
        }
        Kind::IfPure => p1.to_vec(),
        Kind::NewIf | Kind::NewIfFalsy => {
            let yes = b.kind == Kind::NewIf;
            if !p1.contains(&10) {
                return match text::find(p1, ELSE_MACRO) {
                    Some(at) => if yes { p1[..at].to_vec() } else { p1[at + ELSE_MACRO.len()..].to_vec() },
                    None => if yes { p1.to_vec() } else { Vec::new() },
                };
            }
            let mut lines = split(p1, &[10]);
            let else_line = lines.iter().position(|l| equals(trim(l), "{{:else}}"));
            match else_line {
                Some(at) if yes => lines.truncate(at),
                Some(at) => {
                    lines.drain(..at + 1);
                }
                None if !yes => return Vec::new(),
                None => {}
            }
            if !b.keep {
                while lines.first().is_some_and(|l| trim(l).is_empty()) {
                    lines.remove(0);
                }
                while lines.last().is_some_and(|l| trim(l).is_empty()) {
                    lines.pop();
                }
            }
            lines.join(&10u16)
        }
        Kind::Normalize => normalize(trimmed),
        Kind::Escape => risu_escape(if b.keep { p1 } else { trimmed }),
        Kind::Ignore => Vec::new(),
    }
}

// The arg fields risuChatParser reads and writes back: callStack counts every call on the arg object, and
// arg.functions (name to #func body) is set to the call's map once a `call::` runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ArgState {
    pub call_stack: u32,
    pub functions: Option<HashMap<Units, Units>>,
}

struct Engine<'a> {
    env: Env<'a>,
    functions: HashMap<Units, Units>,
    // arg.functions holds `functions`.
    bound: bool,
    call_stack: u32,
}
impl Engine<'_> {
    fn append(&mut self, out: &mut Units, value: &[u16]) -> Result<(), String> {
        if value.len() > self.env.budget.max_output.saturating_sub(out.len()) {
            return Err("output-limit".into());
        }
        out.extend_from_slice(value);
        Ok(())
    }
    // None for a non-string, which never passes `isTruthy` or `===` against the condition string.
    fn chat_var(&mut self, key: Option<&Units>) -> Result<Option<Units>, String> {
        Ok(match self.env.chat_var(key.map(Vec::as_slice))? {
            ChatVar::Text(value) => Some(value),
            ChatVar::Other { .. } => None,
        })
    }
    fn toggle(&self, key: Option<&Units>) -> Result<Units, String> {
        let mut name = units("toggle_");
        name.extend(key.cloned().unwrap_or_else(|| units("undefined")));
        families::global_var(self.env.context, &name)
    }
    // blockStartMatcher; None is {type:'nothing'}.
    fn block_start(&mut self, p1: &[u16]) -> Result<Option<Block>, String> {
        if starts(p1, "#if") || starts(p1, "#if_pure ") {
            let yes = truthy_state(split_limit2(p1).get(1));
            return Ok(Some(Block::new(match (yes, starts(p1, "#if_pure")) {
                (true, true) => Kind::IfPure,
                (true, false) => Kind::Parse,
                (false, _) => Kind::Ignore,
            })));
        }
        if starts(p1, "#when") {
            let flag = |yes: bool| Block::new(if yes { Kind::NewIf } else { Kind::NewIfFalsy });
            if starts(p1, "#when ") {
                return Ok(Some(flag(truthy_state(split_limit2(p1).get(1)))));
            }
            if !starts(p1, "#when::") {
                return Ok(Some(flag(false)));
            }
            let mut statement: Vec<Units> = split(p1, &[58, 58]);
            statement.remove(0);
            if statement.len() == 1 {
                return Ok(Some(flag(truthy_state(statement.first()))));
            }
            let mut mode = 0u8;
            let one = |v: bool| units(if v { "1" } else { "0" });
            while statement.len() > 1 {
                self.env.budget.tick()?;
                let condition = statement.pop().unwrap();
                let operator = statement.pop().unwrap();
                let truthy = |s: Option<&Units>| truthy_state(s);
                let float = |s: Option<&Units>| s.map_or(f64::NAN, |s| calc::parse_float(s));
                let mut word = [0u8; 8];
                let op = ascii_word(&operator, &mut word);
                let pushed = match op {
                    "not" => one(!truthy(Some(&condition))),
                    "keep" | "legacy" => {
                        mode = if op == "keep" { 1 } else { 2 };
                        condition
                    }
                    "and" => {
                        let other = statement.pop();
                        one(truthy(Some(&condition)) && truthy(other.as_ref()))
                    }
                    "or" => {
                        let other = statement.pop();
                        one(truthy(Some(&condition)) || truthy(other.as_ref()))
                    }
                    "is" => one(statement.pop().is_some_and(|o| o == condition)),
                    "isnot" => one(statement.pop().is_none_or(|o| o != condition)),
                    "var" => one(self.chat_var(Some(&condition))?.is_some_and(|value| truthy(Some(&value)))),
                    "toggle" => one(truthy(Some(&self.toggle(Some(&condition))?))),
                    "vis" | "visnot" => {
                        let other = statement.pop();
                        let equal = self.chat_var(other.as_ref())?.is_some_and(|value| value == condition);
                        one(equal == (op == "vis"))
                    }
                    "tis" | "tisnot" => {
                        let other = statement.pop();
                        let equal = self.toggle(other.as_ref())? == condition;
                        one(equal == (op == "tis"))
                    }
                    ">" | "<" | ">=" | "<=" => {
                        let other = float(statement.pop().as_ref());
                        let value = float(Some(&condition));
                        one(match op {
                            ">" => other > value,
                            "<" => other < value,
                            ">=" => other >= value,
                            _ => other <= value,
                        })
                    }
                    _ => one(truthy(Some(&condition))),
                };
                statement.push(pushed);
            }
            let yes = truthy_state(statement.first());
            let mut block = Block::new(match (mode, yes) {
                (2, true) => Kind::Parse,
                (2, false) => Kind::Ignore,
                (_, true) => Kind::NewIf,
                (_, false) => Kind::NewIfFalsy,
            });
            block.keep = mode == 1;
            return Ok(Some(block));
        }
        if equals(p1, "#pure") {
            return Ok(Some(Block::new(Kind::Pure)));
        }
        if equals(p1, "#pure_display") || equals(p1, "#puredisplay") {
            return Ok(Some(Block::new(Kind::PureDisplay)));
        }
        if equals(p1, "#code") {
            return Ok(Some(Block::new(Kind::Normalize)));
        }
        if starts(p1, "#escape") {
            let mut block = Block::new(Kind::Escape);
            block.keep = equals(trim(&p1[7..]), "::keep");
            return Ok(Some(block));
        }
        if starts(p1, "#each") {
            let mut block = Block::new(Kind::Each);
            let mut t2 = trim(&p1[5..]);
            if starts(t2, "::keep ") {
                block.keep = true;
                t2 = trim(&t2[7..]);
            }
            if starts(t2, "as ") {
                t2 = trim(&t2[3..]);
            }
            block.each = t2.to_vec();
            return Ok(Some(block));
        }
        if starts(p1, "#func") {
            let statement = split(p1, &[32]);
            if statement.len() > 1 {
                let mut block = Block::new(Kind::Function);
                block.func_args = statement[1..].to_vec();
                return Ok(Some(block));
            }
        }
        Ok(None)
    }
    // The #each close: expand the body per element and splice it into the text after the pointer.
    fn each(&mut self, body: &[u16], block: &Block) -> Result<Option<Units>, String> {
        let spec = &block.each;
        let (sub, array) = match text::rfind(spec, &units(" as ")) {
            Some(at) => (trim(&spec[at + 4..]).to_vec(), families::parse_array(&spec[..at])?),
            None => {
                let Some(at) = spec.iter().rposition(|&c| c == 32) else { return Ok(None) };
                (spec[at + 1..].to_vec(), families::parse_array(&spec[..at])?)
            }
        };
        let needle = [&units("{{slot::")[..], &sub, &[125, 125]].concat();
        let limit = self.env.budget.max_output;
        let mut added = Vec::new();
        for element in &array {
            self.env.budget.tick()?;
            let replacement = match element {
                super::Json::Str(s) => s.clone(),
                other => super::json_string(other),
            };
            let expanded = replace_all(body, &needle, &replacement, limit, self.env.budget, None)?;
            self.append(&mut added, &expanded)?;
        }
        Ok(Some(if block.keep { added } else { trim(&added).to_vec() }))
    }
    fn parse(&mut self, da: Cow<'_, [u16]>, folds: &[Fold], prepared: bool) -> Result<Units, String> {
        self.call_stack += 1;
        if self.call_stack > CALL_STACK_LIMIT {
            return Ok(units(CALL_STACK_ERROR));
        }
        if !has_cbs_syntax(&da) {
            return Ok(da.into_owned());
        }
        // matcherObj copies callStack now and starts without functions.
        let mut matcher_arg = ArgState { call_stack: self.call_stack, functions: None };
        let mut da = if prepared { da } else { Cow::Owned(replace_angle(&da)) };
        let mut levels: Vec<Level> = vec![Level { text: Vec::new(), kind: 0, block: None }];
        let mut pure = 0usize;
        let mut temp = Temp::default();
        let mut fold = 0usize;
        let mut p = 0usize;
        macro_rules! top {
            () => {
                levels.last_mut().unwrap()
            };
        }
        while p < da.len() {
            self.env.budget.tick()?;
            match da[p] {
                123 => {
                    let next = da.get(p + 1).copied();
                    if next != Some(123) && next != Some(35) {
                        self.append(&mut top!().text, &[123])?;
                    } else {
                        if next == Some(123) && pure == 0 && levels.len() + 1 < STACK_TYPES {
                            let distance = da.len() - p;
                            while folds.get(fold).is_some_and(|f| f.distance > distance) {
                                fold += 1;
                            }
                            if let Some(f) = folds.get(fold).filter(|f| f.distance == distance) {
                                fold += 1;
                                self.append(&mut top!().text, &f.value)?;
                                p += f.length;
                                continue;
                            }
                        }
                        p += 1;
                        let kind = if levels.len() + 1 < STACK_TYPES { 1 } else { 0 };
                        levels.push(Level { text: Vec::new(), kind, block: None });
                    }
                }
                35 => {
                    if da.get(p + 1) != Some(&125) || levels.len() == 1 || top!().kind != 1 {
                        self.append(&mut top!().text, &[35])?;
                    } else {
                        p += 1;
                        let dat = levels.pop().unwrap().text;
                        let value = legacy_block(&dat).unwrap_or_else(|| [&[123, 35][..], &dat, &[35, 125]].concat());
                        self.append(&mut top!().text, &value)?;
                    }
                }
                125 => {
                    if da.get(p + 1) != Some(&125) || levels.len() == 1 || top!().kind != 1 {
                        self.append(&mut top!().text, &[125])?;
                        p += 1;
                        continue;
                    }
                    p += 1;
                    let dat = levels.pop().unwrap().text;
                    if dat.first().is_some_and(|&c| c == 35 || c == 58) {
                        if pure > 0 {
                            self.append(&mut top!().text, &macro_text(&dat))?;
                            if !equals(&dat, ":else") {
                                let kind = if levels.len() + 1 < STACK_TYPES { 6 } else { 0 };
                                levels.push(Level { text: Vec::new(), kind, block: None });
                            }
                        } else {
                            match self.block_start(&dat)? {
                                None => self.append(&mut top!().text, &macro_text(&dat))?,
                                Some(block) => {
                                    if block.kind.pure() {
                                        pure += 1;
                                    }
                                    let kind = if levels.len() + 1 < STACK_TYPES { 5 } else { 0 };
                                    levels.push(Level { text: Vec::new(), kind, block: Some(block) });
                                }
                            }
                        }
                        p += 1;
                        continue;
                    }
                    if dat.first() == Some(&47) && dat.get(1) != Some(&47) {
                        if top!().kind == 5 {
                            let level = levels.pop().unwrap();
                            let block = level.block.expect("block levels carry their match");
                            if block.kind.pure() {
                                pure -= 1;
                            }
                            let result = block_end(&level.text, &block);
                            match block.kind {
                                Kind::Each => {
                                    if let Some(added) = self.each(&result, &block)? {
                                        if da.len() + added.len() > self.env.budget.max_output.saturating_mul(4) {
                                            return Err("output-limit".into());
                                        }
                                        // Folds before the injection point can no longer match; dropping them keeps
                                        // the injected text from meeting a fold at an equal distance.
                                        let remaining = da.len() - (p + 1);
                                        while folds.get(fold).is_some_and(|f| f.distance > remaining) {
                                            fold += 1;
                                        }
                                        da.to_mut().splice(p + 1..p + 1, added);
                                    }
                                }
                                Kind::Function => {
                                    self.functions.insert(block.func_args[0].clone(), result);
                                }
                                Kind::PureDisplay => {
                                    let shown = replace_literal(&result, &[123, 123], &units("\\{\\{"));
                                    let shown = replace_literal(&shown, &[125, 125], &units("\\}\\}"));
                                    self.append(&mut top!().text, &shown)?;
                                }
                                _ => self.append(&mut top!().text, &result)?,
                            }
                            p += 1;
                            continue;
                        }
                        if top!().kind == 6 {
                            let inner = levels.pop().unwrap().text;
                            self.append(&mut top!().text, &inner)?;
                            self.append(&mut top!().text, &macro_text(&dat))?;
                            p += 1;
                            continue;
                        }
                    }
                    if starts(&dat, "call::") {
                        if self.call_stack > CALL_STACK_LIMIT {
                            self.append(&mut top!().text, &units(CALL_STACK_ERROR))?;
                            p += 1;
                            continue;
                        }
                        let arguments = split(&dat, &[58, 58]).split_off(1);
                        if let Some(mut data) = self.functions.get(&arguments[0]).cloned() {
                            let limit = self.env.budget.max_output;
                            for (i, argument) in arguments.iter().enumerate() {
                                let token = units(&format!("{{{{arg::{i}}}}}"));
                                data = replace_all(&data, &token, argument, limit, self.env.budget, None)?;
                            }
                            self.bound = true;
                            let result = self.parse(Cow::Owned(data), &[], false)?;
                            self.append(&mut top!().text, &result)?;
                            p += 1;
                            continue;
                        }
                    }
                    let out = if pure > 0 {
                        Out::Preserve
                    } else {
                        let root = &mut levels.last_mut().unwrap().text;
                        families::matcher(&dat, &mut self.env, &mut temp, root, &mut matcher_arg)?
                    };
                    match out {
                        Out::Preserve => self.append(&mut top!().text, &macro_text(&dat))?,
                        Out::Text(value) => self.append(&mut top!().text, &value)?,
                        Out::Object(value) => {
                            self.append(&mut top!().text, &value)?;
                            if let Some(value) = temp.forced_return() {
                                return Ok(value);
                            }
                        }
                    }
                }
                _ => {
                    let end = da[p..].iter().position(|&c| c == 123 || c == 35 || c == 125).map_or(da.len(), |n| p + n);
                    self.append(&mut levels.last_mut().unwrap().text, &da[p..end])?;
                    p = end;
                    continue;
                }
            }
            p += 1;
        }
        let mut out = std::mem::take(&mut levels[0].text);
        for level in &levels[1..] {
            let prefix: &[u16] = if level.kind == 1 { &[123, 123] } else { &[60] };
            self.append(&mut out, prefix)?;
            self.append(&mut out, &level.text)?;
        }
        Ok(out)
    }
}

// A `{{X}}` whose X holds no brace, does not end in '#', and names a stateless callback.
fn fold_candidates(source: &[u16], options: Options) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut i = 0;
    while i + 1 < source.len() {
        if source[i] != 123 || source[i + 1] != 123 {
            i += 1;
            continue;
        }
        let start = i + 2;
        let Some(n) = source[start..].iter().position(|&c| c == 123 || c == 125) else { break };
        let end = start + n;
        if source[end] == 125 && source.get(end + 1) == Some(&125) {
            let dat = &source[start..end];
            let eligible = !dat.first().is_some_and(|&c| c == 35 || c == 58 || c == 47)
                && dat.last() != Some(&35)
                && !starts(dat, "call::")
                && families::foldable(dat);
            if eligible {
                let mut context = Context::default();
                let mut budget = Budget::default();
                let mut env = Env { context: &mut context, budget: &mut budget, options, rng: None, scope: None };
                let mut root = Vec::new();
                let value = match families::matcher(dat, &mut env, &mut Temp::default(), &mut root, &mut ArgState::default()) {
                    Ok(Out::Text(v)) => Some(v),
                    Ok(Out::Preserve) => Some(macro_text(dat)),
                    _ => None,
                };
                if let Some(value) = value.filter(|v| v.len() <= MAX_FOLD_UNITS) {
                    folds.push(Fold { distance: source.len() - i, length: end + 2 - i, value });
                }
            }
        }
        i += 1;
    }
    folds
}

pub fn prepare(text: &[u16], fold: bool, options: Options) -> Prepared {
    let source = replace_angle(text);
    let folds = if fold { fold_candidates(&source, options) } else { Vec::new() };
    Prepared { source, folds, options }
}

// risuChatParser(text, arg) with the snapshot in `context`; `functions` is arg.functions.
pub fn run(prepared: &Prepared, context: &mut Context, functions: &HashMap<Units, Units>, budget: &mut Budget) -> Result<Units, String> {
    run_with(prepared, context, budget, None, &mut ArgState { call_stack: 0, functions: Some(functions.clone()) })
}

// `run` with Math.random drawn from `rng` instead of the context's verification LCG, against the arg state
// before the call; on success `arg` is left as the call leaves it.
pub fn run_with<'a>(
    prepared: &Prepared,
    context: &'a mut Context,
    budget: &'a mut Budget,
    rng: Option<&'a mut dyn Rng>,
    arg: &mut ArgState,
) -> Result<Units, String> {
    run_scoped(prepared, context, budget, rng, arg, None)
}

pub(super) fn run_scoped<'a>(
    prepared: &Prepared,
    context: &'a mut Context,
    budget: &'a mut Budget,
    rng: Option<&'a mut dyn Rng>,
    arg: &mut ArgState,
    scope: Option<&'a mut dyn families::Scope>,
) -> Result<Units, String> {
    let env = Env { context, budget, options: prepared.options, rng, scope };
    let (bound, functions) = (arg.functions.is_some(), arg.functions.clone().unwrap_or_default());
    let mut engine = Engine { env, functions, bound, call_stack: arg.call_stack };
    let output = engine.parse(Cow::Borrowed(&prepared.source), &prepared.folds, true)?;
    arg.call_stack = engine.call_stack;
    if engine.bound {
        arg.functions = Some(engine.functions);
    }
    Ok(output)
}

// risuChatParser(text, matcherArg) from a callback, on the arg object `arg` describes. That object has no visualize
// field, so the call does not display; undefined text counts the call and then throws below the limit.
pub(super) fn reparse(env: &mut Env, arg: &mut ArgState, text: Option<&[u16]>) -> Result<Out, String> {
    let Some(text) = text else {
        arg.call_stack += 1;
        return Ok(if arg.call_stack > CALL_STACK_LIMIT { Out::Text(units(CALL_STACK_ERROR)) } else { Out::Preserve });
    };
    let options = Options { displaying: false, ..env.options };
    let rng: Option<&mut dyn Rng> = match &mut env.rng {
        Some(rng) => Some(&mut **rng),
        None => None,
    };
    let scope: Option<&mut dyn families::Scope> = match &mut env.scope {
        Some(scope) => Some(&mut **scope),
        None => None,
    };
    let env = Env { context: &mut *env.context, budget: &mut *env.budget, options, rng, scope };
    let (bound, functions) = (arg.functions.is_some(), arg.functions.clone().unwrap_or_default());
    let mut engine = Engine { env, functions, bound, call_stack: arg.call_stack };
    let output = engine.parse(Cow::Borrowed(text), &[], false);
    arg.call_stack = engine.call_stack;
    if engine.bound {
        arg.functions = Some(engine.functions);
    }
    match output {
        Ok(output) => Ok(Out::Text(output)),
        // The callback runs inside `matcher`'s `try`, which keeps the tag for anything the parse throws
        // (`parser.svelte.ts:1292-1316`). Limits, cancellation and declines are not TS throws and still end the call.
        Err(reason) if reason.starts_with("invalid:") => Ok(Out::Preserve),
        Err(reason) => Err(reason),
    }
}

pub fn parse(text: &[u16], context: &mut Context, functions: &HashMap<Units, Units>, options: Options, budget: &mut Budget) -> Result<Units, String> {
    run(&prepare(text, false, options), context, functions, budget)
}
