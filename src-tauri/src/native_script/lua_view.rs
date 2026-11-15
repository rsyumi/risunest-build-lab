//! The conversation a Lua call reads and writes. [`Overlay`] is the stored records a preparation snapshot holds with the
//! request's view replayed over them; a write changes the overlay only, and the caller reports it as an effect.
use super::access::{FullMessage, HistorySelection, NumberArgument};
use super::lua_edit::copy_value;
use super::lua_variables::{assigned_from_ir, assigned_input, matches_ir, matches_json, BindingPolicy,
    RawOperation, RawVariables, ScopeKind, VariableFinish, VariableReader};
use super::lua_values::{project_rows, HistoryText, Rows};
use crate::persistent_store::script_access::owned_value_bytes;
use risunest_ir::chat::MessageRole;
use risunest_ir::stage::{Effect, LuaView, MessageEdit, ScopedTextProcessor, StageError, TextContext, TextScope, TextVariables};
use risunest_ir::{json, Budget, Charge, Error as IrError, OwnedVec, Text, Value};
use risunest_native_lua_experimental::{Scalar, ValueLimits, VariableValue, ValueEvent, ValueVisitor};
use risunest_records::edits::{at_index, relative_index};
use risunest_records::modules::strict_equals;
use serde_json::{Map, Value as Json};
use std::{cell::{Cell, RefCell}, collections::HashMap, sync::Arc, rc::Rc};

/// The stored records a Lua request reads, as the preparation snapshot holds them.
#[derive(Debug, Default)]
pub(crate) struct ConversationSnapshot {
    pub character: Json,
    /// The chat object. Its `message` is not read: `messages` holds the rows.
    pub conversation: Json,
    pub messages: Vec<Json>,
    pub root: Json,
}

/// A value `setChatVar` stores, as TS holds it: a Lua `nil` reaches JS as `null`, a missing argument as `undefined`.
#[derive(Clone, Debug)]
pub(crate) enum Assigned {
    Undefined, Null, Bool(bool), Number(f64), Text(String),
    Structured { value: Arc<Value>, input: VariableValue, _charge: Arc<Charge> },
}

impl PartialEq for Assigned {
    fn eq(&self, other:&Self)->bool {
        match (self,other) {
            (Self::Undefined,Self::Undefined)|(Self::Null,Self::Null)=>true,
            (Self::Bool(a),Self::Bool(b))=>a==b,
            (Self::Number(a),Self::Number(b))=>a==b,
            (Self::Text(a),Self::Text(b))=>a==b,
            (Self::Structured {input:a,..},Self::Structured {input:b,..})=>a==b,
            _=>false,
        }
    }
}

impl From<&str> for Assigned { fn from(value: &str) -> Self { Self::Text(value.to_owned()) } }

impl From<String> for Assigned { fn from(value: String) -> Self { Self::Text(value) } }

impl Assigned {
    pub(super) fn argument(argument: Option<&Scalar>) -> Self {
        match argument {
            None => Self::Undefined,
            Some(Scalar::Nil) => Self::Null,
            Some(Scalar::Boolean(value)) => Self::Bool(*value),
            Some(Scalar::Number(value)) => Self::Number(*value),
            Some(Scalar::String(value)) => Self::Text(value.clone()),
        }
    }

    pub(crate) fn converted(value:Option<VariableValue>,budget:&Budget)->Result<Self,String> {
        let Some(value)=value else {return Ok(Self::Undefined)};
        if let Some(scalar)=value.as_scalar() {return Ok(Self::argument(Some(&scalar)));}
        if !value.is_structured() && value.text_units().is_none() {return Err("unsupported variable argument".into());}
        let charge=Arc::new(budget.reserve_owned(value.owned_bytes().saturating_add(std::mem::size_of::<Charge>()+std::mem::size_of::<Value>()+4*std::mem::size_of::<usize>())).map_err(ir_error)?);
        let value=value.retain(charge.clone());
        let _scratch=budget.reserve_owned(value.export_scratch_bound()).map_err(ir_error)?;
        let mut builder=VariableBuilder {budget,frames:Vec::new(),frames_charge:None,root:None};
        value.visit_own(&mut builder)?;
        if !builder.frames.is_empty() {return Err("variable tree is incomplete".into());}
        Ok(Self::Structured {value:Arc::new(builder.root.ok_or("variable tree is empty")?),input:value,_charge:charge})
    }

    pub(super) fn value(&self, budget: &Budget) -> Result<Value, IrError> {
        Ok(match self {
            Self::Undefined => Value::Undefined,
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(*value),
            Self::Text(value) => Value::Text(Text::from_str(value, budget)?),
            Self::Structured { value, .. } => copy_value(value, budget)?,
        })
    }
}

fn prepare_live<T>(live:&mut Option<OwnedVec<(Text,Option<T>)>>,key:&str,budget:Budget)->Result<usize,String> {
    if live.is_none(){*live=Some(OwnedVec::new(&budget).map_err(ir_error)?);}
    let live=live.as_mut().ok_or("variable live map")?;
    if let Some(index)=live.as_slice().iter().position(|(name,_)|name.as_unicode()==Some(key)){return Ok(index);}
    let index=live.as_slice().len();
    live.push((Text::from_str(key,&budget).map_err(ir_error)?,None)).map_err(ir_error)?;
    Ok(index)
}

struct VariableFrame { value:Value,key:Option<Text> }
struct VariableBuilder<'a> {budget:&'a Budget,frames:Vec<VariableFrame>,frames_charge:Option<Charge>,root:Option<Value>}

impl VariableBuilder<'_> {
    fn push(&mut self,frame:VariableFrame)->Result<(),String>{
        if self.frames.len()==self.frames.capacity(){
            let capacity=self.frames.capacity().saturating_mul(2).max(4);
            let charge=self.budget.reserve_owned(capacity.saturating_mul(std::mem::size_of::<VariableFrame>())).map_err(ir_error)?;
            let mut frames=Vec::new();
            frames.try_reserve_exact(capacity).map_err(|_|"variable frame allocation")?;
            frames.append(&mut self.frames);
            self.frames=frames;
            self.frames_charge=Some(charge);
        }
        self.frames.push(frame);
        Ok(())
    }

    fn append(&mut self,value:Value)->Result<(),String> {
        match self.frames.last_mut() {
            Some(VariableFrame {value:Value::Array(items),..})=>items.push(value).map_err(ir_error),
            Some(VariableFrame {value:object @ Value::Object(_),key,..})=>object.define(key.take().ok_or("variable object key is absent")?,value,self.budget).map_err(ir_error),
            Some(_)=>Err("invalid variable frame".into()),
            None=>{if self.root.replace(value).is_some(){return Err("duplicate variable root".into());} Ok(())},
        }
    }
}

impl ValueVisitor for VariableBuilder<'_> {
    fn visit(&mut self,event:ValueEvent<'_>)->Result<(),String> {
        self.budget.work(1).map_err(ir_error)?;
        let value=match event {
            ValueEvent::Undefined=>Value::Undefined,
            ValueEvent::Null=>Value::Null,
            ValueEvent::Boolean(v)=>Value::Bool(v),
            ValueEvent::Number(v)=>Value::Number(v),
            ValueEvent::Text(v)=>Value::Text(Text::from_units(v,self.budget).map_err(ir_error)?),
            ValueEvent::Array(_)|ValueEvent::Object(_)=>{
                let value=if matches!(event,ValueEvent::Array(_)){Value::Array(OwnedVec::new(self.budget).map_err(ir_error)?)}
                    else {Value::Object(OwnedVec::new(self.budget).map_err(ir_error)?)};
                return self.push(VariableFrame {value,key:None});
            }
            ValueEvent::Key(key)=>{
                self.frames.last_mut().ok_or("variable key outside object")?.key=Some(Text::from_units(key,self.budget).map_err(ir_error)?);
                return Ok(());
            }
            ValueEvent::End=>self.frames.pop().ok_or("variable container ended twice")?.value,
        };
        self.append(value)
    }
}

pub(super) fn variable_input(value:&Value,limits:ValueLimits,budget:&Budget)->Result<VariableValue,String> {
    fn convert(value:&Value,limits:ValueLimits,budget:&Budget,nodes:&mut usize,depth:usize)->Result<VariableValue,String> {
        budget.work(1).map_err(ir_error)?;
        *nodes=nodes.checked_add(1).filter(|n|*n<=limits.nodes).ok_or("variable output node limit exceeded")?;
        if depth>limits.depth {return Err("variable output depth limit exceeded".into());}
        Ok(match value {
            Value::Undefined=>VariableValue::undefined(limits),
            Value::Null=>VariableValue::scalar(Scalar::Nil,limits),
            Value::Bool(v)=>VariableValue::scalar(Scalar::Boolean(*v),limits),
            Value::Number(v)=>VariableValue::scalar(Scalar::Number(*v),limits),
            Value::Text(v)=>VariableValue::text(v.units().collect(),limits),
            Value::Array(items)=>VariableValue::array(items.as_slice().iter().map(|v|convert(v,limits,budget,nodes,depth+1)).collect::<Result<_,_>>()?,limits),
            Value::Object(items)=>VariableValue::object(items.as_slice().iter().map(|(key,v)| {
                Ok((key.units().collect(),convert(v,limits,budget,nodes,depth+1)?))
            }).collect::<Result<_,String>>()?,limits),
        })
    }
    fn bound(value:&Value,budget:&Budget,limits:ValueLimits,nodes:&mut usize,depth:usize)->Result<usize,String>{
        budget.work(1).map_err(ir_error)?;
        *nodes=nodes.checked_add(1).filter(|n|*n<=limits.nodes).ok_or("variable output node limit exceeded")?;
        if depth>limits.depth{return Err("variable output depth limit exceeded".into());}
        let mut bytes=256usize;
        match value{
            Value::Text(text)=>bytes=bytes.saturating_add(text.len_units().saturating_mul(8)),
            Value::Array(items)=>for item in items.as_slice(){bytes=bytes.saturating_add(bound(item,budget,limits,nodes,depth+1)?);},
            Value::Object(items)=>for(key,item)in items.as_slice(){bytes=bytes.saturating_add(key.len_units().saturating_mul(2)).saturating_add(bound(item,budget,limits,nodes,depth+1)?);},
            _=>{}
        }
        if bytes>limits.utf8_bytes{return Err("variable output byte limit exceeded".into());}
        Ok(bytes)
    }
    let charge=Arc::new(budget.reserve_owned(bound(value,budget,limits,&mut 0,0)?.saturating_add(std::mem::size_of::<Charge>()+2*std::mem::size_of::<usize>())).map_err(ir_error)?);
    let value=convert(value,limits,budget,&mut 0,0)?.retain(charge);
    if value.owned_bytes()>limits.utf8_bytes {return Err("variable output byte limit exceeded".into());}
    Ok(value)
}

pub(super) type PersonaProcessor = Rc<RefCell<Box<dyn ScopedTextProcessor + Send>>>;

/// What the host functions read and write. Each invocation runs between `begin` and `end`.
pub(super) trait RecordValues {
    fn field(&self, record: &str, field: &str) -> Result<Option<Value>, String>;
}

pub(super) trait Conversation {
    fn bind_variable_budget(&self, _budget: &Budget) -> Result<(), String> { Ok(()) }
    fn begin(&self) -> Result<(), String>;
    /// Runs after every invocation, whatever ended it.
    fn end(&self) -> Result<(), String>;
    /// What `getVar(key)` returns, or `None` for a value this host cannot pass to Lua.
    fn variable(&self, key: &str) -> Result<Option<Scalar>, String>;
    /// `setVar(key, value)`; whether it changed what `getVar` reads.
    fn set_variable(&self, key: &str, value: &Assigned) -> Result<bool, String>;
    fn variable_input(&self,key:&str,limits:ValueLimits)->Result<VariableValue,String> {
        Ok(VariableValue::scalar(self.variable(key)?.ok_or("unsupported variable value")?,limits))
    }
    fn raw_variable_input(&self, key: &VariableValue, limits: ValueLimits) -> Result<VariableValue, String>;
    fn set_raw_variable(&self, key: VariableValue, value: Assigned,
        prepare: &mut dyn FnMut(&RawOperation) -> Result<(), String>) -> Result<bool, String>;
    fn open_variable_scope(&self, kind: ScopeKind) -> Result<(), String>;
    fn finish_variables(&self, finish: VariableFinish) -> Result<Vec<Effect>, String>;
    fn legacy_variable_effects_only(&self) {}
    fn record_field(&self, record: &str, field: &str) -> Result<Option<Json>, String>;
    fn read_records(&self, budget: &Budget, read: &mut dyn FnMut(&dyn RecordValues) -> Result<(), String>) -> Result<(), String>;
    fn refresh_persona_history(&self, _processor: &PersonaProcessor, _budget: &Budget) -> Result<(), String> { Ok(()) }
    fn persona_description(&self, _effects: &[Effect]) -> Result<Option<Value>, String> { Err("lua-host-capability:getPersonaDescription".into()) }
    fn cut(&self, start: f64, end: Option<f64>) -> Result<(), String>;
    fn length(&self) -> Result<i64, String>;
    /// `field` of the message `at(index)` finds.
    fn message_field(&self, index: f64, field: &str) -> Result<Option<Json>, String>;
    fn message_value(&self, index: f64, field: &str, budget: &Budget) -> Result<Option<Value>, String>;
    fn history(&self, selection: HistorySelection, full: bool, limits: ValueLimits) -> Result<HistoryText, String>;
    /// `message[field] = value` on the message `at(index)` finds, and no edit when it finds none.
    fn set_message(&self, index: f64, field: &'static str, value: String) -> Result<(), String>;
    /// `message.splice(index, 1)`.
    fn remove(&self, index: f64) -> Result<(), String>;
    /// `message.splice(index, 0, message)`.
    fn insert(&self, index: f64, message: Json) -> Result<(), String>;
    /// `appendMessage`'s `message.push(message)`, which opens the message edits unless an operation is open.
    fn append(&self, message: Json) -> Result<(), String>;
    /// `setFullChatMain`'s assignment after its `chatId` merge; returns the rows.
    fn replace(&self, messages: Vec<FullMessage>) -> Result<Vec<Json>, String> {
        self.replace_prepared(messages, &mut |_| Ok(()))
    }
    /// Reserves the complete merged rows before publishing the replacement.
    fn replace_prepared(&self, messages: Vec<FullMessage>, prepare: &mut dyn FnMut(&[Json]) -> Result<(), String>) -> Result<Vec<Json>, String>;
}

fn ir_error(error: IrError) -> String { format!("ir: {error:?}") }

fn unicode(text: &Text, label: &str) -> Result<String, String> { text.as_unicode().map(str::to_owned).ok_or_else(|| label.to_owned()) }

/// A stored JSON value as the IR reads it.
pub(super) fn ir_value(value: &Json, budget: &Budget) -> Result<Value, String> {
    let _scratch=budget.reserve_owned(owned_value_bytes(value).saturating_mul(6).saturating_add(256)).map_err(ir_error)?;
    json::parse_json(&value.to_string(), budget).map_err(ir_error)
}

/// The stored form of a value TS keeps in the working copy: `undefined` leaves an object member out and reads as `null`
/// in an array. A value JSON cannot hold declines.
fn stored(value: &Value) -> Result<Option<Json>, String> {
    Ok(Some(match value {
        Value::Undefined => return Ok(None),
        Value::Null => Json::Null,
        Value::Bool(value) => Json::Bool(*value),
        Value::Number(value) => Json::Number(serde_json::Number::from_f64(*value).ok_or("lua-view-number")?),
        Value::Text(text) => Json::String(unicode(text, "lua-view-text")?),
        Value::Array(items) => Json::Array(items.as_slice().iter().map(|item| Ok(stored(item)?.unwrap_or(Json::Null)))
            .collect::<Result<_, String>>()?),
        Value::Object(entries) => {
            let mut object = Map::new();
            for (key, item) in entries.as_slice() {
                if let Some(item) = stored(item)? { object.insert(unicode(key, "lua-view-text")?, item); }
            }
            Json::Object(object)
        }
    }))
}

/// The value as Lua receives it from JS: `null` and `undefined` as `nil`; an array, an object or a string Lua cannot
/// hold is `None`.
fn given(value: &Value) -> Option<Scalar> {
    match value {
        Value::Undefined | Value::Null => Some(Scalar::Nil),
        Value::Bool(value) => Some(Scalar::Boolean(*value)),
        Value::Number(value) => Some(Scalar::Number(*value)),
        Value::Text(text) => text.as_unicode().map(|text| Scalar::String(text.to_owned())),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// `String(value)`.
fn string_of(value: &Value, budget: &Budget) -> Result<Option<Scalar>, String> {
    Ok(value.to_text(budget).map_err(ir_error)?.as_unicode().map(|text| Scalar::String(text.to_owned())))
}

fn upsert(variables: &mut Vec<(String, Value)>, key: String, value: Value) {
    match variables.iter_mut().find(|(name, _)| *name == key) {
        Some(entry) => entry.1 = value,
        None => variables.push((key, value)),
    }
}

/// `parseKeyValue(char.defaultVariables)` then `templateDefaultVariables`; the first key wins.
fn defaults(character: &Json, root: &Json) -> Vec<(String, String)> {
    let mut defaults: Vec<(String, String)> = Vec::new();
    for source in [character.get("defaultVariables"), root.get("templateDefaultVariables")] {
        let Some(source) = source.and_then(Json::as_str) else { continue };
        for line in source.split('\n') {
            let mut parts = line.split('=');
            if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                if !key.is_empty() && !value.is_empty() && !defaults.iter().any(|(existing, _)| existing == key) {
                    defaults.push((key.to_owned(), value.to_owned()));
                }
            }
        }
    }
    defaults
}

#[derive(Clone)]
enum Row { Stored(usize), Owned(Arc<(Json, Charge)>) }

#[derive(Default)]
struct State {
    /// `None` while the rows are the snapshot's, in order.
    rows: Option<Vec<Row>>,
    rows_charge: Option<Charge>,
    committed_rows: Option<(Vec<Row>, Charge)>,
    history_generation: u64,
    /// An edit listener's view variables, which TS reads from the stored chat after the earlier commit.
    stored: Vec<(String, Value)>,
    /// `pendingVariables`: this request's writes and, for trigger Lua, the view's variables, which TS still holds in the
    /// trigger's access.
    pending: Vec<(String, Value)>,
    /// The trigger locals `getVar` reads first.
    locals: Vec<(String, Value)>,
    defaults: Option<Vec<(String, String)>>,
    defaults_charge: Option<Charge>,
    raw: Option<RawVariables>,
    chat_fields: Vec<(String, Value)>,
    character_fields: Vec<(String, Value)>,
    live: Option<OwnedVec<(Text,Option<Assigned>)>>,
}

enum StoredVariable<'a> { Missing, Value(&'a Value), Json(&'a Json) }

/// The snapshot with the view's effects and this request's writes.
pub(super) struct Overlay {
    snapshot: Arc<ConversationSnapshot>,
    budget: RefCell<Budget>,
    state: RefCell<State>,
    processor: Option<PersonaProcessor>,
    processor_generation: Cell<u64>,
}

impl Overlay {
    fn budget(&self) -> Budget { self.budget.borrow().clone() }
    /// Replays `view` over `snapshot` in list order: within each kind of effect that is the order the send made them.
    pub(super) fn new(snapshot: Arc<ConversationSnapshot>, view: &LuaView, trigger: bool, budget: &Budget) -> Result<Self, String> {
        let overlay = Self { snapshot, budget: RefCell::new(budget.clone()), state: RefCell::new(State::default()), processor: None, processor_generation: Cell::new(0) };
        {
            let mut state = overlay.state.borrow_mut();
            for effect in &view.effects {
                match effect {
                    Effect::ChatVariable { name, value } => {
                        let value = copy_value(value, budget).map_err(ir_error)?;
                        let layer = if trigger { &mut state.pending } else { &mut state.stored };
                        upsert(layer, unicode(name, "lua-view-variable")?, value);
                    }
                    Effect::ChatField { field, value } if field.equals_ascii(b"message") => {
                        overlay.apply(&mut state, &MessageEdit::Replace { messages: copy_value(value, budget).map_err(ir_error)? })?;
                    }
                    Effect::ChatField { field, value } => {
                        if field.equals_ascii(b"scriptstate") { state.stored.clear(); state.pending.clear(); }
                        upsert(&mut state.chat_fields, unicode(field, "lua-view-field")?, copy_value(value, budget).map_err(ir_error)?);
                    }
                    Effect::CharacterField { field, value } => {
                        upsert(&mut state.character_fields, unicode(field, "lua-view-field")?, Value::Text(value.copy(budget).map_err(ir_error)?));
                    }
                    Effect::Message(edit) => overlay.apply(&mut state, edit)?,
                    _ => return Err("lua-view-effect".into()),
                }
            }
            if trigger {
                for (name, value) in &view.locals {
                    let value = copy_value(value, budget).map_err(ir_error)?;
                    upsert(&mut state.locals, unicode(name, "lua-view-variable")?, value);
                }
            }
        }
        {
            let mut state = overlay.state.borrow_mut();
            let mut raw = RawVariables::new(budget, BindingPolicy::AccessScope, 1);
            let limits = risunest_native_lua_experimental::Limits::default().values;
            for (name, value) in std::mem::take(&mut state.pending) {
                let key = super::lua_variables::string_key(&name, budget)?;
                let value = assigned_from_ir(&value, limits, budget)?;
                if let Some(prepared) = raw.prepare(key, value, true, |_| Ok(Assigned::Undefined))? { raw.publish(prepared); }
            }
            for (name, value) in std::mem::take(&mut state.locals) {
                raw.seed_local(Text::from_str(&name, budget).map_err(ir_error)?, assigned_from_ir(&value, limits, budget)?)?;
            }
            state.raw = Some(raw);
            if let Some(rows) = state.rows.take() {
                let charge = state.rows_charge.take().ok_or("lua-view-rows")?;
                state.committed_rows = Some((rows, charge));
            }
            state.history_generation = 0;
        }
        Ok(overlay)
    }

    pub(super) fn with_binding_policy(self, policy: BindingPolicy) -> Result<Self, String> {
        {
            let mut state = self.state.borrow_mut();
            let raw = state.raw.as_mut().ok_or("variable scope missing")?;
            if raw.len() != 0 { return Err("binding policy must be selected before pending writes".into()); }
            raw.policy = policy;
        }
        Ok(self)
    }

    pub(super) fn legacy_effects_only(&self) {
        if let Some(raw) = self.state.borrow_mut().raw.as_mut() { raw.legacy_effects_only(); }
    }

    /// The processor already reflects the effective records after this request's view; replaying the view here would
    /// apply inserts and cuts twice. Later history changes refresh it from the current source.
    pub(super) fn with_processor(mut self, processor: PersonaProcessor) -> Self {
        self.processor_generation.set(self.state.borrow().history_generation);
        self.processor = Some(processor);
        self
    }

    fn len(&self, state: &State) -> usize {
        state.rows.as_ref().or_else(|| state.committed_rows.as_ref().map(|(rows, _)| rows))
            .map_or(self.snapshot.messages.len(), Vec::len)
    }

    fn row<'a>(&'a self, state: &'a State, index: usize) -> &'a Json {
        match state.rows.as_ref().or_else(|| state.committed_rows.as_ref().map(|(rows, _)| rows)).map(|rows| &rows[index]) {
            None => &self.snapshot.messages[index],
            Some(Row::Stored(stored)) => &self.snapshot.messages[*stored],
            Some(Row::Owned(row)) => &row.0,
        }
    }

    fn owned(&self, row: Json) -> Result<Row, String> {
        let charge = self.budget().reserve_owned(owned_value_bytes(&row).saturating_add(std::mem::size_of::<Charge>() + 2 * std::mem::size_of::<usize>())).map_err(ir_error)?;
        Ok(Row::Owned(Arc::new((row, charge))))
    }

    /// Materializes and grows the row references only after their new allocation fits the budget.
    fn prepare_rows(&self, state: &mut State, additional: usize) -> Result<(), String> {
        let length = self.len(state);
        let needed = length.checked_add(additional).ok_or("lua-view-row-limit")?;
        if state.rows.as_ref().is_some_and(|rows| rows.capacity() >= needed) { return Ok(()); }
        let capacity = needed.max(state.rows.as_ref().map_or(0, |rows| rows.capacity().saturating_mul(2)));
        let charge = self.budget().reserve_owned(capacity.saturating_mul(std::mem::size_of::<Row>())).map_err(ir_error)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(capacity).map_err(|_| "lua-view-row-allocation")?;
        match state.rows.take() {
            Some(mut previous) => rows.append(&mut previous),
            None => match &state.committed_rows {
                Some((committed, _)) => rows.extend(committed.iter().cloned()),
                None => rows.extend((0..self.snapshot.messages.len()).map(Row::Stored)),
            },
        }
        state.rows = Some(rows);
        state.rows_charge = Some(charge);
        Ok(())
    }

    fn restructure(&self, state: &mut State, additional: usize, change: impl FnOnce(&mut Vec<Row>) -> Result<(), String>) -> Result<(), String> {
        self.prepare_rows(state, additional)?;
        change(state.rows.as_mut().ok_or("lua-view-rows")?)?;
        state.history_generation = state.history_generation.wrapping_add(1);
        Ok(())
    }

    fn assign_rows(&self, state: &mut State, rows: Vec<Row>) -> Result<(), String> {
        let charge = self.budget().reserve_owned(rows.capacity().saturating_mul(std::mem::size_of::<Row>())).map_err(ir_error)?;
        state.rows = Some(rows);
        state.rows_charge = Some(charge);
        state.history_generation = state.history_generation.wrapping_add(1);
        Ok(())
    }

    fn set_field(&self, state: &mut State, index: f64, field: &str, value: Option<Json>) -> Result<(), String> {
        let Some(at) = at_index(index, self.len(state)) else { return Ok(()); };
        let mut message = self.row(state, at).clone();
        let Json::Object(object) = &mut message else { return Err("message is not an object".into()) };
        match value { Some(value) => { object.insert(field.to_owned(), value); } None => { object.remove(field); } }
        let row = self.owned(message)?;
        self.prepare_rows(state, 0)?;
        state.rows.as_mut().ok_or("lua-view-rows")?[at] = row;
        state.history_generation = state.history_generation.wrapping_add(1);
        Ok(())
    }

    fn remove_at(&self, state: &mut State, index: f64) -> Result<(), String> {
        let at = relative_index(index, self.len(state));
        self.restructure(state, 0, |rows| { if at < rows.len() { rows.remove(at); } Ok(()) })
    }

    fn insert_at(&self, state: &mut State, index: f64, message: Json) -> Result<(), String> {
        let at = relative_index(index, self.len(state));
        let row = self.owned(message)?;
        self.restructure(state, 1, |rows| { rows.insert(at, row); Ok(()) })
    }

    /// One view edit, by the rule `risunest_records::edits::edit_messages` applies it.
    fn apply(&self, state: &mut State, edit: &MessageEdit) -> Result<(), String> {
        let role = |role: MessageRole| Json::String(match role { MessageRole::User => "user", MessageRole::Char => "char" }.into());
        let message = |role: MessageRole, data: &Value, chat_id: &Text| -> Result<Json, String> {
            let mut message = Map::new();
            message.insert("role".into(), Json::String(match role { MessageRole::User => "user", MessageRole::Char => "char" }.into()));
            if let Some(data) = stored(data)? { message.insert("data".into(), data); }
            message.insert("chatId".into(), Json::String(unicode(chat_id, "lua-view-text")?));
            Ok(Json::Object(message))
        };
        match edit {
            MessageEdit::SetData { index, data } => self.set_field(state, *index, "data", stored(data)?),
            MessageEdit::SetRole { index, role: name } => self.set_field(state, *index, "role", Some(role(*name))),
            MessageEdit::Cut { start, end } => {
                let length = self.len(state);
                let from = relative_index(*start, length);
                let to = end.map_or(length, |end| relative_index(end, length)).max(from);
                self.restructure(state, 0, |rows| { rows.truncate(to); rows.drain(..from); Ok(()) })
            }
            MessageEdit::Remove { index } => self.remove_at(state, *index),
            MessageEdit::Append { role, data, chat_id } => {
                let row = self.owned(message(*role, data, chat_id)?)?;
                self.restructure(state, 1, |rows| { rows.push(row); Ok(()) })
            }
            MessageEdit::Insert { index, role, data, chat_id } => self.insert_at(state, *index, message(*role, data, chat_id)?),
            MessageEdit::Replace { messages } => {
                let Value::Array(items) = messages else { return Err("lua-view-message-replace".into()) };
                let rows = items.as_slice().iter().map(|item| self.owned(stored(item)?.unwrap_or(Json::Null))).collect::<Result<Vec<_>, _>>()?;
                self.assign_rows(state, rows)
            }
            _ => Err("lua-view-message-edit".into()),
        }
    }

    fn variable_value(&self, key: &str) -> Result<Value, String> {
        let limits = risunest_native_lua_experimental::Limits::default().values;
        let value = self.variable_input(key, limits)?;
        assigned_from_ir_input(value, limits, &self.budget())?.value(&self.budget()).map_err(ir_error)
    }

    fn stored_variable<'a>(&'a self, state: &'a State, key: &str) -> StoredVariable<'a> {
        if let Some((_, value)) = state.stored.iter().find(|(name, _)| name == key) { return StoredVariable::Value(value); }
        if let Some((_, scriptstate)) = state.chat_fields.iter().find(|(name, _)| name == "scriptstate") {
            return match scriptstate {
                Value::Object(entries) => entries.as_slice().iter().find(|(name, _)| name.as_unicode() == Some(format!("${key}").as_str()))
                    .map_or(StoredVariable::Missing, |(_, value)| StoredVariable::Value(value)),
                _ => StoredVariable::Missing,
            };
        }
        match self.snapshot.conversation.get("scriptstate").and_then(|variables| variables.get(format!("${key}"))) {
            Some(value) => StoredVariable::Json(value),
            None => StoredVariable::Missing,
        }
    }
}

pub(super) fn with_records<T>(source: &dyn Conversation, budget: &Budget,
    mut read: impl FnMut(&dyn RecordValues) -> Result<T, String>) -> Result<T, String> {
    let mut value = None;
    source.read_records(budget, &mut |records| { value = Some(read(records)?); Ok(()) })?;
    value.ok_or_else(|| "lua-record-read".into())
}

pub(super) fn record_value(source: &dyn Conversation, record: &str, field: &str, budget: &Budget) -> Result<Option<Value>, String> {
    with_records(source, budget, |records| records.field(record, field))
}

/// Object keys compare by UTF-16 units, including non-ASCII variable names.
pub(super) fn property<'a>(value: &'a Value, key: &Text, budget: &Budget) -> Result<Option<&'a Value>, String> {
    if let Value::Object(entries) = value {
        for (name, value) in entries.as_slice() {
            budget.work(name.len_units().min(key.len_units()).saturating_add(1)).map_err(ir_error)?;
            if name == key { return Ok(Some(value)); }
        }
    }
    Ok(None)
}

/// The bound persona takes precedence; the effective root mirrors supply the selected persona otherwise.
pub(super) fn persona_field(source: &dyn Conversation, field: &str, fallback: &str, budget: &Budget) -> Result<Option<Value>, String> {
    with_records(source, budget, |records| {
        if let Some(bound) = records.field("conversation", "bindedPersona")?.filter(Value::truthy) {
            if let Some(Value::Array(personas)) = records.field("root", "personas")? {
                for persona in personas.as_slice() {
                    if matches!(persona, Value::Null) { break; }
                    if persona.get(b"id", budget).map_err(ir_error)?.is_some_and(|id| strict_equals(id, &bound)) {
                        return persona.get(field.as_bytes(), budget).map_err(ir_error)?.map(|value| copy_value(value, budget).map_err(ir_error)).transpose();
                    }
                }
            }
        }
        Ok(Some(match records.field("root", if field == "name" { "username" } else { field })? {
            None | Some(Value::Null | Value::Undefined) => Value::Text(Text::from_str(fallback, budget).map_err(ir_error)?),
            Some(value) => value,
        }))
    })
}

/// Resolve a read against the current raw fields without changing the records used by write guards.
pub(super) fn author_note(source: &dyn Conversation, budget: &Budget) -> Result<Value, String> {
    with_records(source, budget, |records| {
        let mut conversation = Value::Object(OwnedVec::new(budget).map_err(ir_error)?);
        for field in ["note", "authorNotePreset"] {
            if let Some(value) = records.field("conversation", field)? {
                conversation.define(Text::from_str(field, budget).map_err(ir_error)?, value, budget).map_err(ir_error)?;
            }
        }
        let attached = risunest_records::author_note::matching_id(&conversation, budget).map_err(note_error)?.is_some();
        let mut root = Value::Object(OwnedVec::new(budget).map_err(ir_error)?);
        let binding = if attached {
            if let Some(value) = records.field("root", "authorNotePresets")? {
                root.define(Text::from_str("authorNotePresets", budget).map_err(ir_error)?, value, budget).map_err(ir_error)?;
            }
            risunest_records::author_note::Binding::from_records(&root, &conversation, budget).map_err(note_error)?
        } else { None };
        drop(root);
        let raw = conversation.get(b"note", budget).map_err(ir_error)?;
        let value = risunest_records::author_note::resolve(binding.as_ref(), raw, budget).map_err(note_error)?;
        match value {
            None | Some(Value::Null | Value::Undefined) => Ok(Value::Text(Text::from_str("", budget).map_err(ir_error)?)),
            Some(value) => copy_value(value, budget).map_err(ir_error),
        }
    })
}

fn note_error(error: StageError) -> String {
    match error {
        StageError::Unsupported(label) if label == "author-note-presets" => "lua-script-error:author-note-presets".into(),
        error => format!("lua-author-note: {error:?}"),
    }
}



fn assigned_from_ir_input(value: VariableValue, _: ValueLimits, budget: &Budget) -> Result<Assigned, String> {
    if value.is_undefined() { Ok(Assigned::Undefined) } else { Assigned::converted(Some(value), budget) }
}

struct OverlayVariables<'a> {
    snapshot: &'a ConversationSnapshot,
    budget: &'a Budget,
    stored: &'a [(String, Value)],
    defaults: &'a mut Option<Vec<(String, String)>>,
    defaults_charge: &'a mut Option<Charge>,
    chat_fields: &'a [(String, Value)],
    character_fields: &'a [(String, Value)],
}

impl VariableReader for OverlayVariables<'_> {
    fn character_available(&mut self) -> Result<bool, String> { Ok(!self.snapshot.character.is_null()) }

    fn defaults_available(&mut self) -> Result<bool, String> {
        if self.defaults.is_none() {
            let character = self.character_fields.iter().find(|(name, _)| name == "defaultVariables");
            let character = match character {
                Some((_, Value::Text(value))) => value.as_unicode(),
                Some(_) => None,
                None => self.snapshot.character.get("defaultVariables").and_then(Json::as_str),
            };
            let template = self.snapshot.root.get("templateDefaultVariables").and_then(Json::as_str);
            let sources = [character, template];
            let bytes = sources.iter().flatten().map(|source| source.len()).sum::<usize>();
            self.budget.work(bytes.saturating_mul(3).saturating_add(1)).map_err(ir_error)?;
            let charge = self.budget.reserve_owned(bytes.saturating_mul(40).saturating_add(128)).map_err(ir_error)?;
            let mut values = Vec::<(String, String)>::new();
            for source in sources.into_iter().flatten() {
                for line in source.split('\n') {
                    let mut parts = line.split('=');
                    if let (Some(key), Some(value)) = (parts.next(), parts.next()) {
                        if !key.is_empty() && !value.is_empty() {
                            let mut present = false;
                            for (name, _) in &values {
                                self.budget.work(name.len().min(key.len()).saturating_add(1)).map_err(ir_error)?;
                                if name == key { present = true; break; }
                            }
                            if !present { values.push((key.into(), value.into())); }
                        }
                    }
                }
            }
            *self.defaults = Some(values);
            *self.defaults_charge = Some(charge);
        }
        Ok(true)
    }

    fn stored(&mut self, name: &Text) -> Result<Value, String> {
        for (candidate, value) in self.stored {
            self.budget.work(candidate.len().min(name.len_units()).saturating_add(1)).map_err(ir_error)?;
            if name.units().eq(candidate.encode_utf16()) { return copy_value(value, self.budget).map_err(ir_error); }
        }
        let mut key = risunest_ir::TextBuilder::new(self.budget).map_err(ir_error)?;
        key.unit(b'$' as u16).map_err(ir_error)?;
        for unit in name.units() { key.unit(unit).map_err(ir_error)?; }
        let key = key.finish().map_err(ir_error)?;
        if let Some((_, state)) = self.chat_fields.iter().find(|(name, _)| name == "scriptstate") {
            return property(state, &key, self.budget)?.map(|value| copy_value(value, self.budget).map_err(ir_error))
                .unwrap_or(Ok(Value::Undefined));
        }
        match key.as_unicode().and_then(|key| self.snapshot.conversation.get("scriptstate").and_then(|state| state.get(key))) {
            Some(value) => ir_value(value, self.budget),
            None => Ok(Value::Undefined),
        }
    }

    fn default(&mut self, key: Option<&[u16]>) -> Result<Value, String> {
        self.defaults_available()?;
        if let Some(key) = key {
            for (name, value) in self.defaults.as_ref().into_iter().flatten() {
                self.budget.work(key.len().min(name.len()).saturating_add(1)).map_err(ir_error)?;
                if key.iter().copied().eq(name.encode_utf16()) {
                    return Text::from_str(value, self.budget).map(Value::Text).map_err(ir_error);
                }
            }
        }
        Text::from_str("null", self.budget).map(Value::Text).map_err(ir_error)
    }
}

struct PersonaVariables<'a>(&'a dyn Conversation);

impl TextVariables for PersonaVariables<'_> {
    fn get(&mut self, key: Option<&Text>, budget: &Budget) -> Result<Text, StageError> {
        self.get_value(key, budget)?.to_text(budget).map_err(StageError::from)
    }

    fn get_value(&mut self, key: Option<&Text>, budget: &Budget) -> Result<Value, StageError> {
        let limits = risunest_native_lua_experimental::Limits::default().values;
        let key = match key {
            Some(key) => {
                let held = Arc::new(budget.reserve_owned(key.len_units().saturating_mul(2).saturating_add(128))?);
                VariableValue::text(key.units().collect(), limits).retain(held)
            },
            None => super::lua_variables::string_key("undefined", budget).map_err(|_| StageError::Limit("lua-persona-variable".into()))?,
        };
        let input = self.0.raw_variable_input(&key, limits).map_err(|error| if error.contains("Limit") || error.contains("limit") {
            StageError::Limit("lua-persona-variable".into())
        } else { StageError::Unsupported("lua-persona-variable".into()) })?;
        if input.is_structured() {
            let mut quota_error = None;
            let has_var = input.has_nonnull_property_checked("var", || budget.work(1).map_err(|error| {
                quota_error = Some(error); "variable query work limit".into()
            }));
            if let Some(error) = quota_error { return Err(error.into()); }
            if has_var.map_err(|_| StageError::Unsupported("lua-persona-variable".into()))? {
                return Err(StageError::Unsupported("matcher-result-var".into()));
            }
        }
        assigned_from_ir_input(input, limits, budget).map_err(|_| StageError::Unsupported("lua-persona-variable".into()))?
            .value(budget).map_err(StageError::from)
    }

    fn set(&mut self, _: Option<&Text>, _: Option<&Text>, _: &Budget) -> Result<(), StageError> {
        Err(StageError::Unsupported("lua-persona-write".into()))
    }
}

fn process_persona(source: &dyn Conversation, processor: Option<&PersonaProcessor>, budget: &Budget) -> Result<Option<Value>, String> {
        let processor = processor.ok_or("lua-host-capability:getPersonaDescription")?;
        source.refresh_persona_history(processor, budget)?;
        let Some(Value::Text(input)) = persona_field(source, "personaPrompt", "", budget)? else { return Err("lua-persona-input".into()) };
        let mut variables = PersonaVariables(source);
        let result = processor.borrow_mut().process_scoped(&input, &TextContext::default(),
            TextScope { variables: &mut variables, trigger_id: None }, budget)
            .map_err(|error|match error{
                StageError::Invalid(_)=>"lua-script-error:persona matcher result".to_owned(),
                StageError::Limit(_)=>"lua-host-limit".to_owned(),
                error=>format!("lua-persona: {error:?}"),
            })?;
        Ok(Some(Value::Text(result)))
}

struct OverlayRecords<'a> { snapshot: &'a ConversationSnapshot, state: &'a State, budget: &'a Budget }

impl RecordValues for OverlayRecords<'_> {
    fn field(&self, record: &str, field: &str) -> Result<Option<Value>, String> {
        let (changes, record) = match record {
            "character" => (&self.state.character_fields, &self.snapshot.character),
            "conversation" => (&self.state.chat_fields, &self.snapshot.conversation),
            "root" => return self.snapshot.root.get(field).map(|value| ir_value(value, self.budget)).transpose(),
            _ => return Err("lua-view-record".into()),
        };
        match changes.iter().find(|(name, _)| name == field) {
            Some((_, value)) => copy_value(value, self.budget).map(Some).map_err(ir_error),
            None => record.get(field).map(|value| ir_value(value, self.budget)).transpose(),
        }
    }
}

impl Conversation for Overlay {
    fn bind_variable_budget(&self, budget: &Budget) -> Result<(), String> {
        *self.budget.borrow_mut() = budget.clone();
        if let Some(raw) = self.state.borrow_mut().raw.as_mut() { raw.rebind_budget(budget); }
        Ok(())
    }
    fn begin(&self) -> Result<(), String> { Ok(()) }

    fn end(&self) -> Result<(), String> { Ok(()) }

    // `getVar` (`triggers.ts:1338-1343`) then `ScriptConversationAccess.getVariable`.
    fn variable(&self, key: &str) -> Result<Option<Scalar>, String> { Ok(given(&self.variable_value(key)?)) }

    // `setVar` (`triggers.ts:1361-1373`), whose local write is `setLocalVar`, then `ScriptConversationAccess.setVariable`.
    fn set_variable(&self, key: &str, value: &Assigned) -> Result<bool, String> {
        self.set_raw_variable(super::lua_variables::string_key(key, &self.budget())?, value.clone(), &mut |_| Ok(()))
    }

    fn variable_input(&self, key: &str, limits: ValueLimits) -> Result<VariableValue, String> {
        self.raw_variable_input(&super::lua_variables::string_key(key, &self.budget())?, limits)
    }

    fn raw_variable_input(&self, key: &VariableValue, limits: ValueLimits) -> Result<VariableValue, String> {
        let mut state = self.state.borrow_mut();
        let State { raw, stored, defaults, defaults_charge, chat_fields, character_fields, .. } = &mut *state;
        let raw = raw.as_ref().ok_or("variable scope missing")?;
        let mut reader = OverlayVariables { snapshot: &self.snapshot, budget: &self.budget(), stored, defaults,
            defaults_charge, chat_fields, character_fields };
        raw.input(key, limits, &mut reader)
    }

    fn set_raw_variable(&self, key: VariableValue, value: Assigned,
        prepare: &mut dyn FnMut(&RawOperation) -> Result<(), String>) -> Result<bool, String> {
        let mut state = self.state.borrow_mut();
        let State { raw, stored, defaults, defaults_charge, chat_fields, character_fields, .. } = &mut *state;
        let raw = raw.as_mut().ok_or("variable scope missing")?;
        let mut reader = OverlayVariables { snapshot: &self.snapshot, budget: &self.budget(), stored, defaults,
            defaults_charge, chat_fields, character_fields };
        let limits = risunest_native_lua_experimental::Limits::default().values;
        let prepared = raw.prepare(key, value, false, |name| assigned_from_ir(&reader.stored(name)?, limits, &self.budget()))?;
        let Some(prepared) = prepared else { return Ok(false); };
        prepare(prepared.operation())?;
        raw.publish(prepared);
        Ok(true)
    }

    fn open_variable_scope(&self, kind: ScopeKind) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        let raw = state.raw.as_mut().ok_or("variable scope missing")?;
        match kind { ScopeKind::None => {}, ScopeKind::Edits => raw.open_edits(), ScopeKind::Operation => raw.open_operation() }
        Ok(())
    }

    fn legacy_variable_effects_only(&self) { self.legacy_effects_only(); }

    fn finish_variables(&self, finish: VariableFinish) -> Result<Vec<Effect>, String> {
        let mut state = self.state.borrow_mut();
        let raw = state.raw.as_ref().ok_or("variable scope missing")?;
        let selected = raw.select(finish, |entry| {
            let Some(name) = entry.name.as_unicode() else { return false; };
            match self.stored_variable(&state, name) {
                StoredVariable::Missing => matches_ir(&entry.operation.value, None),
                StoredVariable::Value(value) => matches_ir(&entry.operation.value, Some(value)),
                StoredVariable::Json(value) => matches_json(&entry.operation.value, Some(value)),
            }
        });
        let effects = state.raw.as_mut().ok_or("variable scope missing")?.finish(&selected, true, finish)?;
        match finish {
            VariableFinish::Variables => {},
            VariableFinish::Finish => {
                if let Some(rows) = state.rows.take() {
                    let charge = state.rows_charge.take().ok_or("lua-view-rows")?;
                    state.committed_rows = Some((rows, charge));
                }
            },
            VariableFinish::WithoutMessages | VariableFinish::Failure => {
                if state.rows.take().is_some() {
                    state.rows_charge = None;
                    state.history_generation = state.history_generation.wrapping_add(1);
                }
            },
        }
        Ok(effects)
    }

    fn record_field(&self, record: &str, field: &str) -> Result<Option<Json>, String> {
        let state = self.state.borrow();
        let (changes, record) = match record {
            "character" => (&state.character_fields, &self.snapshot.character),
            "conversation" => (&state.chat_fields, &self.snapshot.conversation),
            "root" => return Ok(self.snapshot.root.get(field).cloned()),
            _ => return Err("lua-view-record".into()),
        };
        match changes.iter().find(|(name, _)| name == field) {
            Some((_, value)) => stored(value),
            None => Ok(record.get(field).cloned()),
        }
    }

    fn read_records(&self, budget: &Budget, read: &mut dyn FnMut(&dyn RecordValues) -> Result<(), String>) -> Result<(), String> {
        let state = self.state.borrow();
        read(&OverlayRecords { snapshot: &self.snapshot, state: &state, budget })
    }

    fn refresh_persona_history(&self, processor: &PersonaProcessor, budget: &Budget) -> Result<(), String> {
        let state = self.state.borrow();
        let generation = state.history_generation;
        if generation == self.processor_generation.get() { return Ok(()); }
        let mut messages = OwnedVec::new(budget).map_err(ir_error)?;
        for index in 0..self.len(&state) { messages.push(ir_value(self.row(&state, index), budget)?).map_err(ir_error)?; }
        drop(state);
        processor.borrow_mut().apply(&Effect::Message(MessageEdit::Replace { messages: Value::Array(messages) }), budget)
            .map_err(|error| format!("lua-persona: {error:?}"))?;
        self.processor_generation.set(generation);
        Ok(())
    }

    fn persona_description(&self, _effects: &[Effect]) -> Result<Option<Value>, String> {
        process_persona(self, self.processor.as_ref(), &self.budget())
    }

    fn cut(&self, start: f64, end: Option<f64>) -> Result<(), String> {
        self.open_variable_scope(ScopeKind::Operation)?;
        self.apply(&mut self.state.borrow_mut(), &MessageEdit::Cut { start, end })
    }

    fn length(&self) -> Result<i64, String> {
        i64::try_from(self.len(&self.state.borrow())).map_err(|_| "history length exceeds the index range".into())
    }

    fn message_field(&self, index: f64, field: &str) -> Result<Option<Json>, String> {
        let state = self.state.borrow();
        Ok(at_index(index, self.len(&state)).and_then(|at| self.row(&state, at).get(field).cloned()))
    }

    fn message_value(&self, index: f64, field: &str, budget: &Budget) -> Result<Option<Value>, String> {
        let state = self.state.borrow();
        at_index(index, self.len(&state)).and_then(|at| self.row(&state, at).get(field)).map(|value| ir_value(value, budget)).transpose()
    }

    // `ScriptConversationAccess.readChat` over the whole history, which an overlay never windows.
    fn history(&self, selection: HistorySelection, full: bool, limits: ValueLimits) -> Result<HistoryText, String> {
        let state = self.state.borrow();
        let total = self.len(&state);
        let result = match selection {
            HistorySelection::One(NumberArgument::Invalid) | HistorySelection::Recent(NumberArgument::Invalid) =>
                return Err("script access: HistoryCoercion".into()),
            HistorySelection::One(NumberArgument::Value(index)) =>
                project_rows(Rows::One(at_index(index, total).map(|at| self.row(&state, at))), full, limits, &self.budget()),
            HistorySelection::Recent(NumberArgument::Value(count)) => {
                // The count already holds JS max(0, floor(count || 0)); NaN reaches slice as zero.
                let start = if count.is_nan() || count >= total as f64 { 0 } else if count <= 0.0 { total } else { total - count as usize };
                let mut rows = (start..total).map(|at| self.row(&state, at));
                project_rows(Rows::Many(&mut rows), full, limits, &self.budget())
            }
            HistorySelection::FullExposed => {
                let mut rows = (0..total).map(|at| self.row(&state, at));
                project_rows(Rows::Many(&mut rows), full, limits, &self.budget())
            }
        };
        result.map_err(|error| match error {
            super::access::Error::Limit(_) => "lua-host-limit".into(),
            error => format!("script access: {error:?}"),
        })
    }

    fn set_message(&self, index: f64, field: &'static str, value: String) -> Result<(), String> {
        self.open_variable_scope(ScopeKind::Edits)?;
        self.set_field(&mut self.state.borrow_mut(), index, field, Some(Json::String(value)))
    }

    fn remove(&self, index: f64) -> Result<(), String> {
        self.open_variable_scope(ScopeKind::Operation)?;
        self.remove_at(&mut self.state.borrow_mut(), index)
    }

    fn insert(&self, index: f64, message: Json) -> Result<(), String> {
        self.open_variable_scope(ScopeKind::Operation)?;
        self.insert_at(&mut self.state.borrow_mut(), index, message)
    }

    fn append(&self, message: Json) -> Result<(), String> {
        self.open_variable_scope(ScopeKind::Edits)?;
        let row = self.owned(message)?;
        self.restructure(&mut self.state.borrow_mut(), 1, |rows| { rows.push(row); Ok(()) })
    }

    // A stored id is kept for as many messages as the history holds it, and a message keeps its stored fields when its id
    // names one message on both sides (`setFullChatMain`, `scriptings.ts:600-633`).
    fn replace_prepared(&self, messages: Vec<FullMessage>, prepare: &mut dyn FnMut(&[Json]) -> Result<(), String>) -> Result<Vec<Json>, String> {
        self.open_variable_scope(ScopeKind::Operation)?;
        let mut state = self.state.borrow_mut();
        let mut counts = HashMap::<&str, usize>::new();
        for id in messages.iter().filter_map(|message| message.chat_id.as_deref()) { *counts.entry(id).or_default() += 1; }
        // For each id the input names: how often the history holds it, and its message while it holds it once.
        let mut originals = HashMap::<&str, (usize, Option<Map<String, Json>>)>::new();
        for at in 0..self.len(&state) {
            let Json::Object(row) = self.row(&state, at) else { continue };
            let Some(Json::String(id)) = row.get("chatId") else { continue };
            let Some((&id, _)) = counts.get_key_value(id.as_str()).filter(|(id, _)| !id.is_empty()) else { continue };
            originals.entry(id).and_modify(|entry| *entry = (entry.0 + 1, None)).or_insert_with(|| (1, Some(row.clone())));
        }
        let mut kept = HashMap::<&str, usize>::new();
        let mut rows = Vec::new();
        for message in &messages {
            let held = message.chat_id.as_deref().and_then(|id| originals.get(id).map(|entry| (id, entry)))
                .filter(|(id, (count, _))| kept.get(id).copied().unwrap_or(0) < *count);
            let (mut row, id) = match held {
                Some((id, (_, original))) => {
                    *kept.entry(id).or_default() += 1;
                    (original.as_ref().filter(|_| counts.get(id) == Some(&1)).cloned().unwrap_or_default(), id.to_owned())
                }
                None => (Map::new(), uuid::Uuid::new_v4().to_string()),
            };
            row.insert("role".into(), Json::String(message.role.clone()));
            row.insert("data".into(), Json::String(message.data.clone()));
            row.insert("chatId".into(), Json::String(id));
            rows.push(Json::Object(row));
        }
        prepare(&rows)?;
        let owned = rows.iter().map(|row| self.owned(row.clone())).collect::<Result<Vec<_>, _>>()?;
        self.assign_rows(&mut state, owned)?;
        Ok(rows)
    }
}

#[cfg(test)]
pub(super) use store::StoreSource;

#[cfg(test)]
mod store {
    use super::super::access::{Error as AccessError, InvocationPermit, ScriptStoreAccess};
    use super::super::lua_values::project;
    use super::*;
    use crate::persistent_store::PersistentStore;
    use std::rc::Rc;

    fn failure(error: AccessError) -> String { format!("script access: {error:?}") }

    /// The store path: reads and writes through a `ScriptStoreAccess` scope, one invocation permit at a time.
    pub(in super::super) struct StoreSource {
        access: Rc<ScriptStoreAccess>,
        store: Rc<RefCell<PersistentStore>>,
        permit: RefCell<Option<InvocationPermit>>,
        budget: RefCell<Budget>,
        configured: Cell<bool>,
        policy: BindingPolicy,
        locals: Vec<(Text, Assigned)>,
        _locals_charge: Option<Charge>,
        processor: Option<PersonaProcessor>,
        processor_version: RefCell<Option<super::super::access::ReadResult<super::super::access::PersonaVersion>>>,
    }

    impl StoreSource {
        fn budget(&self) -> Budget { self.budget.borrow().clone() }
        pub(in super::super) fn new(access: Rc<ScriptStoreAccess>, store: Rc<RefCell<PersistentStore>>, budget: Budget) -> Self {
            Self { access, store, permit: RefCell::new(None), budget: RefCell::new(budget), configured: Cell::new(false), policy: BindingPolicy::AccessScope,
                locals: Vec::new(), _locals_charge: None, processor: None, processor_version: RefCell::new(None) }
        }

        pub(in super::super) fn with_processor(mut self, processor: PersonaProcessor) -> Result<Self, String> {
            let version = {
                let store = self.store.try_borrow().map_err(|_| "Lua store is busy".to_owned())?;
                self.access.capture_persona_version(&store, &self.budget()).map_err(failure)?
            };
            *self.processor_version.borrow_mut() = Some(version);
            self.processor = Some(processor);
            Ok(self)
        }

        pub(in super::super) fn with_binding_policy(mut self, policy: BindingPolicy) -> Self { self.policy = policy; self }

        pub(in super::super) fn with_locals(mut self, locals: &[(Text, Value)]) -> Result<Self, String> {
            let mut values = Vec::new();
            let limits = risunest_native_lua_experimental::Limits::default().values;
            let bytes = locals.len().saturating_mul(4 * std::mem::size_of::<(Text, Assigned)>());
            self._locals_charge = Some(self.budget().reserve_owned(bytes).map_err(ir_error)?);
            for (name, value) in locals {
                values.push((name.copy(&self.budget()).map_err(ir_error)?, assigned_from_ir(value, limits, &self.budget())?));
            }
            self.locals = values;
            Ok(self)
        }

        pub(in super::super) fn open(&self, already_aborted: bool) -> Result<(), AccessError> {
            let permit = self.access.begin_invocation(already_aborted)?;
            if let Err(error) = self.access.rebind_raw_budget(&permit, &self.budget()) {
                let _ = self.access.end_invocation(&permit); return Err(error);
            }
            if !self.configured.get() {
                let result = self.access.configure_raw_variables(&permit, &self.budget(), self.policy, &self.locals);
                match result {
                    Ok(Ok(())) => self.configured.set(true),
                    Ok(Err(_)) => { let _ = self.access.end_invocation(&permit); return Err(AccessError::Limit("raw-variable-preparation")); }
                    Err(error) => { let _ = self.access.end_invocation(&permit); return Err(error); }
                }
            }
            *self.permit.borrow_mut() = Some(permit);
            Ok(())
        }

        /// Ends the invocation; whether its owner aborted it.
        pub(in super::super) fn close(&self) -> Result<bool, AccessError> {
            match self.permit.borrow_mut().take() { Some(permit) => self.access.end_invocation(&permit), None => Ok(false) }
        }

        pub(in super::super) fn abort(&self) -> Result<(), AccessError> {
            match self.permit.borrow().as_ref() { Some(permit) => self.access.abort_invocation(permit), None => Ok(()) }
        }

        fn with<T>(&self, run: impl FnOnce(&ScriptStoreAccess, &PersistentStore, &InvocationPermit) -> Result<T, AccessError>) -> Result<T, String> {
            let permit = self.permit.borrow();
            let permit = permit.as_ref().ok_or_else(|| "Lua invocation ended".to_owned())?;
            let store = self.store.try_borrow().map_err(|_| "Lua store is busy".to_owned())?;
            run(&self.access, &store, permit).map_err(failure)
        }
    }

    struct StoreRecords<'a, 'b> { read: &'a super::super::access::RecordRead<'b>, budget: &'a Budget }

    impl RecordValues for StoreRecords<'_, '_> {
        fn field(&self, record: &str, field: &str) -> Result<Option<Value>, String> {
            self.read.field(record, field, |value| value.map(|value| ir_value(value, self.budget)).transpose()).map_err(failure)?
        }
    }

    impl Conversation for StoreSource {
            fn bind_variable_budget(&self, budget: &Budget) -> Result<(), String> {
            *self.budget.borrow_mut() = budget.clone();
            Ok(())
        }
        fn begin(&self) -> Result<(), String> { self.open(false).map_err(failure) }

        fn end(&self) -> Result<(), String> { self.close().map(|_| ()).map_err(failure) }

        fn variable(&self, key: &str) -> Result<Option<Scalar>, String> {
            let limits = risunest_native_lua_experimental::Limits::default().values;
            let value = self.variable_input(key, limits)?;
            let value = assigned_from_ir_input(value, limits, &self.budget())?;
            Ok(given(&value.value(&self.budget()).map_err(ir_error)?))
        }

        fn set_variable(&self, key: &str, value: &Assigned) -> Result<bool, String> {
            self.set_raw_variable(super::super::lua_variables::string_key(key, &self.budget())?, value.clone(), &mut |_| Ok(()))
        }

        fn variable_input(&self, key: &str, limits: ValueLimits) -> Result<VariableValue, String> {
            self.raw_variable_input(&super::super::lua_variables::string_key(key, &self.budget())?, limits)
        }

        fn raw_variable_input(&self, key: &VariableValue, limits: ValueLimits) -> Result<VariableValue, String> {
            self.with(|access, store, permit| access.raw_variable_input(store, permit, key, limits))?
        }

        fn set_raw_variable(&self, key: VariableValue, value: Assigned,
            prepare: &mut dyn FnMut(&RawOperation) -> Result<(), String>) -> Result<bool, String> {
            self.with(|access, store, permit| access.set_raw_variable(store, permit, key, value, prepare))?
        }

        fn open_variable_scope(&self, kind: ScopeKind) -> Result<(), String> {
            self.with(|access, _, permit| access.open_raw_scope(permit, kind))
        }

        fn finish_variables(&self, finish: VariableFinish) -> Result<Vec<Effect>, String> {
            let id = self.access.next_segment().map_err(failure)?;
            let mut store = self.store.try_borrow_mut().map_err(|_| "Lua store is busy".to_owned())?;
            let outcome = self.access.commit_raw(&mut store, &id, finish).map_err(failure)?;
            if let Some(error) = outcome.error { return Err(failure(error)); }
            match outcome.status {
                super::super::access::SegmentStatus::Committed(_) | super::super::access::SegmentStatus::NoChange => Ok(Vec::new()),
                status => Err(format!("script access settlement: {status:?}")),
            }
        }

        fn record_field(&self, record: &str, field: &str) -> Result<Option<Json>, String> {
            self.with(|access, store, permit| access.read_record_field(store, permit, record, field)).map(|value| value.value().clone())
        }

        fn read_records(&self, budget: &Budget, read: &mut dyn FnMut(&dyn RecordValues) -> Result<(), String>) -> Result<(), String> {
            self.with(|access, store, permit| access.with_record_view(store, permit, |records| read(&StoreRecords { read: records, budget })))?
        }

        fn message_value(&self, index: f64, field: &str, budget: &Budget) -> Result<Option<Value>, String> {
            self.with(|access, store, permit| access.with_message_field(store, permit, index, field,
                |value| value.map(|value| ir_value(value, budget)).transpose()))?
        }

        fn refresh_persona_history(&self, processor: &PersonaProcessor, budget: &Budget) -> Result<(), String> {
            let update = {
                let previous = self.processor_version.borrow();
                let previous = previous.as_ref().ok_or("lua-persona-version")?;
                self.with(|access, store, permit| access.read_persona_history(store, permit, budget, previous.value()))??
            };
            let Some((version, messages)) = update else { return Ok(()); };
            processor.borrow_mut().apply(&Effect::Message(MessageEdit::Replace { messages }), budget)
                .map_err(|error| format!("lua-persona: {error:?}"))?;
            *self.processor_version.borrow_mut() = Some(version);
            Ok(())
        }

        fn persona_description(&self, _effects: &[Effect]) -> Result<Option<Value>, String> {
            process_persona(self, self.processor.as_ref(), &self.budget())
        }

        fn cut(&self, start: f64, end: Option<f64>) -> Result<(), String> {
            self.with(|access, store, permit| access.cut_messages(store, permit, start, end))
        }

        fn length(&self) -> Result<i64, String> { self.with(|access, store, permit| access.history_length(store, permit)) }

        fn message_field(&self, index: f64, field: &str) -> Result<Option<Json>, String> {
            let message = self.with(|access, store, permit| access.read_message(store, permit, index))?;
            Ok(message.value().as_ref().and_then(|row| row.get(field)).cloned())
        }

        fn history(&self, selection: HistorySelection, full: bool, limits: ValueLimits) -> Result<HistoryText, String> {
            let rows = self.with(|access, store, permit| access.read_history(store, permit, selection))?;
            project(&rows, full, limits).map_err(failure)
        }

        fn set_message(&self, index: f64, field: &'static str, value: String) -> Result<(), String> {
            self.with(|access, store, permit| match field {
                "data" => access.set_message_data(store, permit, index, value),
                _ => access.set_message_role(store, permit, index, &value),
            })
        }

        fn remove(&self, index: f64) -> Result<(), String> { self.with(|access, store, permit| access.remove_message(store, permit, index)) }

        fn insert(&self, index: f64, message: Json) -> Result<(), String> {
            self.with(|access, store, permit| access.insert_message(store, permit, index, message))
        }

        fn append(&self, message: Json) -> Result<(), String> {
            self.with(|access, store, permit| access.append_message(store, permit, message))
        }

        fn replace_prepared(&self, messages: Vec<FullMessage>, prepare: &mut dyn FnMut(&[Json]) -> Result<(), String>) -> Result<Vec<Json>, String> {
            let mut failure = None;
            let result = self.with(|access, store, permit| access.replace_messages_prepared(store, permit, messages, &mut |rows| {
                prepare(rows).map_err(|error| { failure = Some(error); AccessError::Limit("lua-edits") })
            }));
            match failure { Some(error) => Err(error), None => result }
        }
    }
}
