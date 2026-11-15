use super::{Budget, Conversion, Scalar, ScalarConversion, ValueLimits};
use mlua::{Function, Lua, MultiValue, Value};
use std::{cell::{Cell, RefCell}, rc::Rc, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryReader { One, Recent, Full }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumberArgument { Value(f64), Invalid }

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreparedHistory { One(NumberArgument), Recent(NumberArgument), Full }

#[derive(Clone, Debug)]
enum Js { Undefined, Null, Boolean(bool), Number(f64), Text(Vec<u16>), Reference(usize), Function }

#[derive(Clone, Debug)]
enum Prototype { Builtin, Null, Value(Js) }
#[derive(Clone, Debug)]
enum Node { Array(Vec<Js>), Object { own: Vec<(Vec<u16>, Js)>, prototype: Prototype } }

/// A bounded Wasmoon-converted argument. The existing graph preserves aliases and enumerable prototypes while
/// retained by the host; exporting own properties produces the tree the persistent JSON boundary writes.
#[derive(Clone)]
pub struct VariableValue { root: Js, graph: Arc<Vec<Node>>, limits: ValueLimits, lease:Option<Arc<dyn std::any::Any+Send+Sync>> }

impl std::fmt::Debug for VariableValue {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        f.debug_struct("VariableValue").field("root",&self.root).field("nodes",&self.graph.len()).finish()
    }
}

impl PartialEq for VariableValue {
    fn eq(&self, other: &Self) -> bool {
        match (&self.root, &other.root) {
            (Js::Undefined,Js::Undefined)|(Js::Null, Js::Null) => true,
            (Js::Boolean(a), Js::Boolean(b)) => a == b,
            (Js::Number(a), Js::Number(b)) => a == b,
            (Js::Text(a), Js::Text(b)) => a == b,
            (Js::Reference(a), Js::Reference(b)) => a == b && Arc::ptr_eq(&self.graph, &other.graph),
            _ => false,
        }
    }
}

pub enum ValueEvent<'a> { Undefined, Null, Boolean(bool), Number(f64), Text(&'a [u16]), Array(usize), Object(usize), Key(&'a [u16]), End }

pub trait ValueVisitor { fn visit(&mut self, event: ValueEvent<'_>) -> Result<(), String>; }

/// Property coercion streams through the caller's work and allocation budget, including temporary joins.
pub trait PropertyKeyVisitor: ValueVisitor {
    fn checkpoint(&mut self) -> Result<(), String>;
    fn reserve(&mut self, bytes: usize) -> Result<(), String>;
    fn number_units(&mut self, value: f64) -> Result<Vec<u16>, String>;
}

#[derive(Clone,Copy)]
enum Property<'a> { Value(&'a Js), Length(usize), ObjectText, ArrayText, ArrayJoin, ValueOf, Absent }

struct KeyConversion<'a,'b> {value:&'a VariableValue,visitor:&'b mut dyn PropertyKeyVisitor,nodes:usize,bytes:usize}

impl<'a,'b> KeyConversion<'a,'b> {
    fn tick(&mut self,depth:usize)->Result<(),String>{
        self.visitor.checkpoint()?;
        self.nodes=self.nodes.checked_add(1).filter(|n|*n<=self.value.limits.nodes).ok_or("variable key node limit exceeded")?;
        if depth>self.value.limits.depth{return Err("variable key depth limit exceeded".into());}
        Ok(())
    }
    fn reserve(&mut self,bytes:usize)->Result<(),String>{
        self.bytes=self.bytes.checked_add(bytes).filter(|n|*n<=self.value.limits.utf8_bytes).ok_or("variable key byte limit exceeded")?;
        self.visitor.reserve(bytes)
    }
    fn lookup(&mut self,mut id:usize,name:&str,depth:usize)->Result<Property<'a>,String>{
        for _ in 0..=self.value.graph.len(){
            self.tick(depth)?;
            match &self.value.graph[id]{
                Node::Object {own,prototype}=>{
                    for (key,value) in own{
                        self.tick(depth)?;
                        if key_is(key,name){return Ok(Property::Value(value));}
                    }
                    match prototype{
                        Prototype::Value(Js::Reference(next))=>id=*next,
                        Prototype::Value(Js::Function)=>return Err("unsupported variable key function".into()),
                        Prototype::Builtin=>return Ok(match name{"toString"=>Property::ObjectText,"valueOf"=>Property::ValueOf,_=>Property::Absent}),
                        _=>return Ok(Property::Absent),
                    }
                }
                Node::Array(items)=>{
                    if name=="length"{return Ok(Property::Length(items.len()));}
                    if let Ok(index)=name.parse::<usize>(){
                        if name.bytes().all(|value|value.is_ascii_digit())&&(name=="0"||!name.starts_with('0')){if let Some(value)=items.get(index){return Ok(Property::Value(value));}}
                    }
                    return Ok(match name{"toString"=>Property::ArrayText,"join"=>Property::ArrayJoin,"valueOf"=>Property::ValueOf,_=>Property::Absent});
                }
            }
        }
        Err("variable key prototype limit exceeded".into())
    }
    fn copy(&mut self,value:&Js)->Result<Js,String>{
        if let Js::Text(text)=value{self.reserve(text.len().saturating_mul(2))?;}
        Ok(value.clone())
    }
    fn text(&mut self,text:&str)->Result<Js,String>{
        self.reserve(text.len().saturating_mul(2))?;
        Ok(Js::Text(text.encode_utf16().collect()))
    }
    fn append(&mut self,output:&mut Vec<u16>,text:&[u16])->Result<(),String>{
        let length=output.len().checked_add(text.len()).ok_or("variable key byte limit exceeded")?;
        if length>output.capacity(){
            let capacity=output.capacity().saturating_mul(2).max(length).max(4);
            self.reserve(capacity.saturating_mul(2))?;
            output.reserve_exact(capacity-output.len());
        }
        output.extend_from_slice(text);
        Ok(())
    }
    fn units(&mut self,value:Js)->Result<Vec<u16>,String>{
        match value{
            Js::Text(text)=>Ok(text),
            Js::Undefined=>match self.text("undefined")?{Js::Text(text)=>Ok(text),_=>unreachable!()},
            Js::Null=>match self.text("null")?{Js::Text(text)=>Ok(text),_=>unreachable!()},
            Js::Boolean(value)=>match self.text(if value{"true"}else{"false"})?{Js::Text(text)=>Ok(text),_=>unreachable!()},
            Js::Number(value)=>{self.reserve(128)?;self.visitor.number_units(value)},
            _=>Err("unsupported variable key primitive".into()),
        }
    }
    fn number(&mut self,value:&Js,depth:usize)->Result<f64,String>{
        let primitive=self.primitive(value,false,depth+1)?;
        Ok(match primitive{
            Js::Undefined=>f64::NAN,Js::Null=>0.0,Js::Boolean(value)=>if value{1.0}else{0.0},
            Js::Number(value)=>value,Js::Text(text)=>{self.reserve(text.len())?;js_number(&text)},_=>return Err("unsupported variable key primitive".into()),
        })
    }
    fn join(&mut self,receiver:usize,depth:usize)->Result<Js,String>{
        self.tick(depth)?;
        match self.lookup(receiver,"join",depth)?{
            Property::ArrayJoin=>{},
            Property::Value(Js::Function)=>return Err("unsupported variable key function".into()),
            _=>return self.text(if matches!(self.value.graph[receiver],Node::Array(_)){"[object Array]"}else{"[object Object]"}),
        }
        let length=match self.lookup(receiver,"length",depth)?{
            Property::Length(length)=>length,
            Property::Value(value)=>{
                let value=self.copy(value)?;
                let number=self.number(&value,depth+1)?;
                if number.is_nan()||number<=0.0{0}else{number.floor().min(9_007_199_254_740_991.0) as usize}
            }
            Property::Absent=>0,
            _=>return Err("unsupported variable key length".into()),
        };
        let mut output=Vec::new();
        for index in 0..length{
            self.tick(depth)?;
            if index>0{self.append(&mut output,&[44])?;}
            self.reserve(32)?;
            let key=index.to_string();
            let value=match self.lookup(receiver,&key,depth)?{
                Property::Value(value)=>Some(self.copy(value)?),Property::Absent=>None,
                Property::Length(length)=>Some(Js::Number(length as f64)),
                _=>return Err("unsupported variable key element".into()),
            };
            if let Some(value)=value.filter(|value|!matches!(value,Js::Null|Js::Undefined)){
                let primitive=self.primitive(&value,true,depth+1)?;
                let text=self.units(primitive)?;
                self.append(&mut output,&text)?;
            }
        }
        Ok(Js::Text(output))
    }
    fn primitive(&mut self,value:&Js,string_hint:bool,depth:usize)->Result<Js,String>{
        self.tick(depth)?;
        let Js::Reference(id)=value else{
            if matches!(value,Js::Function){return Err("unsupported variable key function".into());}
            return self.copy(value);
        };
        for name in if string_hint{["toString","valueOf"]}else{["valueOf","toString"]}{
            match self.lookup(*id,name,depth)?{
                Property::ObjectText=>return self.text(if matches!(self.value.graph[*id],Node::Array(_)){"[object Array]"}else{"[object Object]"}),
                Property::ArrayText=>return self.join(*id,depth+1),
                Property::Value(Js::Function)=>return Err("unsupported variable key function".into()),
                _=>{},
            }
        }
        Err("lua-script-error:cannot convert data key to primitive".into())
    }
}

impl VariableValue {
    pub fn undefined(limits:ValueLimits)->Self {Self {root:Js::Undefined,graph:Arc::new(Vec::new()),limits,lease:None}}
    pub fn is_undefined(&self)->bool {matches!(self.root,Js::Undefined)}

    /// Map keys use SameValueZero; assignment equality still uses strict equality.
    pub fn same_value_zero(&self,other:&Self)->bool {
        self==other || matches!((&self.root,&other.root),(Js::Number(a),Js::Number(b)) if a.is_nan() && b.is_nan())
    }

    pub fn key_comparison_work(&self,other:&Self)->usize {
        match (&self.root,&other.root) {(Js::Text(a),Js::Text(b))=>a.len().min(b.len()).saturating_add(1),_=>1}
    }

    pub fn text_units(&self)->Option<&[u16]> {
        match &self.root {Js::Text(text)=>Some(text),_=>None}
    }

    /// Takes caller-admitted UTF-16 text without normalizing its code units.
    pub fn text(units:Vec<u16>,limits:ValueLimits)->Self {
        Self {root:Js::Text(units),graph:Arc::new(Vec::new()),limits,lease:None}
    }

    pub fn scalar(value: Scalar, limits: ValueLimits) -> Self {
        let root = match value { Scalar::Nil => Js::Null, Scalar::Boolean(v) => Js::Boolean(v), Scalar::Number(v) => Js::Number(v),
            Scalar::String(v) => Js::Text(v.encode_utf16().collect()) };
        Self { root, graph: Arc::new(Vec::new()), limits, lease:None }
    }

    pub fn as_scalar(&self) -> Option<Scalar> {
        Some(match &self.root { Js::Null => Scalar::Nil, Js::Boolean(v) => Scalar::Boolean(*v), Js::Number(v) => Scalar::Number(*v),
            Js::Text(v) => Scalar::String(String::from_utf16(v).ok()?), _ => return None })
    }

    pub fn retain(mut self,lease:Arc<dyn std::any::Any+Send+Sync>)->Self {self.lease=Some(lease);self}

    pub fn is_structured(&self) -> bool { matches!(self.root, Js::Reference(_)) }

    pub fn export_scratch_bound(&self)->usize {
        if !self.is_structured(){return 0;}
        4*self.graph.len().min(self.limits.depth.saturating_add(1)).max(1)*std::mem::size_of::<usize>()
    }

    pub fn visit_property_key(&self,visitor:&mut dyn PropertyKeyVisitor)->Result<(),String>{
        self.visit_key(visitor,true)
    }

    /// Storage concatenation uses the default primitive hint, separately from property lookup.
    pub fn visit_storage_key(&self,visitor:&mut dyn PropertyKeyVisitor)->Result<(),String>{
        self.visit_key(visitor,false)
    }

    fn visit_key(&self,visitor:&mut dyn PropertyKeyVisitor,string_hint:bool)->Result<(),String>{
        let mut conversion=KeyConversion {value:self,visitor,nodes:0,bytes:0};
        let primitive=conversion.primitive(&self.root,string_hint,0)?;
        conversion.visitor.visit(match &primitive{
            Js::Undefined=>ValueEvent::Undefined,Js::Null=>ValueEvent::Null,Js::Boolean(value)=>ValueEvent::Boolean(*value),
            Js::Number(value)=>ValueEvent::Number(*value),Js::Text(text)=>ValueEvent::Text(text),
            _=>return Err("unsupported variable key primitive".into()),
        })
    }

    /// Looks up one effective object property without flattening prototypes or changing the stored own-property tree.
    pub fn has_nonnull_property(&self,name:&str)->Result<bool,String>{
        self.has_nonnull_property_checked(name,||Ok(()))
    }

    pub fn has_nonnull_property_checked(&self,name:&str,mut checkpoint:impl FnMut()->Result<(),String>)->Result<bool,String>{
        let Js::Reference(mut id)=self.root else{return Ok(false)};
        for _ in 0..=self.graph.len(){
            checkpoint()?;
            let Node::Object {own,prototype}=&self.graph[id] else{return Ok(false)};
            for (key,value) in own{
                checkpoint()?;
                if key_is(key,name){return Ok(!matches!(value,Js::Null|Js::Undefined));}
            }
            match prototype{
                Prototype::Value(Js::Reference(next))=>id=*next,
                _=>return Ok(false),
            }
        }
        Err("variable prototype bound exceeded".into())
    }

    pub fn owned_bytes(&self) -> usize {
        let js = |value: &Js| match value { Js::Text(v) => v.capacity().saturating_mul(2), _ => 0 };
        std::mem::size_of::<Self>() + 2*std::mem::size_of::<usize>() + std::mem::size_of::<Vec<Node>>() + js(&self.root) + self.graph.capacity().saturating_mul(std::mem::size_of::<Node>())
            + self.graph.iter().map(|node| match node {
                Node::Array(values) => values.capacity().saturating_mul(std::mem::size_of::<Js>()) + values.iter().map(js).sum::<usize>(),
                Node::Object { own, .. } => own.capacity().saturating_mul(std::mem::size_of::<(Vec<u16>,Js)>())
                    + own.iter().map(|(key,value)| key.capacity().saturating_mul(2) + js(value)).sum::<usize>(),
            }).sum::<usize>()
    }

    fn shifted(value: &mut Js, offset: usize) { if let Js::Reference(id) = value { *id += offset; } }

    fn append(graph: &mut Vec<Node>, value: Self) -> Js {
        let offset = graph.len();
        let mut root = value.root;
        let mut nodes = Arc::try_unwrap(value.graph).unwrap_or_else(|shared| (*shared).clone());
        for node in &mut nodes {
            match node {
                Node::Array(items) => for value in items { Self::shifted(value, offset); },
                Node::Object { own, prototype } => {
                    for (_, value) in own { Self::shifted(value, offset); }
                    if let Prototype::Value(value) = prototype { Self::shifted(value, offset); }
                }
            }
        }
        Self::shifted(&mut root, offset);
        graph.extend(nodes);
        root
    }

    pub fn array(values: Vec<Self>, limits: ValueLimits) -> Self {
        let mut graph = Vec::new();
        let values = values.into_iter().map(|value| Self::append(&mut graph, value)).collect();
        let root = Js::Reference(graph.len());
        graph.push(Node::Array(values));
        Self { root, graph: Arc::new(graph), limits, lease:None }
    }

    pub fn object(values: Vec<(Vec<u16>, Self)>, limits: ValueLimits) -> Self {
        let mut graph = Vec::new();
        let own = values.into_iter().map(|(key,value)| (key,Self::append(&mut graph,value))).collect();
        let root = Js::Reference(graph.len());
        graph.push(Node::Object {own,prototype:Prototype::Builtin});
        Self { root, graph:Arc::new(graph), limits, lease:None }
    }

    pub fn visit_own(&self, visitor: &mut dyn ValueVisitor) -> Result<(), String> {
        fn visit(value:&VariableValue, current:&Js, visitor:&mut dyn ValueVisitor, path:&mut Vec<usize>, count:&mut usize, bytes:&mut usize,
            depth:usize) -> Result<(),String> {
            *count = count.checked_add(1).filter(|n| *n <= value.limits.nodes).ok_or("values export node limit exceeded")?;
            if depth > value.limits.depth { return Err("values export depth limit exceeded".into()); }
            match current {
                Js::Undefined=>visitor.visit(ValueEvent::Undefined),
                Js::Null => visitor.visit(ValueEvent::Null),
                Js::Boolean(v) => visitor.visit(ValueEvent::Boolean(*v)),
                Js::Number(v) => visitor.visit(ValueEvent::Number(*v)),
                Js::Text(v) => {
                    *bytes = bytes.checked_add(v.len().saturating_mul(2)).filter(|n| *n <= value.limits.utf8_bytes).ok_or("values export byte limit exceeded")?;
                    visitor.visit(ValueEvent::Text(v))
                }
                Js::Function => Err("unsupported values-profile function".into()),
                Js::Reference(id) => {
                    if path.contains(id) { return Err("unsupported values-profile cycle".into()); }
                    path.push(*id);
                    match &value.graph[*id] {
                        Node::Array(items) => {
                            visitor.visit(ValueEvent::Array(items.len()))?;
                            for item in items { visit(value,item,visitor,path,count,bytes,depth+1)?; }
                        }
                        Node::Object {own,..} => {
                            visitor.visit(ValueEvent::Object(own.len()))?;
                            for (key,item) in own {
                                *bytes = bytes.checked_add(key.len().saturating_mul(2)).filter(|n| *n <= value.limits.utf8_bytes).ok_or("values export byte limit exceeded")?;
                                visitor.visit(ValueEvent::Key(key))?;
                                visit(value,item,visitor,path,count,bytes,depth+1)?;
                            }
                        }
                    }
                    path.pop();
                    visitor.visit(ValueEvent::End)
                }
            }
        }
        visit(self,&self.root,visitor,&mut Vec::new(),&mut 0,&mut 0,0)
    }
}

pub(super) struct Input<'a> {
    lua: &'a Lua,
    control: Conversion<'a>,
    limits: ValueLimits,
    nodes: Cell<usize>,
    bytes: Cell<usize>,
    key_string: Option<Function>,
    graph: Vec<Node>,
    null:Option<mlua::AnyUserData>,
    reserve:Option<&'a dyn Fn(usize)->Result<Box<dyn std::any::Any>,String>>,
    leased:Cell<usize>,
    output_precharged:Cell<bool>,
    leases:RefCell<Vec<Box<dyn std::any::Any>>>,
}

#[derive(Debug)]
enum AdmissionFailure {Limit(&'static str),Quota(String),Unsupported(&'static str)}
impl std::fmt::Display for AdmissionFailure {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        match self{Self::Limit(v)|Self::Unsupported(v)=>f.write_str(v),Self::Quota(v)=>f.write_str(v)}
    }
}
impl std::error::Error for AdmissionFailure {}
fn limit(message:&'static str)->mlua::Error{mlua::Error::external(AdmissionFailure::Limit(message))}
fn admission(error:&mlua::Error)->Option<&AdmissionFailure>{
    match error{
        mlua::Error::ExternalError(error)=>error.downcast_ref::<AdmissionFailure>(),
        mlua::Error::CallbackError {cause,..}|mlua::Error::WithContext {cause,..}|mlua::Error::BadArgument {cause,..}=>admission(cause),
        _=>None,
    }
}
pub(super) fn quota_label(error:&mlua::Error)->Option<&str>{match admission(error){Some(AdmissionFailure::Quota(v))=>Some(v),_=>None}}
pub(super) fn is_limit(error:&mlua::Error)->bool{matches!(admission(error),Some(AdmissionFailure::Limit(_)))}
pub(super) fn is_unsupported(error:&mlua::Error)->bool{matches!(admission(error),Some(AdmissionFailure::Unsupported(_)))}

fn error(message: &'static str) -> mlua::Error { mlua::Error::runtime(message) }
fn key_is(key: &[u16], text: &str) -> bool { key.iter().copied().eq(text.encode_utf16()) }

impl<'a> Input<'a> {
    pub(super) fn new(lua: &'a Lua, budget: &'a Rc<RefCell<Option<Budget>>>, limits: ValueLimits, key_string: Option<Function>) -> Self {
        Self { lua, control: Conversion::new(lua, budget, limits), limits, nodes: Cell::new(0),
            bytes: Cell::new(0), key_string, graph: Vec::new(),null:None,reserve:None,leased:Cell::new(0),output_precharged:Cell::new(false),leases:RefCell::new(Vec::new()) }
    }

    pub(super) fn with_null(mut self,null:Option<mlua::AnyUserData>)->Self{self.null=null;self}

    pub(super) fn with_reserver(mut self,reserve:&'a dyn Fn(usize)->Result<Box<dyn std::any::Any>,String>)->Self{self.reserve=Some(reserve);self}

    /// The caller holds the complete output scratch lease until encoding returns.
    pub(super) fn precharged_output(&self){self.output_precharged.set(true);}

    pub(super) fn check(&self) -> mlua::Result<()> { self.control.check() }

    fn charge(&self, bytes: usize) -> mlua::Result<()> {
        self.check()?;
        let next=self.bytes.get().checked_add(bytes).filter(|bytes|*bytes<=self.limits.utf8_bytes)
            .ok_or_else(||limit("values input byte limit exceeded"))?;
        if let Some(reserve)=self.reserve.filter(|_|!self.output_precharged.get()) {
            if next>self.leased.get(){
                let target=next.checked_add(4095).map(|v|v/4096*4096).ok_or_else(||limit("values input byte limit exceeded"))?;
                let overhead=4*std::mem::size_of::<Box<dyn std::any::Any>>();
                let lease=reserve(target-self.leased.get()+overhead).map_err(|label|mlua::Error::external(AdmissionFailure::Quota(label)))?;
                self.leases.borrow_mut().push(lease);
                self.leased.set(target);
            }
        }
        self.bytes.set(next);
        Ok(())
    }

    fn node(&self, depth: usize) -> mlua::Result<()> {
        self.check()?;
        if depth > self.limits.depth { return Err(limit("values input depth limit exceeded")); }
        self.nodes.set(self.nodes.get().checked_add(1).filter(|nodes| *nodes <= self.limits.nodes)
            .ok_or_else(|| limit("values input node limit exceeded"))?);
        self.charge(std::mem::size_of::<Js>())
    }

    fn text(&self, bytes: &[u8]) -> mlua::Result<Vec<u16>> {
        // Monotonic admission includes vector capacity and overlapping UTF-8/UTF-16 copies.
        self.charge(bytes.len().checked_mul(8).and_then(|n| n.checked_add(64)).ok_or_else(|| limit("values input byte limit exceeded"))?)?;
        let result = wasmoon_text(bytes)?;
        self.check()?;
        Ok(result)
    }

    pub(super) fn scalar(&self, value: Value) -> mlua::Result<Scalar> {
        self.node(0)?;
        Ok(match value {
            Value::Nil => Scalar::Nil,
            Value::UserData(value) if self.null.as_ref().is_some_and(|null|null.to_pointer()==value.to_pointer())=>Scalar::Nil,
            Value::Boolean(value) => Scalar::Boolean(value),
            Value::Integer(value) => Scalar::Number(value as f64),
            Value::Number(value) => Scalar::Number(value),
            Value::String(value) => Scalar::String(String::from_utf16(&self.text(&value.as_bytes())?)
                .map_err(|_| error("values compatibility: unpaired UTF-16 store string"))?),
            _ => return Err(error("unsupported scalar value")),
        })
    }

    pub(super) fn encode_scalar(&self, value: &Scalar, budget: &'a Rc<RefCell<Option<Budget>>>) -> mlua::Result<Value> {
        self.check()?;
        match value {
            Scalar::Nil if self.null.is_some()=>Ok(Value::UserData(self.null.as_ref().ok_or_else(||error("missing null value"))?.clone())),
            Scalar::String(text) => {
                self.node(0)?;
                self.charge(text.len())?;
                let prefix = text.split('\0').next().unwrap_or("");
                Ok(Value::String(self.lua.create_string(prefix.as_bytes())?))
            }
            _ => ScalarConversion::new(self.lua, budget, self.limits).encode(value),
        }
    }

    fn stringify_key(&self, key: Value) -> mlua::Result<Vec<u16>> {
        self.node(0)?;
        let value: mlua::LuaString = self.key_string.as_ref().ok_or_else(|| error("missing key converter"))?.call(key)?;
        self.text(&value.as_bytes())
    }

    fn convert(&mut self, value: Value, depth: usize, seen: &mut Vec<(usize, usize)>) -> mlua::Result<Js> {
        self.node(depth)?;
        Ok(match value {
            Value::Nil => Js::Null,
            Value::Boolean(value) => Js::Boolean(value),
            Value::Integer(value) => Js::Number(value as f64),
            Value::Number(value) => Js::Number(value),
            Value::String(value) => Js::Text(self.text(&value.as_bytes())?),
            Value::Function(_) => Js::Function,
            Value::UserData(value) if self.null.as_ref().is_some_and(|null|null.to_pointer()==value.to_pointer())=>Js::Null,
            Value::Table(table) => {
                if let Some(meta) = table.metatable() {
                    match meta.raw_get::<Value>("__name")? {
                        Value::Nil => (),
                        Value::String(name) => { self.text(&name.as_bytes())?; }
                        _ => (),
                    }
                }
                let identity = table.to_pointer() as usize;
                if let Some((_, id)) = seen.iter().find(|(pointer, _)| *pointer == identity) { return Ok(Js::Reference(*id)); }
                let mut sequential = true;
                let mut count = 0_usize;
                // Wasmoon classifies before registering the table in its seen map.
                for pair in table.clone().pairs::<Value, Value>() {
                    let (key, _) = pair?;
                    count = count.checked_add(1).ok_or_else(|| limit("values input node limit exceeded"))?;
                    let key = self.stringify_key(key)?;
                    self.charge(64)?;
                    sequential &= key_is(&key, &count.to_string());
                }
                self.charge(4 * (std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>()))?;
                let id = self.graph.len();
                self.graph.push(if sequential && count > 0 { Node::Array(Vec::new()) }
                    else { Node::Object { own: Vec::new(), prototype: Prototype::Builtin } });
                seen.push((identity, id));
                // A second live traversal is observable through key __tostring.
                for pair in table.pairs::<Value, Value>() {
                    let (key, value) = pair?;
                    let key = self.stringify_key(key)?;
                    let value = self.convert(value, depth + 1, seen)?;
                    self.assign(id, key, value)?;
                }
                Js::Reference(id)
            }
            // This profile creates no JS proxy, promise, pointer or thread capabilities.
            _ => return Err(mlua::Error::external(AdmissionFailure::Unsupported("unsupported values-profile host capability"))),
        })
    }

    fn proto_setter(&self, id: usize, key: &[u16]) -> mlua::Result<bool> {
        let Node::Object { own, prototype } = &self.graph[id] else { return Ok(false); };
        if own.iter().any(|(name, _)| name == key) { return Ok(false); }
        let mut prototype = prototype;
        for _ in 0..=self.graph.len() {
            self.check()?;
            match prototype {
                Prototype::Builtin => return Ok(key_is(key, "__proto__")),
                Prototype::Null => return Ok(false),
                Prototype::Value(Js::Function) => {
                    if ["name", "length", "caller", "arguments"].iter().any(|name| key_is(key, name)) {
                        return Err(error("cannot assign inherited function property"));
                    }
                    return Ok(key_is(key, "__proto__"));
                }
                Prototype::Value(Js::Reference(next)) => match &self.graph[*next] {
                    Node::Object { own, prototype: next } => {
                        if own.iter().any(|(name, _)| name == key) { return Ok(false); }
                        prototype = next;
                    }
                    Node::Array(_) => return Ok(key_is(key, "__proto__")),
                },
                Prototype::Value(_) => return Ok(false),
            }
        }
        Err(error("cyclic object prototype"))
    }

    fn assign(&mut self, id: usize, key: Vec<u16>, value: Js) -> mlua::Result<()> {
        if matches!(&self.graph[id], Node::Array(_)) {
            self.charge(4 * std::mem::size_of::<Js>())?;
            if let Node::Array(items) = &mut self.graph[id] { items.push(value); }
            return Ok(());
        }
        if self.proto_setter(id, &key)? {
            let next = match &value {
                Js::Null => Prototype::Null,
                Js::Reference(_) | Js::Function => Prototype::Value(value.clone()),
                _ => return Ok(()),
            };
            let mut cursor = &next;
            for _ in 0..=self.graph.len() {
                self.check()?;
                match cursor {
                    Prototype::Value(Js::Reference(next)) => {
                        if *next == id { return Err(error("cyclic object prototype")); }
                        if let Node::Object { prototype, .. } = &self.graph[*next] { cursor = prototype; } else { break; }
                    }
                    _ => break,
                }
            }
            if let Node::Object { prototype, .. } = &mut self.graph[id] { *prototype = next; }
            return Ok(());
        }
        self.charge(4 * std::mem::size_of::<(Vec<u16>, Js)>())?;
        if let Node::Object { own, .. } = &mut self.graph[id] {
            if let Some((_, previous)) = own.iter_mut().find(|(name, _)| name == &key) { *previous = value; }
            else { own.push((key, value)); }
        }
        Ok(())
    }

    fn cloneable(&self, value: &Js, seen: &mut Vec<bool>, depth: usize) -> mlua::Result<()> {
        self.node(depth)?;
        match value {
            Js::Function => Err(error("values input could not be cloned")),
            Js::Reference(id) if !seen[*id] => {
                seen[*id] = true;
                match &self.graph[*id] {
                    Node::Array(items) => for value in items { self.cloneable(value, seen, depth + 1)?; },
                    Node::Object { own, .. } => for (_, value) in own { self.cloneable(value, seen, depth + 1)?; },
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn truthy(value: &Js) -> bool {
        match value { Js::Undefined|Js::Null => false, Js::Boolean(value) => *value,
            Js::Number(value) => *value != 0.0 && !value.is_nan(), Js::Text(value) => !value.is_empty(), _ => true }
    }

    fn append_text(&self, output: &mut Vec<u16>, text: &[u16]) -> mlua::Result<()> {
        self.charge(text.len().checked_mul(4).and_then(|n| n.checked_add(16)).ok_or_else(|| limit("values input byte limit exceeded"))?)?;
        output.extend_from_slice(text);
        Ok(())
    }

    fn primitive_text(&self, value: &Js, output: &mut Vec<u16>, arrays: &mut Vec<usize>, depth: usize) -> mlua::Result<bool> {
        self.node(depth)?;
        match value {
            Js::Undefined=>self.append_text(output,&[117,110,100,101,102,105,110,101,100])?,
            Js::Null => self.append_text(output, &[110,117,108,108])?,
            Js::Boolean(value) => self.append_text(output, if *value { &[116,114,117,101] } else { &[102,97,108,115,101] })?,
            Js::Number(value) => {
                self.charge(2048)?;
                let text = if value.is_nan() { "NaN".into() } else if *value == f64::INFINITY { "Infinity".into() }
                    else if *value == f64::NEG_INFINITY { "-Infinity".into() } else if *value == 0.0 { "0".into() } else { value.to_string() };
                self.append_text(output, &text.encode_utf16().collect::<Vec<_>>())?;
            }
            Js::Text(value) => self.append_text(output, value)?,
            Js::Reference(id) => match &self.graph[*id] {
                Node::Object { own, .. } => {
                    if own.iter().any(|(key, _)| key_is(key, "toString")) { return Ok(false); }
                    self.append_text(output, &[91,111,98,106,101,99,116,32,79,98,106,101,99,116,93])?;
                }
                Node::Array(items) => {
                    if arrays.contains(id) { return Ok(true); }
                    self.charge(4 * std::mem::size_of::<usize>())?;
                    arrays.push(*id);
                    for (index, value) in items.iter().enumerate() {
                        if index > 0 { self.append_text(output, &[44])?; }
                        if !matches!(value, Js::Null) && !self.primitive_text(value, output, arrays, depth + 1)? { arrays.pop(); return Ok(false); }
                    }
                    arrays.pop();
                }
            },
            Js::Function => return Ok(false),
        }
        Ok(true)
    }

    fn number(&self, value: Option<&Js>) -> mlua::Result<NumberArgument> {
        let number = match value {
            None|Some(Js::Undefined) => f64::NAN,
            Some(Js::Null) => 0.0,
            Some(Js::Boolean(value)) => if *value { 1.0 } else { 0.0 },
            Some(Js::Number(value)) => *value,
            Some(Js::Text(value)) => { self.charge(value.len())?; js_number(value) },
            Some(value) => {
                let mut text = Vec::new();
                if !self.primitive_text(value, &mut text, &mut Vec::new(), 0)? { return Ok(NumberArgument::Invalid); }
                self.charge(text.len())?;
                js_number(&text)
            }
        };
        self.check()?;
        Ok(NumberArgument::Value(number))
    }

    pub(super) fn variables(&mut self, arguments: MultiValue) -> mlua::Result<Vec<VariableValue>> {
        let mut roots = Vec::new();
        for value in arguments {
            self.charge(4 * std::mem::size_of::<Js>())?;
            roots.push(self.convert(value, 0, &mut Vec::new())?);
        }
        self.check()?;
        let graph = Arc::new(std::mem::take(&mut self.graph));
        Ok(roots.into_iter().map(|root| VariableValue { root, graph: graph.clone(), limits: self.limits, lease:None }).collect())
    }

    pub(super) fn variable_scratch(&self,value:&VariableValue)->mlua::Result<usize> {
        let mut bytes=value.graph.len().checked_mul(4*std::mem::size_of::<Option<mlua::Table>>()).ok_or_else(||limit("values output scratch limit exceeded"))?;
        bytes=bytes.saturating_add(value.graph.len().min(self.limits.depth).saturating_mul(256)).saturating_add(512);
        let text_bytes=|v:&Js|match v{Js::Text(v)=>v.len().saturating_mul(8).saturating_add(64),_=>0};
        bytes=bytes.saturating_add(text_bytes(&value.root));
        for (id,node) in value.graph.iter().enumerate() {
            self.node(0)?;
            match node {
                Node::Array(items)=>for item in items{bytes=bytes.saturating_add(text_bytes(item));},
                Node::Object {..}=>{
                    let mut cursor=Some(id);
                    for _ in 0..=value.graph.len(){
                        self.node(0)?;
                        let Some(id)=cursor.take() else{break};
                        match &value.graph[id]{
                            Node::Object {own,prototype}=>{
                                for (key,item) in own{
                                    self.node(0)?;
                                    bytes=bytes.saturating_add(key.len().saturating_mul(8).saturating_add(192)).saturating_add(text_bytes(item));
                                }
                                if let Prototype::Value(Js::Reference(next))=prototype{cursor=Some(*next);}
                            }
                            Node::Array(items)=>bytes=bytes.saturating_add(items.len().saturating_mul(256)),
                        }
                    }
                }
            }
        }
        self.check()?;
        Ok(bytes)
    }

    pub(super) fn encode_variable(&self, value: &VariableValue, budget: &'a Rc<RefCell<Option<Budget>>>) -> mlua::Result<Value> {
        fn encode(input:&Input<'_>, value:&VariableValue, current:&Js, memo:&mut [Option<mlua::Table>], depth:usize,
            budget:&Rc<RefCell<Option<Budget>>>) -> mlua::Result<Value> {
            input.node(depth)?;
            Ok(match current {
                Js::Undefined=>Value::Nil,
                Js::Null => input.null.as_ref().map_or(Value::Nil,|null|Value::UserData(null.clone())),
                Js::Boolean(v) => Value::Boolean(*v),
                Js::Number(v) => ScalarConversion::new(input.lua,budget,input.limits).encode(&Scalar::Number(*v))?,
                Js::Text(v) => {
                    input.charge(v.len().saturating_mul(8).saturating_add(64))?;
                    Value::String(input.lua.create_string(super::js_string_bytes(v))?)
                }
                Js::Function => return Err(error("unsupported values-profile function")),
                Js::Reference(id) => {
                    if let Some(table) = &memo[*id] { return Ok(Value::Table(table.clone())); }
                    let table=input.lua.create_table()?;
                    memo[*id]=Some(table.clone());
                    match &value.graph[*id] {
                        Node::Array(items) => for (index,item) in items.iter().enumerate() {
                            table.raw_set(index+1,encode(input,value,item,memo,depth+1,budget)?)?;
                        },
                        Node::Object {..} => {
                            let mut cursor=Some(*id);
                            let mut seen=std::collections::BTreeSet::new();
                            for _ in 0..=value.graph.len() {
                                let Some(id)=cursor.take() else {break};
                                match &value.graph[id] {
                                    Node::Object {own,prototype} => {
                                        for (key,item) in own {
                                            input.node(depth)?;
                                            input.charge(key.len().saturating_mul(4).saturating_add(64))?;
                                            if seen.insert(key.clone()) {
                                                table.raw_set(input.lua.create_string(super::js_string_bytes(key))?,encode(input,value,item,memo,depth+1,budget)?)?;
                                            }
                                        }
                                        if let Prototype::Value(Js::Reference(next))=prototype {cursor=Some(*next);}
                                    }
                                    Node::Array(items) => for (index,item) in items.iter().enumerate() {
                                        let key=index.to_string().encode_utf16().collect::<Vec<_>>();
                                        input.node(depth)?;
                                        input.charge(key.len().saturating_mul(4).saturating_add(64))?;
                                        if seen.insert(key.clone()) {table.raw_set(input.lua.create_string(super::js_string_bytes(&key))?,encode(input,value,item,memo,depth+1,budget)?)?;}
                                    },
                                }
                            }
                        }
                    }
                    Value::Table(table)
                }
            })
        }
        self.charge(value.graph.len().saturating_mul(4*std::mem::size_of::<Option<mlua::Table>>()))?;
        let mut memo=vec![None;value.graph.len()];
        let result=encode(self,value,&value.root,&mut memo,0,budget)?;
        self.check()?;
        Ok(result)
    }

    pub(super) fn history(&mut self, arguments: MultiValue, reader: HistoryReader) -> mlua::Result<PreparedHistory> {
        let mut converted = Vec::new();
        for value in arguments {
            // getStackValues starts a new Wasmoon seen map for every argument.
            self.charge(4 * std::mem::size_of::<Js>())?;
            converted.push(self.convert(value, 0, &mut Vec::new())?);
        }
        self.charge(self.graph.len())?;
        let mut seen = vec![false; self.graph.len()];
        for value in &converted { self.cloneable(value, &mut seen, 0)?; }
        let result = match reader {
            HistoryReader::Full => PreparedHistory::Full,
            HistoryReader::One => PreparedHistory::One(self.number(converted.get(1))?),
            HistoryReader::Recent => {
                let number = if converted.get(1).is_none_or(|value| !Self::truthy(value)) { NumberArgument::Value(0.0) }
                    else { self.number(converted.get(1))? };
                PreparedHistory::Recent(match number {
                    NumberArgument::Value(value) if !value.is_nan() => NumberArgument::Value(value.floor().max(0.0)),
                    value => value,
                })
            }
        };
        self.check()?;
        Ok(result)
    }
}

/// A value `Input::scalar` converts as Wasmoon does: a primitive, or a string that reads into JS as Unicode text.
pub(super) fn scalar_like(value: &Value,null:Option<&mlua::AnyUserData>) -> bool {
    match value {
        Value::Nil | Value::Boolean(_) | Value::Integer(_) | Value::Number(_) => true,
        Value::UserData(value)=>null.is_some_and(|null|null.to_pointer()==value.to_pointer()),
        Value::String(text) => wasmoon_text(&text.as_bytes()).is_ok_and(|units| char::decode_utf16(units).all(|unit| unit.is_ok())),
        _ => false,
    }
}

// Only bytes owned by Lua, including its one trailing NUL, may be consulted.
pub(super) fn wasmoon_text(bytes: &[u8]) -> mlua::Result<Vec<u16>> {
    let end = bytes.iter().position(|value| *value == 0).unwrap_or(bytes.len());
    if end > 16 {
        let text = String::from_utf8_lossy(&bytes[..end]);
        return Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).encode_utf16().collect());
    }
    let mut output = Vec::new();
    let mut position = 0;
    while position < end {
        let first = bytes[position];
        let width = if first & 128 == 0 { 1 } else if first & 224 == 192 { 2 } else if first & 240 == 224 { 3 } else { 4 };
        if position + width - 1 > bytes.len() { return Err(mlua::Error::runtime(format!("values compatibility: unknown UTF-8 tail (lead {position}, required {}, owned through {})", position + width - 1, bytes.len()))); }
        let byte = |offset: usize| if offset == bytes.len() { 0 } else { bytes[offset] };
        let value = match width {
            1 => first as u32,
            2 => ((first & 31) as u32) << 6 | (byte(position + 1) & 63) as u32,
            3 => ((first & 15) as u32) << 12 | ((byte(position + 1) & 63) as u32) << 6 | (byte(position + 2) & 63) as u32,
            _ => ((first & 7) as u32) << 18 | ((byte(position + 1) & 63) as u32) << 12
                | ((byte(position + 2) & 63) as u32) << 6 | (byte(position + 3) & 63) as u32,
        };
        if value < 65536 { output.push(value as u16); }
        else { let value = value - 65536; output.push((55296 | value >> 10) as u16); output.push((56320 | value & 1023) as u16); }
        position += width;
    }
    Ok(output)
}

fn whitespace(value: u16) -> bool {
    matches!(value, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

fn js_number(value: &[u16]) -> f64 {
    let start = value.iter().position(|unit| !whitespace(*unit)).unwrap_or(value.len());
    let end = value.iter().rposition(|unit| !whitespace(*unit)).map_or(start, |index| index + 1);
    let value = &value[start..end];
    if value.is_empty() { return 0.0; }
    if value.iter().any(|unit| *unit > 127) { return f64::NAN; }
    let bytes = value.iter().map(|unit| *unit as u8).collect::<Vec<_>>();
    if bytes == b"Infinity" || bytes == b"+Infinity" { return f64::INFINITY; }
    if bytes == b"-Infinity" { return f64::NEG_INFINITY; }
    if bytes.len() >= 2 && bytes[0] == b'0' {
        let bits = match bytes[1] { b'x' | b'X' => 4, b'b' | b'B' => 1, b'o' | b'O' => 3, _ => 0 };
        if bits != 0 { return radix_number(&bytes[2..], bits); }
    }
    let mut index = usize::from(matches!(bytes[0], b'+' | b'-'));
    let mut digits = 0;
    while index < bytes.len() && bytes[index].is_ascii_digit() { digits += 1; index += 1; }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() { digits += 1; index += 1; }
    }
    if digits == 0 { return f64::NAN; }
    if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
        index += 1;
        if index < bytes.len() && matches!(bytes[index], b'+' | b'-') { index += 1; }
        let before = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() { index += 1; }
        if before == index { return f64::NAN; }
    }
    if index != bytes.len() { return f64::NAN; }
    std::str::from_utf8(&bytes).unwrap().parse::<f64>().unwrap_or(f64::NAN)
}

fn radix_number(bytes: &[u8], width: usize) -> f64 {
    if bytes.is_empty() { return f64::NAN; }
    let mut significand = 0_u64;
    let mut length = 0_usize;
    let mut guard = false;
    let mut sticky = false;
    for byte in bytes {
        let digit = match byte { b'0'..=b'9' => byte - b'0', b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10, _ => return f64::NAN };
        if digit as usize >= 1 << width { return f64::NAN; }
        for shift in (0..width).rev() {
            let bit = digit >> shift & 1;
            if length == 0 && bit == 0 { continue; }
            if length < 53 { significand = significand * 2 + bit as u64; }
            else if length == 53 { guard = bit != 0; }
            else { sticky |= bit != 0; }
            length += 1;
        }
    }
    if length == 0 { return 0.0; }
    if length <= 53 { return significand as f64; }
    if guard && (sticky || significand & 1 != 0) { significand += 1; }
    if length > 1024 { return f64::INFINITY; }
    significand as f64 * 2_f64.powi((length - 53) as i32)
}
