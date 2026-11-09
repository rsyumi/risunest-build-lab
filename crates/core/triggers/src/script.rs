//! Admission: the triggers a run can reach, compiled from their stored objects, and every decline that stored data
//! alone decides.

use crate::input::TriggerInput;
use crate::js::{equals, unsupported};
use risunest_ir::budget::Budget;
use risunest_ir::chat::MessageRole;
use risunest_ir::num::to_number;
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use std::borrow::Cow;

/// Which triggers one `runTrigger` invocation runs besides those whose first effect is `triggercode` or `triggerlua`
/// (`canRunTrigger`, `src/ts/process/triggers.ts:1178-1181`).
#[derive(Debug)]
pub(crate) enum Selector {
    /// A truthy `manualName`: the triggers with that `comment`.
    Name(Text),
    /// No `manualName`: the triggers of this `type`.
    Mode(&'static str),
    /// A truthy `manualName` that is not a string, which no `comment` equals.
    Nothing,
}

impl Selector {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Name(left), Self::Name(right)) => left.equals(right),
            (Self::Mode(left), Self::Mode(right)) => left == right,
            (Self::Nothing, Self::Nothing) => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Trigger {
    comment: Option<Text>,
    kind: Option<Text>,
    any_mode: bool,
    pub low_level: bool,
    /// Compiled when some selection of the run reaches the trigger.
    pub body: Option<Body>,
}

impl Trigger {
    pub fn selected(&self, selector: &Selector) -> bool {
        self.any_mode || match selector {
            Selector::Name(name) => self.comment.as_ref().is_some_and(|comment| comment.equals(name)),
            Selector::Mode(mode) => self.kind.as_ref().is_some_and(|kind| equals(kind, mode)),
            Selector::Nothing => false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Body {
    pub conditions: Vec<Condition>,
    pub effects: Vec<Effect>,
}

#[derive(Debug)]
pub(crate) struct Effect {
    /// The `indent` property, `None` when the effect has none (`currentIndent = 0`).
    pub indent: Option<u32>,
    pub op: Op,
}

#[derive(Debug)]
pub(crate) enum Condition {
    /// `var`, `value` and `chatindex`.
    Compare { left: Left, operator: Option<Text>, value: Text },
    /// `exists`; `start` is `0 - condition.depth`, the `slice` start.
    Exists { value: Text, kind: Option<Text>, start: f64 },
    /// Any other type passes.
    Pass,
}

#[derive(Debug)]
pub(crate) enum Left { Var(Text), Literal(Text), ChatIndex }

/// `xType === 'value' ? parse(x) : getVar(parse(x))`, or its inverse where TS tests for `'var'`.
#[derive(Debug)]
pub(crate) enum Src { Value(Text), Var(Text) }

/// `varType`, which `v2SetDictVar` and `v2DeleteDictKey` test for both `'value'` and `'var'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VarType { Value, Var, Other }

#[derive(Debug, Clone, Copy)]
pub(crate) enum Location { Start, HistoryEnd, PromptEnd, Other }

#[derive(Debug)]
pub(crate) enum Op {
    Noop,
    /// `modulo` is false for v1 `setvar`, which has no `%=`.
    SetVar { var: Text, value: Src, operator: Option<Text>, modulo: bool },
    DeclareLocal { var: Text, value: Src, indent: u32 },
    If { source: Src, target: Src, condition: Option<Text>, indent: u32 },
    Else { indent: u32 },
    EndIndent { indent: u32, end_of_loop: bool },
    Loop,
    LoopTimes { value: Src },
    BreakLoop,
    RunTrigger { target: Selector },
    ConsoleLog { source: Src },
    Tokenize { value: Src, output: Text },
    /// `v2RunLLM` with low-level access: `model` is the `requestChatData` mode, `streaming` its `useStreaming`. `legacy`
    /// is v1 `runLLM` (`:1648-1674`): `output` is the variable name as written and a result other than a success
    /// stores `'Error: ' + result.result`.
    RunLlm { value: Src, model: Text, streaming: bool, output: Text, legacy: bool },
    /// `v2CheckSimilarity` with low-level access. `legacy` is v1 `checkSimilarity` (`:1676-1689`): `value` is parsed
    /// before `source`, and `output` is the variable name as written.
    CheckSimilarity { source: Src, value: Src, output: Text, legacy: bool },
    StopTrigger,
    /// `defaults`: `v2CutChat` reads a `NaN` start as 0 and a `NaN` end as the length; v1 `cutchat` slices with them.
    CutChat { start: Src, end: Src, defaults: bool },
    ModifyChat { index: Src, value: Src },
    SystemPrompt { location: Location, value: Src },
    Impersonate { role: Option<MessageRole>, value: Src },
    StopSending,
    SendAiPrompt,
    ExtractRegex { value: Src, regex: Src, flags: Src, result: Src, output: Text },
    /// v1 `extractRegex`: only `value` is parsed, and `output` is the variable name as written.
    LegacyExtract { value: Text, regex: Text, flags: Text, result: Text, output: Text },
    LastMessage { output: Text },
    MessageAt { index: Src, output: Text },
    MessageCount { output: Text },
    Random { min: Src, max: Src, output: Text },
    CharAt { source: Src, index: Src, output: Text },
    CharCount { source: Src, output: Text },
    Case { source: Src, upper: bool, output: Text },
    SetCharAt { source: Src, index: Src, value: Src, output: Text },
    Split { source: Src, delimiter: Src, output: Text },
    Join { var: Src, delimiter: Src, output: Text },
    MakeArray { var: Text },
    ArrayLength { var: Text, output: Text },
    ArrayGet { var: Text, index: Src, output: Text },
    ArrayPush { var: Text, value: Src, front: bool },
    ArrayTake { var: Text, output: Text, front: bool },
    ArraySplice { var: Text, start: Src, item: Src },
    ArraySlice { var: Text, start: Src, end: Src, output: Text },
    ArrayIndexOf { var: Text, value: Src, output: Text },
    ArrayRemove { var: Text, index: Src },
    ArraySet { var: Text, index: Src, value: Src },
    Concat { left: Src, right: Src, output: Text },
    LastByRole { role: &'static str, output: Text },
    FirstMessage { output: Text },
    QuickSearch { value: Src, depth: Src, condition: Option<Text>, output: Text },
    RegexTest { value: Src, regex: Src, flags: Src, output: Text },
    ReplaceString { source: Src, regex: Src, result: Src, replacement: Src, flags: Src, output: Text },
    Calculate { expression: Src, output: Text },
    GetNote { output: Text },
    SetNote { value: Src },
    ResetDict { var: Text },
    DictGet { var: Src, key: Src, output: Text },
    DictSet { var: Text, var_type: VarType, key: Src, value: Src },
    DictDelete { var: Text, var_type: VarType, key: Src },
    DictHas { var: Src, key: Src, output: Text },
    DictSize { var: Src, output: Text },
    DictKeys { var: Src, values: bool, output: Text },
}

#[derive(Debug)]
pub(crate) struct Program {
    pub triggers: Vec<Trigger>,
}

// Effects that need a host or a record this stage does not run yet. Each declines as `trigger-effect-<type>`.
const DECLINED: &[&str] = &[
    // Host callbacks, waits and display refreshes.
    "command", "triggerlua",
    "v2Command", "v2ShowAlert", "v2GetAlertInput", "v2GetAlertSelect", "v2Wait", "v2UpdateGUI", "v2UpdateChatAt",
    // Character, persona and lorebook records.
    "v2ModifyLorebook", "v2GetLorebook", "v2GetLorebookCount", "v2GetLorebookEntry", "v2SetLorebookActivation",
    "v2GetLorebookIndexViaName", "v2GetAllLorebooks", "v2GetLorebookByName", "v2GetLorebookByIndex", "v2CreateLorebook",
    "v2ModifyLorebookByIndex", "v2DeleteLorebookByIndex", "v2GetLorebookCountNew", "v2SetLorebookAlwaysActive",
    "v2GetCharacterDesc", "v2SetCharacterDesc", "v2GetPersonaDesc", "v2SetPersonaDesc", "v2GetReplaceGlobalNote",
    "v2SetReplaceGlobalNote",
];

// Effects that return from `runTrigger` outside display mode.
const DISPLAY_ONLY: &[&str] = &["v2GetDisplayState", "v2SetDisplayState", "v2GetRequestState", "v2SetRequestState",
    "v2GetRequestStateRole", "v2SetRequestStateRole", "v2GetRequestStateLength"];

fn shape() -> StageError { unsupported("trigger-script-shape") }

struct Fields<'a> { value: &'a Value, budget: &'a Budget }
impl<'a> Fields<'a> {
    fn get(&self, key: &str) -> Result<Option<&'a Value>, StageError> {
        Ok(self.value.get(key.as_bytes(), self.budget)?.filter(|value| !matches!(value, Value::Undefined)))
    }
    fn text(&self, key: &str) -> Result<Text, StageError> {
        match self.get(key)? {
            Some(Value::Text(text)) => Ok(text.copy(self.budget)?),
            _ => Err(unsupported("trigger-effect-field")),
        }
    }
    fn optional_text(&self, key: &str) -> Result<Option<Text>, StageError> {
        Ok(match self.get(key)? { Some(Value::Text(text)) => Some(text.copy(self.budget)?), _ => None })
    }
    fn is(&self, key: &str, literal: &str) -> Result<bool, StageError> {
        Ok(matches!(self.get(key)?, Some(Value::Text(text)) if equals(text, literal)))
    }
    /// `typeField === 'value' ? parse(field) : getVar(parse(field))`.
    fn src(&self, key: &str, kind: &str) -> Result<Src, StageError> {
        let text = self.text(key)?;
        Ok(if self.is(kind, "value")? { Src::Value(text) } else { Src::Var(text) })
    }
    fn value(&self, key: &str) -> Result<Src, StageError> { self.src(key, "valueType") }
}

/// The `type` of a trigger's first effect, as `canRunTrigger` reads it with `effect[0]?.type`.
fn first_type<'a>(effects: &'a [Value], budget: &Budget) -> Result<Option<&'a Value>, StageError> {
    match effects.first() {
        None | Some(Value::Undefined | Value::Null) => Ok(None),
        Some(effect) => Ok(effect.get(b"type", budget)?),
    }
}

struct Raw<'a> { value: &'a Value, low_level: bool }

/// Compiles every trigger the `top` selection reaches, directly or through `v2RunTrigger`, and declines anything
/// this stage does not run. Returns `None` when there are no triggers at all, where `runTrigger` returns `null`.
pub(crate) fn admit(input: &TriggerInput, top: Selector, budget: &Budget) -> Result<Option<(Program, Selector)>, StageError> {
    let mut raws = Vec::new();
    for list in &input.lists {
        let Value::Array(triggers) = &list.triggers else { return Err(shape()) };
        for trigger in triggers.as_slice() { raws.push(Raw { value: trigger, low_level: list.low_level }); }
    }
    if raws.is_empty() { return Ok(None); }
    let mut triggers = Vec::with_capacity(raws.len());
    for raw in &raws {
        budget.work(1)?;
        let fields = Fields { value: raw.value, budget };
        if !matches!(raw.value, Value::Object(_)) { return Err(shape()); }
        let Some(Value::Array(effects)) = fields.get("effect")? else { return Err(shape()) };
        let first = first_type(effects.as_slice(), budget)?;
        let any_mode = matches!(first, Some(Value::Text(kind)) if equals(kind, "triggercode") || equals(kind, "triggerlua"));
        triggers.push(Trigger { comment: fields.optional_text("comment")?, kind: fields.optional_text("type")?, any_mode,
            low_level: raw.low_level, body: None });
    }
    let mut selectors = vec![top];
    let mut next = 0;
    while next < selectors.len() {
        for index in 0..triggers.len() {
            if triggers[index].body.is_some() || !triggers[index].selected(&selectors[next]) { continue; }
            let body = compile(raws[index].value, triggers[index].low_level, budget)?;
            for effect in &body.effects {
                if let Op::RunTrigger { target } = &effect.op {
                    if !selectors.iter().any(|known| known.same(target)) { selectors.push(copy_selector(target, budget)?); }
                }
            }
            triggers[index].body = Some(body);
        }
        next += 1;
    }
    let top = selectors.swap_remove(0);
    Ok(Some((Program { triggers }, top)))
}

fn copy_selector(selector: &Selector, budget: &Budget) -> Result<Selector, StageError> {
    Ok(match selector {
        Selector::Name(name) => Selector::Name(name.copy(budget)?),
        Selector::Mode(mode) => Selector::Mode(mode),
        Selector::Nothing => Selector::Nothing,
    })
}

fn compile(trigger: &Value, low_level: bool, budget: &Budget) -> Result<Body, StageError> {
    let fields = Fields { value: trigger, budget };
    let Some(Value::Array(conditions)) = fields.get("conditions")? else { return Err(shape()) };
    let Some(Value::Array(effects)) = fields.get("effect")? else { return Err(shape()) };
    let mut body = Body { conditions: Vec::new(), effects: Vec::new() };
    for condition in conditions.as_slice() { body.conditions.push(compile_condition(condition, budget)?); }
    for effect in effects.as_slice() { body.effects.push(compile_effect(effect, low_level, budget)?); }
    Ok(body)
}

fn compile_condition(condition: &Value, budget: &Budget) -> Result<Condition, StageError> {
    if !matches!(condition, Value::Object(_)) { return Err(shape()); }
    let fields = Fields { value: condition, budget };
    let kind = fields.optional_text("type")?;
    let kind = kind.as_ref().map(|kind| ["var", "value", "chatindex", "exists"].into_iter().find(|name| equals(kind, name)));
    Ok(match kind.flatten() {
        Some("var") => Condition::Compare { left: Left::Var(fields.text("var")?), operator: fields.optional_text("operator")?, value: fields.text("value")? },
        Some("value") => Condition::Compare { left: Left::Literal(fields.text("var")?), operator: fields.optional_text("operator")?, value: fields.text("value")? },
        Some("chatindex") => Condition::Compare { left: Left::ChatIndex, operator: fields.optional_text("operator")?, value: fields.text("value")? },
        Some(_) => {
            // `0 - condition.depth` over a primitive depth.
            let depth = match fields.get("depth")? {
                None => f64::NAN,
                Some(Value::Null) => 0.0,
                Some(Value::Bool(value)) => f64::from(u8::from(*value)),
                Some(Value::Number(value)) => *value,
                Some(Value::Text(text)) => to_number(text, budget)?,
                Some(_) => return Err(unsupported("trigger-condition-depth")),
            };
            Condition::Exists { value: fields.text("value")?, kind: fields.optional_text("type2")?, start: 0.0 - depth }
        }
        None => Condition::Pass,
    })
}

fn indent(fields: &Fields<'_>) -> Result<Option<u32>, StageError> {
    match fields.value.get(b"indent", fields.budget)? {
        None => Ok(None),
        Some(Value::Number(value)) if value.fract() == 0.0 && *value >= 0.0 && *value <= f64::from(u32::MAX) => Ok(Some(*value as u32)),
        Some(_) => Err(unsupported("trigger-indent")),
    }
}

fn compile_effect(effect: &Value, low_level: bool, budget: &Budget) -> Result<Effect, StageError> {
    if !matches!(effect, Value::Object(_)) { return Err(shape()); }
    let fields = Fields { value: effect, budget };
    let indent = indent(&fields)?;
    let required = || indent.ok_or_else(|| unsupported("trigger-indent"));
    let Some(Value::Text(kind)) = fields.get("type")? else { return Ok(Effect { indent, op: Op::Noop }) };
    let kind_units: Vec<u16> = kind.units().collect();
    let name = String::from_utf16_lossy(&kind_units);
    if DECLINED.contains(&name.as_str()) {
        return Err(StageError::Unsupported(Cow::Owned(format!("trigger-effect-{name}"))));
    }
    if DISPLAY_ONLY.contains(&name.as_str()) { return Err(unsupported("trigger-early-return")); }
    let output = || fields.text("outputVar");
    let src = |key: &str, kind: &str| fields.src(key, kind);
    let op = match name.as_str() {
        "v2Header" | "v2Comment" => Op::Noop,
        "v2SetVar" => Op::SetVar { var: fields.text("var")?, value: fields.value("value")?, operator: fields.optional_text("operator")?, modulo: true },
        "v2DeclareLocalVar" => Op::DeclareLocal { var: fields.text("var")?, value: fields.value("value")?, indent: required()? },
        "v2If" => Op::If { source: Src::Var(fields.text("source")?), target: src("target", "targetType")?,
            condition: fields.optional_text("condition")?, indent: required()? },
        "v2IfAdvanced" => {
            let source = fields.text("source")?;
            let source = if fields.is("sourceType", "var")? { Src::Var(source) } else { Src::Value(source) };
            Op::If { source, target: src("target", "targetType")?, condition: fields.optional_text("condition")?, indent: required()? }
        }
        "v2Else" => Op::Else { indent: required()? },
        "v2EndIndent" => Op::EndIndent { indent: required()?, end_of_loop: fields.get("endOfLoop")?.is_some_and(Value::truthy) },
        "v2Loop" => Op::Loop,
        "v2LoopNTimes" => Op::LoopTimes { value: fields.value("value")? },
        "v2BreakLoop" => Op::BreakLoop,
        "v2RunTrigger" => Op::RunTrigger { target: target(&fields, "target")? },
        "v2ConsoleLog" => Op::ConsoleLog { source: src("source", "sourceType")? },
        "v2Tokenize" => Op::Tokenize { value: fields.value("value")?, output: output()? },
        "v2StopTrigger" => Op::StopTrigger,
        "v2CutChat" => Op::CutChat { start: src("start", "startType")?, end: src("end", "endType")?, defaults: true },
        "v2ModifyChat" => Op::ModifyChat { index: src("index", "indexType")?, value: fields.value("value")? },
        "v2SystemPrompt" => Op::SystemPrompt { location: location(&fields)?, value: fields.value("value")? },
        "v2Impersonate" => Op::Impersonate { role: role(&fields)?, value: fields.value("value")? },
        // The v1 effects parse every value and have no value types (`:1505-1747`).
        "setvar" => Op::SetVar { var: fields.text("var")?, value: Src::Value(fields.text("value")?), operator: fields.optional_text("operator")?,
            modulo: false },
        "systemprompt" => Op::SystemPrompt { location: location(&fields)?, value: Src::Value(fields.text("value")?) },
        "impersonate" => Op::Impersonate { role: role(&fields)?, value: Src::Value(fields.text("value")?) },
        "stop" => Op::StopSending,
        "runtrigger" => Op::RunTrigger { target: target(&fields, "value")? },
        "cutchat" => Op::CutChat { start: Src::Value(fields.text("start")?), end: Src::Value(fields.text("end")?), defaults: false },
        "modifychat" => Op::ModifyChat { index: Src::Value(fields.text("index")?), value: Src::Value(fields.text("value")?) },
        "sendAIprompt" => if low_level { Op::SendAiPrompt } else { Op::Noop },
        "extractRegex" if low_level => Op::LegacyExtract { value: fields.text("value")?, regex: fields.text("regex")?,
            flags: fields.text("flags")?, result: fields.text("result")?, output: fields.text("inputVar")? },
        // Low-level host callbacks; without low-level access each is a `break`.
        "runLLM" if low_level => Op::RunLlm { value: Src::Value(fields.text("value")?), model: Text::from_str("model", budget)?,
            streaming: false, output: fields.text("inputVar")?, legacy: true },
        "checkSimilarity" if low_level => Op::CheckSimilarity { source: Src::Value(fields.text("source")?),
            value: Src::Value(fields.text("value")?), output: fields.text("inputVar")?, legacy: true },
        "showAlert" | "runImgGen" if low_level => {
            return Err(StageError::Unsupported(Cow::Owned(format!("trigger-effect-{name}"))));
        }
        "showAlert" | "runLLM" | "checkSimilarity" | "runImgGen" | "extractRegex" => Op::Noop,
        "v2StopPromptSending" => Op::StopSending,
        "v2SendAIprompt" => if low_level { Op::SendAiPrompt } else { Op::Noop },
        "v2RunLLM" if low_level => {
            let streaming = match fields.get("streaming")? {
                None | Some(Value::Null) => false,
                Some(Value::Bool(streaming)) => *streaming,
                Some(_) => return Err(unsupported("trigger-effect-field")),
            };
            Op::RunLlm { value: fields.value("value")?, model: fields.text("model")?, streaming, output: output()?, legacy: false }
        }
        "v2CheckSimilarity" if low_level => Op::CheckSimilarity { source: src("source", "sourceType")?, value: fields.value("value")?,
            output: output()?, legacy: false },
        "v2ImgGen" => {
            if low_level { return Err(StageError::Unsupported(Cow::Owned(format!("trigger-effect-{name}")))); }
            Op::Noop
        }
        "v2CheckSimilarity" | "v2RunLLM" => Op::Noop,
        "v2ExtractRegex" => Op::ExtractRegex { value: fields.value("value")?, regex: src("regex", "regexType")?,
            flags: src("flags", "flagsType")?, result: src("result", "resultType")?, output: output()? },
        "v2GetLastMessage" => Op::LastMessage { output: output()? },
        "v2GetMessageAtIndex" => Op::MessageAt { index: src("index", "indexType")?, output: output()? },
        "v2GetMessageCount" => Op::MessageCount { output: output()? },
        "v2Random" => Op::Random { min: src("min", "minType")?, max: src("max", "maxType")?, output: output()? },
        "v2GetCharAt" => Op::CharAt { source: src("source", "sourceType")?, index: src("index", "indexType")?, output: output()? },
        "v2GetCharCount" => Op::CharCount { source: src("source", "sourceType")?, output: output()? },
        "v2ToLowerCase" | "v2ToUpperCase" => Op::Case { source: src("source", "sourceType")?, upper: name == "v2ToUpperCase", output: output()? },
        "v2SetCharAt" => Op::SetCharAt { source: src("source", "sourceType")?, index: src("index", "indexType")?,
            value: fields.value("value")?, output: output()? },
        "v2SplitString" => {
            let delimiter = fields.text("delimiter")?;
            let delimiter = if fields.is("delimiterType", "var")? {
                Src::Var(delimiter)
            } else if fields.is("delimiterType", "regex")? {
                return Err(unsupported("trigger-split-regex"));
            } else {
                Src::Value(delimiter)
            };
            Op::Split { source: src("source", "sourceType")?, delimiter, output: output()? }
        }
        "v2JoinArrayVar" => Op::Join { var: src("var", "varType")?, delimiter: src("delimiter", "delimiterType")?, output: output()? },
        "v2MakeArrayVar" => {
            let var = fields.text("var")?;
            if bracketed(&var, b'[', b']') { return Err(unsupported("trigger-early-return")); }
            Op::MakeArray { var }
        }
        "v2GetArrayVarLength" => Op::ArrayLength { var: fields.text("var")?, output: output()? },
        "v2GetArrayVar" => Op::ArrayGet { var: fields.text("var")?, index: src("index", "indexType")?, output: output()? },
        "v2PushArrayVar" | "v2UnshiftArrayVar" => Op::ArrayPush { var: fields.text("var")?, value: fields.value("value")?, front: name == "v2UnshiftArrayVar" },
        "v2PopArrayVar" | "v2ShiftArrayVar" => Op::ArrayTake { var: fields.text("var")?, output: output()?, front: name == "v2ShiftArrayVar" },
        "v2SpliceArrayVar" => Op::ArraySplice { var: fields.text("var")?, start: src("start", "startType")?, item: src("item", "itemType")? },
        "v2SliceArrayVar" => Op::ArraySlice { var: fields.text("var")?, start: src("start", "startType")?, end: src("end", "endType")?, output: output()? },
        "v2GetIndexOfValueInArrayVar" => Op::ArrayIndexOf { var: fields.text("var")?, value: fields.value("value")?, output: output()? },
        "v2RemoveIndexFromArrayVar" => Op::ArrayRemove { var: fields.text("var")?, index: src("index", "indexType")? },
        "v2SetArrayVar" => Op::ArraySet { var: fields.text("var")?, index: src("index", "indexType")?, value: fields.value("value")? },
        "v2ConcatString" => Op::Concat { left: src("source1", "source1Type")?, right: src("source2", "source2Type")?, output: output()? },
        "v2GetLastUserMessage" => Op::LastByRole { role: "user", output: output()? },
        "v2GetLastCharMessage" => Op::LastByRole { role: "char", output: output()? },
        "v2GetFirstMessage" => Op::FirstMessage { output: output()? },
        "v2QuickSearchChat" => Op::QuickSearch { value: fields.value("value")?, depth: src("depth", "depthType")?,
            condition: fields.optional_text("condition")?, output: output()? },
        "v2RegexTest" => Op::RegexTest { value: fields.value("value")?, regex: src("regex", "regexType")?, flags: src("flags", "flagsType")?, output: output()? },
        "v2Calculate" => Op::Calculate { expression: src("expression", "expressionType")?, output: output()? },
        "v2ReplaceString" => Op::ReplaceString { source: src("source", "sourceType")?, regex: src("regex", "regexType")?,
            result: src("result", "resultType")?, replacement: src("replacement", "replacementType")?, flags: src("flags", "flagsType")?,
            output: output()? },
        "v2GetAuthorNote" => Op::GetNote { output: output()? },
        "v2SetAuthorNote" => Op::SetNote { value: fields.value("value")? },
        "v2MakeDictVar" | "v2ClearDict" => {
            let var = fields.text("var")?;
            if bracketed(&var, b'{', b'}') { return Err(unsupported("trigger-early-return")); }
            Op::ResetDict { var }
        }
        "v2GetDictVar" => Op::DictGet { var: src("var", "varType")?, key: src("key", "keyType")?, output: output()? },
        "v2SetDictVar" => Op::DictSet { var: fields.text("var")?, var_type: var_type(&fields)?, key: src("key", "keyType")?, value: fields.value("value")? },
        "v2DeleteDictKey" => Op::DictDelete { var: fields.text("var")?, var_type: var_type(&fields)?, key: src("key", "keyType")? },
        "v2HasDictKey" => Op::DictHas { var: src("var", "varType")?, key: src("key", "keyType")?, output: output()? },
        "v2GetDictSize" => Op::DictSize { var: src("var", "varType")?, output: output()? },
        "v2GetDictKeys" | "v2GetDictValues" => Op::DictKeys { var: src("var", "varType")?, values: name == "v2GetDictValues", output: output()? },
        // `triggercode` and any type TS has no case for.
        _ => Op::Noop,
    };
    Ok(Effect { indent, op })
}

/// The `manualName` a nested `runTrigger` gets from `field`.
fn target(fields: &Fields<'_>, field: &str) -> Result<Selector, StageError> {
    Ok(match fields.get(field)? {
        Some(Value::Text(name)) if name.len_units() != 0 => Selector::Name(name.copy(fields.budget)?),
        Some(value) if value.truthy() => Selector::Nothing,
        _ => Selector::Mode("manual"),
    })
}

/// `additonalSysPrompt[effect.location]`.
fn location(fields: &Fields<'_>) -> Result<Location, StageError> {
    Ok(match fields.get("location")? {
        Some(Value::Text(text)) if equals(text, "start") => Location::Start,
        Some(Value::Text(text)) if equals(text, "historyend") => Location::HistoryEnd,
        Some(Value::Text(text)) if equals(text, "promptend") => Location::PromptEnd,
        _ => Location::Other,
    })
}

fn role(fields: &Fields<'_>) -> Result<Option<MessageRole>, StageError> {
    Ok(if fields.is("role", "user")? { Some(MessageRole::User) } else if fields.is("role", "char")? { Some(MessageRole::Char) } else { None })
}

fn var_type(fields: &Fields<'_>) -> Result<VarType, StageError> {
    Ok(if fields.is("varType", "value")? { VarType::Value } else if fields.is("varType", "var")? { VarType::Var } else { VarType::Other })
}

/// `text.startsWith(open) && text.endsWith(close)`.
pub(crate) fn bracketed(text: &Text, open: u8, close: u8) -> bool {
    text.units().next() == Some(u16::from(open)) && text.units().last() == Some(u16::from(close))
}
