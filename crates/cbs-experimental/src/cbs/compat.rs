use super::{units, Budget, Context, Node, Units};
use std::collections::HashMap;
use std::mem::size_of;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Undefined,
    Text(Units),
}
impl Value {
    fn argument(value: Option<&Units>, budget: &mut Budget) -> Result<Self, String> {
        match value { Some(v) => Ok(Self::Text(copy_units(v, budget)?)), None => { budget.tick()?; Ok(Self::Undefined) } }
    }
    fn property_key(&self, budget: &mut Budget) -> Result<Units, String> {
        match self { Self::Undefined => { budget.tick()?; Ok(units("undefined")) }, Self::Text(v) => copy_units(v, budget) }
    }
    fn bytes(&self) -> usize {
        match self { Self::Undefined => 0, Self::Text(v) => v.capacity() * 2 }
    }
    fn resolved(&self, budget: &mut Budget) -> Result<Units, String> {
        match self { Self::Undefined => { budget.tick()?; Ok(units("null")) }, Self::Text(v) => copy_units(v, budget) }
    }
    fn copy(&self, budget: &mut Budget) -> Result<Self, String> {
        match self { Self::Undefined => { budget.tick()?; Ok(Self::Undefined) }, Self::Text(v) => Ok(Self::Text(copy_units(v, budget)?)) }
    }
    fn same(&self, other: &Self, budget: &mut Budget) -> Result<bool, String> {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => equal_units(a, b, budget),
            _ => { budget.tick()?; Ok(matches!((self, other), (Self::Undefined, Self::Undefined))) }
        }
    }
}

pub(super) fn copy_units(value: &[u16], budget: &mut Budget) -> Result<Units, String> {
    budget.tick()?;
    let mut copy = Vec::new();
    copy.try_reserve_exact(value.len()).map_err(|_| "effect-allocation")?;
    for chunk in value.chunks(256) {
        budget.tick()?;
        copy.extend_from_slice(chunk);
    }
    Ok(copy)
}
fn equal_units(left: &[u16], right: &[u16], budget: &mut Budget) -> Result<bool, String> {
    budget.tick()?;
    if left.len() != right.len() { return Ok(false); }
    for (left, right) in left.chunks(256).zip(right.chunks(256)) {
        budget.tick()?;
        if left != right { return Ok(false); }
    }
    Ok(true)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    Ordinary(Units),
    HistoryAccess(Units),
}
impl ReadError {
    pub fn name(&self) -> &'static str {
        match self { Self::Ordinary(_) => "Error", Self::HistoryAccess(_) => "CbsHistoryAccessError" }
    }
    pub fn message(&self) -> Units {
        match self {
            Self::Ordinary(message) => message.clone(),
            Self::HistoryAccess(access) => {
                let mut message = units("CBS history was not prepared: ");
                message.extend(access);
                message
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Value(Value),
    Error(ReadError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    MutableMap,
    SnapshotOnly,
}
#[derive(Clone, Debug)]
pub struct Limits {
    pub effects: usize,
    pub reads: usize,
    pub retained_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self { effects: 4096, reads: 16384, retained_bytes: 8 * 1024 * 1024 }
    }
}
#[derive(Clone)]
pub struct Input {
    pub binding: Binding,
    pub run_var: bool,
    pub rm_var: bool,
    pub variables: HashMap<Units, Cell>,
    pub context: Context,
    pub limits: Limits,
    pub history: Option<super::history::PreparedAbsoluteHistory>,
    pub program: Option<super::program::Limits>,
}
impl Default for Input {
    fn default() -> Self {
        Self {
            binding: Binding::MutableMap, run_var: false, rm_var: false,
            variables: HashMap::new(), context: Context::default(), limits: Limits::default(), history: None, program: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub sequence: usize,
    pub key: Value,
    pub value: Value,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub sequence: usize,
    pub key: Value,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Dependencies {
    pub keys: Vec<Value>,
    pub untracked: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Evaluation {
    pub result: Result<Units, ReadError>,
    pub effects: Vec<Write>,
    pub reads: Vec<Read>,
    pub dependencies: Dependencies,
    pub overlay: Vec<(Units, Value)>,
    pub draws: usize,
    pub rng_state: u32,
    pub history: Option<super::history::Observation>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Rejected(String),
    Interrupted(String),
    Evaluated(Evaluation),
}
#[derive(Clone, Debug)]
pub struct Program {
    nodes: Vec<Node>,
    history_enabled: bool,
    source: Option<Units>,
}
impl Program {
    pub fn fold_constants(&mut self) -> usize {
        if self.source.is_some() { 0 } else { super::fold_constants(&mut self.nodes) }
    }
}
pub fn compile(source: &[u16]) -> Result<Program, String> {
    super::compile_mode(source, true, false).map(|nodes| Program { nodes, history_enabled: false, source: None })
}
pub fn compile_with_history(source: &[u16]) -> Result<Program, String> {
    super::compile_mode(source, true, true).map(|nodes| Program { nodes, history_enabled: true, source: None })
}

pub fn compile_program(source: &[u16], history_enabled: bool) -> Result<Program, String> {
    super::source_domain(source)?;
    let mut owned = Vec::new();
    owned.try_reserve_exact(source.len()).map_err(|_| "program-allocation")?;
    owned.extend_from_slice(source);
    Ok(Program { nodes: Vec::new(), history_enabled, source: Some(owned) })
}
pub fn evaluate(program: &Program, input: &Input, budget: &mut Budget) -> Outcome {
    if program.source.is_some() != input.program.is_some() { return Outcome::Rejected("program-domain".into()); }
    if let (Some(source), Some(limits)) = (&program.source, &input.program) {
        if let Err(reason) = limits.admit(source) { return Outcome::Rejected(reason); }
    }
    if program.history_enabled != input.history.is_some() { return Outcome::Rejected("history-domain".into()); }
    if input.limits.effects == 0 || input.limits.reads == 0 || input.limits.retained_bytes == 0 {
        return Outcome::Rejected("invalid-effect-limits".into());
    }
    if !input.context.variables.is_empty() || input.context.choices.is_some() {
        return Outcome::Rejected("binding-domain".into());
    }
    if input.binding == Binding::SnapshotOnly {
        for cell in input.variables.values() {
            if let Err(reason) = budget.tick() { return Outcome::Interrupted(reason); }
            if matches!(cell, Cell::Error(_)) { return Outcome::Rejected("binding-domain".into()); }
        }
    }
    let context = (|| -> Result<Context, String> {
        let mut globals = HashMap::new();
        globals.try_reserve(input.context.globals.len()).map_err(|_| "effect-allocation")?;
        for (key, value) in &input.context.globals {
            globals.insert(copy_units(key, budget)?, copy_units(value, budget)?);
        }
        Ok(Context { globals, user: copy_units(&input.context.user, budget)?, index: input.context.index,
            state: input.context.state, ..Context::default() })
    })();
    let mut context = match context { Ok(context) => context, Err(reason) => return Outcome::Interrupted(reason) };
    let history = match input.history.as_ref().map(|capture| super::history::State::new(capture, input.context.index, budget)).transpose() {
        Ok(history) => history,
        Err(reason) if matches!(reason.as_str(), "cancelled" | "budget" | "effect-allocation") => return Outcome::Interrupted(reason),
        Err(reason) => return Outcome::Rejected(reason),
    };
    let mut state = Some(State::new(input, history));
    let result = if let (Some(source), Some(limits)) = (&program.source, &input.program) {
        super::program::evaluate(source, limits, program.history_enabled, &mut context, budget, &mut state)
    } else { super::evaluate_inner(&program.nodes, &mut context, budget, &mut state) };
    let mut state = state.unwrap();
    let result = match result {
        Ok(output) => Ok(output),
        Err(reason) if reason == "compat-read-error" => match state.failure.take() {
            Some(error) => Err(error),
            None => return Outcome::Rejected(reason),
        },
        Err(reason) if matches!(reason.as_str(), "cancelled" | "budget" | "output-limit"
            | "effect-limit" | "read-limit" | "retained-limit" | "effect-allocation"
            | "history-dependency-limit" | "history-retained-limit"
            | "program-retained-limit" | "program-frame-limit" | "program-function-limit"
            | "program-source-limit" | "program-expanded-limit" | "program-allocation") => {
            return Outcome::Interrupted(reason);
        }
        Err(reason) => return Outcome::Rejected(reason),
    };
    Outcome::Evaluated(Evaluation {
        result, effects: state.effects, reads: state.reads,
        dependencies: Dependencies { keys: state.keys, untracked: state.untracked },
        overlay: state.overlay, draws: context.draws, rng_state: context.state,
        history: state.history.map(|history| history.observation),
    })
}

pub(super) struct State<'a> {
    input: &'a Input,
    effects: Vec<Write>,
    reads: Vec<Read>,
    keys: Vec<Value>,
    overlay: Vec<(Units, Value)>,
    sequence: usize,
    payload_bytes: usize,
    failure: Option<ReadError>,
    history: Option<super::history::State<'a>>,
    pub(super) untracked: bool,
}
fn admitted_key(key: &[u16]) -> bool {
    !["__proto__", "constructor", "toString", "valueOf", "toLocaleString", "hasOwnProperty",
        "isPrototypeOf", "propertyIsEnumerable", "__defineGetter__", "__defineSetter__",
        "__lookupGetter__", "__lookupSetter__"].iter().any(|value| key == units(value))
}
impl<'a> State<'a> {
    fn new(input: &'a Input, history: Option<super::history::State<'a>>) -> Self {
        Self {
            input, effects: Vec::new(), reads: Vec::new(), keys: Vec::new(), overlay: Vec::new(),
            sequence: 0, payload_bytes: 0, failure: None, history, untracked: false,
        }
    }
    fn retained(&self, payload: usize) -> Result<(), String> {
        let bytes = payload.saturating_add(self.effects.capacity() * size_of::<Write>())
            .saturating_add(self.reads.capacity() * size_of::<Read>())
            .saturating_add(self.keys.capacity() * size_of::<Value>())
            .saturating_add(self.overlay.capacity() * size_of::<(Units, Value)>());
        if bytes > self.input.limits.retained_bytes { Err("retained-limit".into()) } else { Ok(()) }
    }
    fn overlay_index(&self, property: &[u16], budget: &mut Budget) -> Result<Option<usize>, String> {
        for (index, (key, _)) in self.overlay.iter().enumerate() {
            if equal_units(key, property, budget)? { return Ok(Some(index)); }
        }
        Ok(None)
    }
    pub(super) fn read(&mut self, argument: Option<&Units>, caught: bool, budget: &mut Budget) -> Result<Option<Units>, String> {
        let key = Value::argument(argument, budget)?;
        let property = key.property_key(budget)?;
        if !admitted_key(&property) { return Err("variable-prototype-key".into()); }
        if self.reads.len() >= self.input.limits.reads { return Err("read-limit".into()); }
        let mut new_key = true;
        for existing in &self.keys {
            if existing.same(&key, budget)? { new_key = false; break; }
        }
        let dependency = if new_key { Some(key.copy(budget)?) } else { None };
        let payload = self.payload_bytes.saturating_add(key.bytes())
            .saturating_add(dependency.as_ref().map_or(0, Value::bytes));
        self.retained(payload)?;
        self.reads.try_reserve(1).map_err(|_| "effect-allocation")?;
        if new_key { self.keys.try_reserve(1).map_err(|_| "effect-allocation")?; }
        self.retained(payload)?;
        self.reads.push(Read { sequence: self.sequence, key });
        if let Some(key) = dependency { self.keys.push(key); }
        self.sequence += 1;
        self.payload_bytes = payload;
        if let Some(index) = self.overlay_index(&property, budget)? {
            return Ok(Some(self.overlay[index].1.resolved(budget)?));
        }
        budget.tick()?;
        match self.input.variables.get(&property) {
            None => Ok(Some(units("null"))),
            Some(Cell::Value(value)) => Ok(Some(value.resolved(budget)?)),
            Some(Cell::Error(ReadError::Ordinary(_))) if caught => Ok(None),
            Some(Cell::Error(error)) => {
                self.failure = Some(match error {
                    ReadError::Ordinary(message) => ReadError::Ordinary(copy_units(message, budget)?),
                    ReadError::HistoryAccess(access) => ReadError::HistoryAccess(copy_units(access, budget)?),
                });
                Err("compat-read-error".into())
            }
        }
    }
    fn write(&mut self, key: Option<&Units>, value: Value, budget: &mut Budget) -> Result<(), String> {
        if self.input.binding == Binding::SnapshotOnly { return Ok(()); }
        let key = Value::argument(key, budget)?;
        let property = key.property_key(budget)?;
        if !admitted_key(&property) { return Err("variable-prototype-key".into()); }
        if self.effects.len() >= self.input.limits.effects { return Err("effect-limit".into()); }
        let existing = self.overlay_index(&property, budget)?;
        let overlay_value = value.copy(budget)?;
        let removed = existing.map_or(0, |at| self.overlay[at].1.bytes());
        let payload = self.payload_bytes.saturating_sub(removed).saturating_add(key.bytes())
            .saturating_add(value.bytes()).saturating_add(overlay_value.bytes())
            .saturating_add(if existing.is_none() { property.capacity() * 2 } else { 0 });
        self.retained(payload)?;
        self.effects.try_reserve(1).map_err(|_| "effect-allocation")?;
        if existing.is_none() { self.overlay.try_reserve(1).map_err(|_| "effect-allocation")?; }
        self.retained(payload)?;
        self.effects.push(Write { sequence: self.sequence, key, value });
        match existing {
            Some(at) => self.overlay[at].1 = overlay_value,
            None => self.overlay.push((property, overlay_value)),
        }
        self.sequence += 1;
        self.payload_bytes = payload;
        Ok(())
    }
    pub(super) fn callback(&mut self, name: &str, args: &[Units], budget: &mut Budget)
        -> Option<Result<Option<Units>, String>>
    {
        if matches!(name, "lastmessage" | "lastmessageid" | "lastmessageindex" | "previouschatlog" | "role") {
            let history = self.history.as_mut()?;
            return Some(history.callback(name, args, self.input.context.index, budget).and_then(|reply| match reply {
                super::history::Reply::Value(value) => Ok(Some(value)),
                super::history::Reply::Preserve => Ok(None),
                super::history::Reply::Missing(access) => {
                    self.failure = Some(ReadError::HistoryAccess(access));
                    Err("compat-read-error".into())
                }
            }));
        }
        if !matches!(name, "getvar" | "setvar" | "addvar" | "setdefaultvar") { return None; }
        Some(self.variable_callback(name, args, budget))
    }
    fn variable_callback(&mut self, name: &str, args: &[Units], budget: &mut Budget)
        -> Result<Option<Units>, String>
    {
        if name == "getvar" { return self.read(args.first(), true, budget); }
        if self.input.rm_var { return Ok(Some(Vec::new())); }
        if !self.input.run_var { return Ok(None); }
        match name {
            "setvar" => self.write(args.first(), Value::argument(args.get(1), budget)?, budget)?,
            "addvar" => {
                let Some(current) = self.read(args.first(), true, budget)? else { return Ok(None); };
                let left = super::scalar::number(Some(&current), Some(&mut *budget))?;
                let right = super::scalar::number(args.get(1), Some(&mut *budget))?;
                self.write(args.first(), Value::Text(super::number_string(left + right)), budget)?;
            }
            "setdefaultvar" => {
                let Some(current) = self.read(args.first(), true, budget)? else { return Ok(None); };
                if current.is_empty() || current == units("null") {
                    self.write(args.first(), Value::argument(args.get(1), budget)?, budget)?;
                }
            }
            _ => unreachable!(),
        }
        Ok(Some(Vec::new()))
    }
}
