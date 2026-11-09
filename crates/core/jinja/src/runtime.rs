//! `runtime.ts`: the environment, `Interpreter.evaluate` and its helpers, the builtin filters, tests and globals.

use crate::ast::{parameter, special_arguments, Node, Special};
use crate::error::{runtime, too_deep, JinjaError};
use crate::units::{self, code_points, is_word, lossy, rc, s, Str};
use crate::value::{
    array_value, join_payload, loose_equals, object_value, payload_string, same_value_zero, strict_equals, to_json,
    to_string, Func, JsonOptions, Map, ObjMethod, StrMethod, V,
};
use crate::RenderOptions;
use risunest_ir::{Budget, Error, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

pub(crate) struct Scope<'a> {
    vars: RefCell<HashMap<Str, V<'a>>>,
    parent: Option<Env<'a>>,
}
pub(crate) type Env<'a> = Rc<Scope<'a>>;

impl<'a> Scope<'a> {
    /// The scope's variables, for [`V`]'s iterative drop.
    pub(crate) fn drain(&mut self) -> impl Iterator<Item = V<'a>> + '_ { self.vars.get_mut().drain().map(|(_, value)| value) }
}

fn new_env<'a>(parent: Option<&Env<'a>>) -> Env<'a> { Rc::new(Scope { vars: RefCell::new(HashMap::new()), parent: parent.cloned() }) }
fn set_var<'a>(env: &Env<'a>, name: &Str, value: V<'a>) { env.vars.borrow_mut().insert(name.clone(), value); }
fn is(name: &[u16], ascii: &str) -> bool { name.iter().copied().eq(ascii.encode_utf16()) }
fn func(value: Func<'_>) -> V<'_> { V::Func(Rc::new(value)) }

/// `lookupVariable`. Every JS `Environment` starts with its own `namespace`, so that name never resolves upward.
fn lookup<'a>(env: &Env<'a>, name: &[u16]) -> V<'a> {
    let mut scope = env.clone();
    loop {
        if let Some(value) = scope.vars.borrow().get(name) { return value.clone(); }
        if is(name, "namespace") { return func(Func::Namespace); }
        let Some(parent) = scope.parent.clone() else { return V::Undefined };
        scope = parent;
    }
}

pub(crate) enum Flow {
    Break,
    Continue,
    Error(JinjaError),
}
impl From<JinjaError> for Flow {
    fn from(value: JinjaError) -> Self { Flow::Error(value) }
}
impl From<Error> for Flow {
    fn from(value: Error) -> Self { Flow::Error(JinjaError::Budget(value)) }
}
type R<T> = Result<T, Flow>;

fn fail<T>(message: impl Into<String>) -> R<T> { Err(Flow::Error(runtime(message))) }
fn decline<T>(name: &'static str) -> R<T> { Err(Flow::Error(JinjaError::Unsupported(name))) }

struct Interp<'a, 'b> {
    budget: &'b Budget,
    options: &'b RenderOptions,
    depth: usize,
    limit: usize,
    /// Every namespace the render made. A namespace can hold itself, which `Rc` alone would leak.
    namespaces: Vec<Weak<RefCell<Map<'a>>>>,
}

/// `a ** b` where the port can prove what V8 answers. V8 computes `pow` itself, so a result that is not exactly
/// representable can differ in its last bit from the platform's `powf`; those are [`Power::Inexact`].
enum Power { Exact(f64), NotFinite, Inexact }

/// `|x| = odd * 2^exponent` for a finite, nonzero `x`.
fn split(x: f64) -> (u64, i64) {
    let bits = x.abs().to_bits();
    let biased = (bits >> 52) as i64;
    let fraction = bits & ((1 << 52) - 1);
    let (significand, exponent) = if biased == 0 { (fraction, -1074) } else { (fraction | 1 << 52, biased - 1075) };
    let shift = significand.trailing_zeros();
    (significand >> shift, exponent + i64::from(shift))
}

fn power(a: f64, b: f64) -> Power {
    // Number::exponentiate fixes NaN, zero and infinite operands exactly.
    if b.is_nan() { return Power::NotFinite; }
    if b == 0.0 { return Power::Exact(1.0); }
    if a.is_nan() { return Power::NotFinite; }
    if b.is_infinite() {
        let magnitude = a.abs();
        if magnitude == 1.0 || (magnitude > 1.0) == (b > 0.0) { return Power::NotFinite; }
        return Power::Exact(0.0);
    }
    let negative = a.is_sign_negative() && b.fract() == 0.0 && (b % 2.0).abs() == 1.0;
    let signed = |magnitude: f64| Power::Exact(if negative { -magnitude } else { magnitude });
    if a.is_infinite() { return if b > 0.0 { Power::NotFinite } else { signed(0.0) }; }
    // A zero base with a negative exponent threw before this.
    if a == 0.0 { return signed(0.0); }
    if a < 0.0 && b.fract() != 0.0 { return Power::NotFinite; }
    if b == 1.0 { return Power::Exact(a); }
    let (odd, exponent) = split(a);
    if b == 0.5 {
        let root = a.sqrt();
        let (root_odd, root_exponent) = split(root);
        if u128::from(root_odd) * u128::from(root_odd) == u128::from(odd) && 2 * root_exponent == exponent { return Power::Exact(root); }
    } else if b.fract() == 0.0 && (b > 0.0 && exponent >= 0 || b < 0.0 && odd == 1) {
        // An integer to a positive power, or a power of two to a negative one, is exact when the odd part fits the
        // significand and the result stays in the normal range.
        let mut product: u64 = 1;
        let mut count = 0.0;
        while odd > 1 && count < b {
            match product.checked_mul(odd) {
                Some(next) if next < 1 << 53 => product = next,
                _ => break,
            }
            count += 1.0;
        }
        let scale = exponent as f64 * b;
        let top = scale + f64::from(63 - product.leading_zeros());
        if (odd == 1 || count == b) && scale >= -1022.0 && top <= 1023.0 {
            return signed(product as f64 * f64::from_bits(((scale as i64 + 1023) as u64) << 52));
        }
    }
    // Far past the largest double, every `pow` overflows.
    if b * a.abs().log2() > 1100.0 { return Power::NotFinite; }
    Power::Inexact
}

/// `Number.isInteger(n) ? IntegerValue : FloatValue`.
fn number_value<'a>(number: f64) -> V<'a> { if number.is_finite() && number.trunc() == number { V::Int(number) } else { V::Float(number) } }

/// `convertToRuntimeValues` for a context value.
fn convert<'a>(value: &Value, depth: usize, budget: &Budget) -> Result<V<'a>, JinjaError> {
    budget.work(1)?;
    if depth > budget.limits().max_depth { return Err(too_deep()); }
    Ok(match value {
        Value::Undefined => V::Undefined,
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(*flag),
        Value::Number(number) => number_value(*number),
        Value::Text(text) => V::Str(Rc::from(units::collect(text, budget)?)),
        Value::Array(items) => array_value(items.as_slice().iter().map(|item| convert(item, depth + 1, budget)).collect::<Result<_, _>>()?),
        Value::Object(entries) => {
            let mut map = Map::default();
            for (key, item) in entries.as_slice() { map.entries.push((Rc::from(units::collect(key, budget)?), convert(item, depth + 1, budget)?)); }
            object_value(map)
        }
    })
}

pub(crate) fn render(program: &[Node], context: &Value, options: &RenderOptions, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    render_in(program, context, options, budget)
}

fn render_in<'a>(program: &'a [Node], context: &Value, options: &RenderOptions, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    let env: Env<'a> = new_env(None);
    let declare = |name: &str, value: V<'a>, env: &Env<'a>| -> Result<(), JinjaError> { declare_variable(env, &rc(name), value) };
    for (name, value) in [("false", V::Bool(false)), ("true", V::Bool(true)), ("none", V::Null)] { declare(name, value, &env)?; }
    declare("raise_exception", func(Func::RaiseException), &env)?;
    declare("range", func(Func::Range), &env)?;
    declare("strftime_now", func(Func::StrftimeNow), &env)?;
    for (name, value) in [("True", V::Bool(true)), ("False", V::Bool(false)), ("None", V::Null)] { declare(name, value, &env)?; }
    match context {
        Value::Object(entries) => for (key, value) in entries.as_slice() {
            let name: Str = Rc::from(units::collect(key, budget)?);
            declare_variable(&env, &name, convert(value, 1, budget)?)?;
        },
        Value::Undefined | Value::Null => {}
        _ => return Err(JinjaError::Unsupported("context-not-object")),
    }
    let mut interp = Interp { budget, options, depth: 0, limit: budget.limits().max_depth.saturating_mul(4), namespaces: Vec::new() };
    let result = interp.block(program, &env);
    drop(env);
    for namespace in interp.namespaces.drain(..) {
        if let Some(namespace) = namespace.upgrade() {
            let entries = std::mem::take(&mut namespace.borrow_mut().entries);
            drop(entries);
        }
    }
    match result {
        Ok(output) => Ok(output),
        Err(Flow::Error(error)) => Err(error),
        Err(Flow::Break | Flow::Continue) => Err(runtime("break or continue outside a loop")),
    }
}

/// `declareVariable`: a second declaration of a name, `namespace` included, throws.
fn declare_variable<'a>(env: &Env<'a>, name: &Str, value: V<'a>) -> Result<(), JinjaError> {
    if is(name, "namespace") || env.vars.borrow().contains_key(name) {
        // JS throws a SyntaxError, but while rendering.
        return Err(runtime(format!("Variable already declared: {}", lossy(name))));
    }
    set_var(env, name, value);
    Ok(())
}

/// `Array.prototype.at` index: ToIntegerOrInfinity, then counted from the end when negative.
fn relative_index(index: f64, length: usize) -> Option<usize> {
    let index = if index.is_nan() { 0.0 } else { index.trunc() };
    let index = if index >= 0.0 { index } else { length as f64 + index };
    (index >= 0.0 && index < length as f64).then_some(index as usize)
}

fn js_min(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) } }
fn js_max(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) } }

/// `slice(array, start, stop, step)` from `utils.ts`, as indices.
fn slice_indices(length: usize, start: Option<f64>, stop: Option<f64>, step: Option<f64>, budget: &Budget) -> Result<Vec<usize>, Error> {
    let step = step.unwrap_or(1.0);
    let direction = if step.is_nan() { f64::NAN } else if step == 0.0 { step } else { step.signum() };
    let length = length as f64;
    let (start, stop) = if direction >= 0.0 {
        let start = start.unwrap_or(0.0);
        let stop = stop.unwrap_or(length);
        (if start < 0.0 { js_max(length + start, 0.0) } else { js_min(start, length) },
         if stop < 0.0 { js_max(length + stop, 0.0) } else { js_min(stop, length) })
    } else {
        let start = start.unwrap_or(length - 1.0);
        let stop = stop.unwrap_or(-1.0);
        (if start < 0.0 { js_max(length + start, -1.0) } else { js_min(start, length - 1.0) },
         if stop < -1.0 { js_max(length + stop, -1.0) } else { js_min(stop, length - 1.0) })
    };
    let mut indices = Vec::new();
    let mut index = start;
    while direction * index < direction * stop {
        budget.work(1)?;
        indices.push(index as usize);
        index += step;
    }
    Ok(indices)
}

const TESTS: &[&str] = &[
    "boolean", "callable", "odd", "even", "false", "true", "none", "string", "number", "integer", "iterable", "mapping",
    "sequence", "lower", "upper", "defined", "undefined", "equalto", "eq",
];

enum SortKey { Null, Undefined, Number(f64), Text(Vec<u16>), Other }

impl<'a, 'b> Interp<'a, 'b> {
    fn eval(&mut self, node: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        self.budget.work(1)?;
        self.depth += 1;
        if self.depth > self.limit {
            self.depth -= 1;
            return Err(too_deep().into());
        }
        let result = self.eval_node(node, env);
        self.depth -= 1;
        result
    }

    /// `evaluateBlock`: the runtime string of every statement whose value is not null or undefined.
    fn block(&mut self, statements: &'a [Node], env: &Env<'a>) -> R<Vec<u16>> {
        let mut out = Vec::new();
        for statement in statements {
            let value = self.eval(statement, env)?;
            if matches!(value, V::Null | V::Undefined) { continue; }
            let text = to_string(&value, self.budget)?;
            self.budget.work(text.len())?;
            if out.len().saturating_add(text.len()) > self.budget.limits().max_text_units { return Err(Error::Limit("text-units").into()); }
            out.extend_from_slice(&text);
        }
        Ok(out)
    }

    fn text(&self, value: Vec<u16>) -> R<V<'a>> { Ok(V::str(value, self.budget)?) }

    // A debug build gives a function one stack slot per local of every arm, so each arm with locals of its own is a
    // separate function and the recursion through here stays small.
    fn eval_node(&mut self, node: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        match node {
            Node::Set { assignee, value, body } => self.eval_set(assignee, value.as_deref(), body, env),
            Node::If { test, body, alternate } => self.eval_if(test, body, alternate, env),
            Node::For { loopvar, iterable, body, default_block } => self.eval_for(loopvar, iterable, body, default_block, env),
            Node::Macro { name, args, body } => self.eval_macro(name, args, body, env),
            Node::CallStatement { callee, call_args, caller_args, body } => self.eval_call_statement(callee, call_args, caller_args.as_deref(), body, env),
            Node::Break => Err(Flow::Break),
            Node::Continue => Err(Flow::Continue),
            Node::Integer(value) => Ok(V::Int(*value)),
            Node::Float(value) => Ok(V::Float(*value)),
            Node::Str(value) => Ok(V::Str(value.clone())),
            Node::Array(items) => Ok(V::Array(Rc::new(self.eval_items(items, env)?))),
            Node::Tuple(items) => Ok(V::Tuple(Rc::new(self.eval_items(items, env)?))),
            Node::Object(entries) => self.eval_object(entries, env),
            Node::Identifier(name) => {
                self.budget.work(1)?;
                Ok(lookup(env, name))
            }
            Node::Call { callee, args } => self.eval_call(callee, args, env),
            Node::Member { object, property, computed } => self.eval_member(object, property, *computed, env),
            Node::Unary { operator, argument } => self.eval_unary(operator, argument, env),
            Node::Binary { operator, left, right } => self.eval_binary(operator, left, right, env),
            Node::Filter { operand, filter } => self.eval_filter(operand, filter, env),
            Node::FilterStatement { filter, body } => self.eval_filter_statement(filter, body, env),
            Node::Test { operand, negate, test } => self.eval_test(operand, *negate, test, env),
            Node::Select { lhs, test } => self.eval_select(lhs, test, env),
            Node::Ternary { condition, true_expr, false_expr } => self.eval_ternary(condition, true_expr, false_expr, env),
            Node::Comment => Ok(V::Null),
            other => fail(format!("Unknown node type: {}", other.type_name())),
        }
    }

    fn eval_if(&mut self, test: &'a Node, body: &'a [Node], alternate: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let test = self.eval(test, env)?.truthy();
        let output = self.block(if test { body } else { alternate }, env)?;
        self.text(output)
    }

    fn eval_macro(&mut self, name: &'a Str, args: &'a [Node], body: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        self.budget.work(body.len())?;
        let special = special_arguments(args, body);
        set_var(env, name, func(Func::Macro { name: name.clone(), args, body, special }));
        Ok(V::Null)
    }

    fn eval_call_statement(&mut self, callee: &'a Node, call_args: &'a [Node], caller_args: Option<&'a [Node]>, body: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let parameters: &'a [Node] = caller_args.unwrap_or(&[]);
        let special = special_arguments(parameters, body);
        let caller = func(Func::Caller { args: parameters, body, special, env: env.clone() });
        let (mut args, kwargs) = self.eval_arguments(call_args, env)?;
        args.push(V::Kwargs(Rc::new(kwargs)));
        let function = self.callee(callee, env)?;
        let scope = new_env(Some(env));
        set_var(&scope, &rc("caller"), caller);
        self.call(&function, args, &scope)
    }

    fn callee(&mut self, callee: &'a Node, env: &Env<'a>) -> R<Rc<Func<'a>>> {
        let callee = self.eval(callee, env)?;
        match &callee {
            V::Func(function) => Ok(function.clone()),
            other => fail(format!("Cannot call something that is not a function: got {}", other.type_name())),
        }
    }

    fn eval_items(&mut self, items: &'a [Node], env: &Env<'a>) -> R<Vec<V<'a>>> {
        let mut values = Vec::with_capacity(items.len());
        for item in items { values.push(self.eval(item, env)?); }
        Ok(values)
    }

    fn eval_object(&mut self, entries: &'a [(Node, Node)], env: &Env<'a>) -> R<V<'a>> {
        let mut map = Map::default();
        for (key, value) in entries {
            let key = match &self.eval(key, env)? {
                V::Str(key) => key.clone(),
                other => return fail(format!("Object keys must be strings: got {}", other.type_name())),
            };
            let value = self.eval(value, env)?;
            self.budget.work(map.len())?;
            map.set(key, value);
        }
        Ok(object_value(map))
    }

    fn eval_call(&mut self, callee: &'a Node, args: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let (mut args, kwargs) = self.eval_arguments(args, env)?;
        if kwargs.len() > 0 { args.push(V::Kwargs(Rc::new(kwargs))); }
        let function = self.callee(callee, env)?;
        self.call(&function, args, env)
    }

    fn eval_unary(&mut self, operator: &'a [u16], argument: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        let argument = self.eval(argument, env)?;
        if is(operator, "not") { return Ok(V::Bool(!argument.js_truthy())); }
        let sign = if is(operator, "-") { -1.0 } else { 1.0 };
        match &argument {
            V::Float(value) => Ok(V::Float(sign * value)),
            V::Int(value) => Ok(V::Int(sign * value)),
            V::Bool(value) => Ok(V::Int(sign * f64::from(u8::from(*value)))),
            other => fail(format!("Unknown operator \"{}\" for {}", lossy(operator), other.type_name())),
        }
    }

    fn eval_filter(&mut self, operand: &'a Node, filter: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        let operand = self.eval(operand, env)?;
        self.apply_filter(operand, filter, env)
    }

    fn eval_filter_statement(&mut self, filter: &'a Node, body: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let rendered = self.block(body, env)?;
        let rendered = self.text(rendered)?;
        self.apply_filter(rendered, filter, env)
    }

    fn eval_test(&mut self, operand: &'a Node, negate: bool, test: &'a [u16], env: &Env<'a>) -> R<V<'a>> {
        let operand = self.eval(operand, env)?;
        let Some(result) = self.test(test, &operand, None)? else { return fail(format!("Unknown test: {}", lossy(test))) };
        Ok(V::Bool(if negate { !result } else { result }))
    }

    fn eval_select(&mut self, lhs: &'a Node, test: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        if !self.eval(test, env)?.truthy() { return Ok(V::Undefined); }
        self.eval(lhs, env)
    }

    fn eval_ternary(&mut self, condition: &'a Node, true_expr: &'a Node, false_expr: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        let condition = self.eval(condition, env)?.truthy();
        self.eval(if condition { true_expr } else { false_expr }, env)
    }

    fn eval_set(&mut self, assignee: &'a Node, value: Option<&'a Node>, body: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let rhs = match value {
            Some(value) => self.eval(value, env)?,
            None => {
                let output = self.block(body, env)?;
                self.text(output)?
            }
        };
        match assignee {
            Node::Identifier(name) => set_var(env, name, rhs),
            Node::Tuple(targets) => {
                let Some(items) = rhs.items() else { return fail(format!("Cannot unpack non-iterable type in set: {}", rhs.type_name())) };
                if items.len() != targets.len() {
                    return fail(format!("Too {} items to unpack in set", if targets.len() > items.len() { "few" } else { "many" }));
                }
                for (target, item) in targets.iter().zip(items.iter()) {
                    let Node::Identifier(name) = target else { return fail(format!("Cannot unpack to non-identifier in set: {}", target.type_name())) };
                    set_var(env, name, item.clone());
                }
            }
            Node::Member { object, property, .. } => {
                let object = self.eval(object, env)?;
                let V::Namespace(namespace) = &object else { return fail("cannot assign attribute on non-namespace object") };
                let Node::Identifier(name) = property.as_ref() else { return fail("Cannot assign to member with non-identifier property") };
                self.budget.work(namespace.borrow().len())?;
                namespace.borrow_mut().set(name.clone(), rhs);
            }
            other => return fail(format!("Invalid LHS inside assignment expression: {}", other.type_name())),
        }
        Ok(V::Null)
    }

    fn eval_for(&mut self, loopvar: &'a Node, iterable: &'a Node, body: &'a [Node], default_block: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let scope = new_env(Some(env));
        let (iterable, test) = match iterable {
            Node::Select { lhs, test } => (lhs.as_ref(), Some(test.as_ref())),
            other => (other, None),
        };
        let iterable = self.eval(iterable, &scope)?;
        let items: Rc<Vec<V<'a>>> = match &iterable {
            V::Array(items) | V::Tuple(items) => items.clone(),
            V::Object(map) | V::Kwargs(map) => Rc::new(map.entries.iter().map(|(key, _)| V::Str(key.clone())).collect()),
            other => return fail(format!("Expected iterable or object type in for loop: got {}", other.type_name())),
        };
        enum Update<'a> { One(V<'a>), Many(Rc<Vec<V<'a>>>) }
        let apply = |scope: &Env<'a>, update: &Update<'a>| -> R<()> {
            match (loopvar, update) {
                (Node::Identifier(name), Update::One(value)) => set_var(scope, name, value.clone()),
                (Node::Tuple(targets), Update::Many(values)) => for (target, value) in targets.iter().zip(values.iter()) {
                    let Node::Identifier(name) = target else { return fail(format!("Cannot unpack non-identifier type: {}", target.type_name())) };
                    set_var(scope, name, value.clone());
                },
                _ => unreachable!("updates follow the loop variable"),
            }
            Ok(())
        };
        let mut selected: Vec<V<'a>> = Vec::new();
        let mut updates: Vec<Update<'a>> = Vec::new();
        for current in items.iter() {
            self.budget.work(1)?;
            let update = match loopvar {
                Node::Identifier(_) => Update::One(current.clone()),
                Node::Tuple(targets) => {
                    let V::Array(values) = current else { return fail(format!("Cannot unpack non-iterable type: {}", current.type_name())) };
                    if targets.len() != values.len() {
                        return fail(format!("Too {} items to unpack", if targets.len() > values.len() { "few" } else { "many" }));
                    }
                    Update::Many(values.clone())
                }
                other => return fail(format!("Invalid loop variable(s): {}", other.type_name())),
            };
            if let Some(test) = test {
                let loop_scope = new_env(Some(&scope));
                apply(&loop_scope, &update)?;
                if !self.eval(test, &loop_scope)?.truthy() { continue; }
            }
            selected.push(current.clone());
            updates.push(update);
        }
        let mut result = Vec::new();
        let mut no_iteration = true;
        let length = selected.len();
        for index in 0..length {
            let number = |value: usize| V::Int(value as f64);
            let mut state = Map::default();
            state.entries = vec![
                (rc("index"), number(index + 1)),
                (rc("index0"), number(index)),
                (rc("revindex"), number(length - index)),
                (rc("revindex0"), number(length - index - 1)),
                (rc("first"), V::Bool(index == 0)),
                (rc("last"), V::Bool(index == length - 1)),
                (rc("length"), number(length)),
                (rc("previtem"), if index > 0 { selected[index - 1].clone() } else { V::Undefined }),
                (rc("nextitem"), if index + 1 < length { selected[index + 1].clone() } else { V::Undefined }),
            ];
            set_var(&scope, &rc("loop"), object_value(state));
            apply(&scope, &updates[index])?;
            match self.block(body, &scope) {
                Ok(output) => {
                    self.budget.work(output.len())?;
                    if result.len().saturating_add(output.len()) > self.budget.limits().max_text_units { return Err(Error::Limit("text-units").into()); }
                    result.extend_from_slice(&output);
                }
                Err(Flow::Continue) => continue,
                Err(Flow::Break) => break,
                Err(error) => return Err(error),
            }
            no_iteration = false;
        }
        if no_iteration {
            let output = self.block(default_block, &scope)?;
            result.extend_from_slice(&output);
        }
        self.text(result)
    }

    fn eval_arguments(&mut self, args: &'a [Node], env: &Env<'a>) -> R<(Vec<V<'a>>, Map<'a>)> {
        let mut positional = Vec::new();
        let mut keywords = Map::default();
        for argument in args {
            match argument {
                Node::Spread(inner) => {
                    let value = self.eval(inner, env)?;
                    let Some(items) = value.items() else { return fail(format!("Cannot unpack non-iterable type: {}", value.type_name())) };
                    positional.extend(items.iter().cloned());
                }
                Node::KeywordArgument { .. } | Node::KeywordSpread(_) => {}
                other => positional.push(self.eval(other, env)?),
            }
        }
        let add = |keywords: &mut Map<'a>, key: Str, value: V<'a>, budget: &Budget| -> R<()> {
            budget.work(keywords.len())?;
            if keywords.has(&key) { return fail(format!("Got multiple values for keyword argument '{}'", lossy(&key))); }
            keywords.set(key, value);
            Ok(())
        };
        for argument in args {
            match argument {
                Node::KeywordArgument { key, value } => {
                    let value = self.eval(value, env)?;
                    add(&mut keywords, key.clone(), value, self.budget)?;
                }
                Node::KeywordSpread(inner) => {
                    let value = self.eval(inner, env)?;
                    let Some(map) = value.object() else { return fail(format!("Argument after ** must be a mapping, not {}", value.type_name())) };
                    for (key, item) in &map.entries { add(&mut keywords, key.clone(), item.clone(), self.budget)?; }
                }
                _ => {}
            }
        }
        Ok((positional, keywords))
    }

    fn eval_member(&mut self, object: &'a Node, property: &'a Node, computed: bool, env: &Env<'a>) -> R<V<'a>> {
        let object = self.eval(object, env)?;
        let property = if computed {
            if let Node::Slice { start, stop, step } = property {
                return self.eval_slice(object, start.as_deref(), stop.as_deref(), step.as_deref(), env);
            }
            self.eval(property, env)?
        } else {
            match property {
                Node::Integer(value) => V::Int(*value),
                Node::Identifier(name) => V::Str(name.clone()),
                other => return fail(format!("Unexpected member property: {}", other.type_name())),
            }
        };
        self.budget.work(1)?;
        let value = match &object {
            V::Object(map) | V::Kwargs(map) => {
                let V::Str(key) = &property else { return fail(format!("Cannot access property with non-string: got {}", property.type_name())) };
                self.budget.work(map.len())?;
                match map.get(key) { Some(value) => Some(value.clone()), None => builtin(&object, key) }
            }
            V::Namespace(map) => {
                let V::Str(key) = &property else { return fail(format!("Cannot access property with non-string: got {}", property.type_name())) };
                self.budget.work(map.borrow().len())?;
                map.borrow().get(key).cloned()
            }
            V::Array(items) | V::Tuple(items) => match &property {
                V::Int(index) => relative_index(*index, items.len()).map(|index| items[index].clone()),
                V::Str(key) => builtin(&object, key),
                other => return fail(format!("Cannot access property with non-string/non-number: got {}", other.type_name())),
            },
            V::Str(text) => match &property {
                V::Int(index) => match relative_index(*index, text.len()) {
                    Some(index) => Some(V::Str(Rc::from(&text[index..index + 1]))),
                    // `new StringValue(undefined)`: a string whose payload is undefined.
                    None => return decline("string-index-out-of-range"),
                },
                V::Str(key) => builtin(&object, key),
                other => return fail(format!("Cannot access property with non-string/non-number: got {}", other.type_name())),
            },
            _ => {
                let V::Str(_) = &property else { return fail(format!("Cannot access property with non-string: got {}", property.type_name())) };
                None
            }
        };
        Ok(value.unwrap_or(V::Undefined))
    }

    fn eval_slice(&mut self, object: V<'a>, start: Option<&'a Node>, stop: Option<&'a Node>, step: Option<&'a Node>, env: &Env<'a>) -> R<V<'a>> {
        if !matches!(object, V::Array(_) | V::Tuple(_) | V::Str(_)) { return fail("Slice object must be an array or string"); }
        let mut bound = |node: Option<&'a Node>, label: &str| -> R<Option<f64>> {
            let value = match node { Some(node) => self.eval(node, env)?, None => V::Undefined };
            match value {
                V::Int(number) => Ok(Some(number)),
                V::Undefined => Ok(None),
                _ => fail(format!("Slice {label} must be numeric or undefined")),
            }
        };
        let start = bound(start, "start")?;
        let stop = bound(stop, "stop")?;
        let step = bound(step, "step")?;
        match &object {
            V::Array(items) | V::Tuple(items) => {
                let indices = slice_indices(items.len(), start, stop, step, self.budget)?;
                Ok(array_value(indices.into_iter().map(|index| items[index].clone()).collect()))
            }
            V::Str(text) => {
                let points = code_points(text);
                let indices = slice_indices(points.len(), start, stop, step, self.budget)?;
                let parts: Vec<&[u16]> = indices.into_iter().map(|index| points[index]).collect();
                let joined = units::concat(&parts, self.budget)?;
                self.text(joined)
            }
            _ => unreachable!(),
        }
    }

    fn eval_binary(&mut self, operator: &[u16], left: &'a Node, right: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        let left = self.eval(left, env)?;
        if is(operator, "and") { return if left.truthy() { self.eval(right, env) } else { Ok(left) }; }
        if is(operator, "or") { return if left.truthy() { Ok(left) } else { self.eval(right, env) }; }
        let right = self.eval(right, env)?;
        let budget = self.budget;
        if is(operator, "==") { return Ok(V::Bool(loose_equals(&left, &right, budget)?)); }
        if is(operator, "!=") { return Ok(V::Bool(!loose_equals(&left, &right, budget)?)); }
        let membership = is(operator, "in") || is(operator, "not in");
        if matches!(left, V::Undefined) || matches!(right, V::Undefined) {
            if matches!(right, V::Undefined) && membership { return Ok(V::Bool(is(operator, "not in"))); }
            return fail(format!("Cannot perform operation {} on undefined values", lossy(operator)));
        } else if matches!(left, V::Null) || matches!(right, V::Null) {
            return fail("Cannot perform operation on null values");
        } else if is(operator, "~") {
            let joined = units::concat(&[&payload_string(&left, budget)?, &payload_string(&right, budget)?], budget)?;
            return self.text(joined);
        } else if let (true, Some(a), Some(b)) = (is(operator, "**"), left.numeric_like(), right.numeric_like()) {
            if a == 0.0 && b < 0.0 { return fail("0.0 cannot be raised to a negative power"); }
            let result = match power(a, b) {
                Power::Exact(result) => result,
                Power::NotFinite => return fail("Exponentiation result is not a finite real number"),
                Power::Inexact => return Err(JinjaError::Unsupported("pow-inexact").into()),
            };
            let float = matches!(left, V::Float(_)) || matches!(right, V::Float(_)) || b < 0.0;
            return Ok(if float { V::Float(result) } else { V::Int(result) });
        } else if let (Some(a), Some(b)) = (left.number(), right.number()) {
            let float = matches!(left, V::Float(_)) || matches!(right, V::Float(_));
            let typed = |value: f64| if float { V::Float(value) } else { V::Int(value) };
            if is(operator, "+") { return Ok(typed(a + b)); }
            if is(operator, "-") { return Ok(typed(a - b)); }
            if is(operator, "*") { return Ok(typed(a * b)); }
            if is(operator, "/") { return Ok(V::Float(a / b)); }
            if is(operator, "//") { return Ok(typed((a / b).floor())); }
            if is(operator, "%") { return Ok(typed(a % b)); }
            if is(operator, "<") { return Ok(V::Bool(a < b)); }
            if is(operator, ">") { return Ok(V::Bool(a > b)); }
            if is(operator, ">=") { return Ok(V::Bool(a >= b)); }
            if is(operator, "<=") { return Ok(V::Bool(a <= b)); }
        } else if let (Some(a), Some(b)) = (left.items(), right.items()) {
            if is(operator, "+") {
                budget.work(a.len() + b.len())?;
                return Ok(array_value(a.iter().chain(b.iter()).cloned().collect()));
            }
        } else if let Some(items) = right.items() {
            if membership {
                let mut member = false;
                for item in items.iter() {
                    budget.work(1)?;
                    if strict_equals(item, &left)? { member = true; break; }
                }
                return Ok(V::Bool(member == is(operator, "in")));
            }
        }
        if matches!(left, V::Str(_)) || matches!(right, V::Str(_)) {
            if is(operator, "+") {
                let joined = units::concat(&[&payload_string(&left, budget)?, &payload_string(&right, budget)?], budget)?;
                return self.text(joined);
            }
        }
        if let (V::Str(needle), V::Str(haystack)) = (&left, &right) {
            if membership {
                let found = units::find(haystack, needle, 0, budget)?.is_some();
                return Ok(V::Bool(found == is(operator, "in")));
            }
        }
        if let (V::Str(key), Some(map)) = (&left, right.object()) {
            if membership {
                budget.work(map.len())?;
                return Ok(V::Bool(map.has(key) == is(operator, "in")));
            }
        }
        fail(format!("Unknown operator \"{}\" between {} and {}", lossy(operator), left.type_name(), right.type_name()))
    }

    /// `Environment.tests`, the names in [`TESTS`]; `None` for an unknown test. `b` is the optional second argument `selectattr` passes.
    fn test(&self, name: &[u16], a: &V<'a>, b: Option<&V<'a>>) -> R<Option<bool>> {
        let name = String::from_utf16_lossy(name);
        Ok(Some(match name.as_str() {
            "boolean" => matches!(a, V::Bool(_)),
            "callable" => matches!(a, V::Func(_)),
            "odd" | "even" => {
                let V::Int(value) = a else { return fail(format!("cannot {name} on {}", a.type_name())) };
                let odd = value % 2.0 != 0.0;
                if name == "odd" { odd } else { !odd }
            }
            "false" => matches!(a, V::Bool(false)),
            "true" => matches!(a, V::Bool(true)),
            "none" => matches!(a, V::Null),
            "string" => matches!(a, V::Str(_)),
            "number" => matches!(a, V::Int(_) | V::Float(_)),
            "integer" => matches!(a, V::Int(_)),
            "iterable" => matches!(a, V::Array(_) | V::Str(_)),
            "mapping" => matches!(a, V::Object(_) | V::Kwargs(_)),
            "sequence" => matches!(a, V::Array(_) | V::Tuple(_) | V::Object(_) | V::Kwargs(_) | V::Str(_)),
            "lower" | "upper" => match a {
                V::Str(text) => units::case_map(text, name == "upper", self.options.case_tables, self.budget)?[..] == text[..],
                _ => false,
            },
            "defined" => !matches!(a, V::Undefined),
            "undefined" => matches!(a, V::Undefined),
            "equalto" | "eq" => {
                let Some(b) = b else { return fail("Cannot read properties of undefined (reading 'value')") };
                strict_equals(a, b)?
            }
            _ => {
                debug_assert!(!TESTS.contains(&name.as_str()));
                return Ok(None);
            }
        }))
    }

    fn call(&mut self, function: &Rc<Func<'a>>, args: Vec<V<'a>>, scope: &Env<'a>) -> R<V<'a>> {
        self.budget.work(1)?;
        match function.as_ref() {
            Func::Str(method, text) => self.string_method(*method, text, &args),
            Func::Object(method, map) => self.object_method(*method, map, &args),
            Func::Namespace => self.namespace(args),
            Func::RaiseException => self.raise_exception(&args),
            Func::Range => self.range(&args),
            Func::StrftimeNow => self.strftime_now(&args),
            Func::Macro { name, args: parameters, body, special } => self.run_macro(Some(name), parameters, body, *special, args, scope),
            Func::Caller { args: parameters, body, special, env } => self.run_macro(None, parameters, body, *special, args, env),
        }
    }

    fn raise_exception(&mut self, args: &[V<'a>]) -> R<V<'a>> {
        let message = match args.first() {
            None | Some(V::Undefined) => Vec::new(),
            Some(value) => payload_string(value, self.budget)?,
        };
        Err(Flow::Error(JinjaError::Raised(message)))
    }

    /// A macro runs in a scope under the caller's scope, the `caller` of a call block under the block's scope.
    fn run_macro(&mut self, name: Option<&Str>, parameters: &'a [Node], body: &'a [Node], special: Special, args: Vec<V<'a>>, parent: &Env<'a>) -> R<V<'a>> {
        let scope = new_env(Some(parent));
        let display = match name { Some(name) => format!("'{}'", lossy(name)), None => "None".to_owned() };
        self.bind_macro_arguments(&display, parameters, special, args, &scope)?;
        drop(display);
        let output = self.block(body, &scope)?;
        self.text(output)
    }

    fn bind_macro_arguments(&mut self, display: &str, parameters: &'a [Node], special: Special, args: Vec<V<'a>>, scope: &Env<'a>) -> R<()> {
        let mut positional = args;
        let mut keywords = Map::default();
        if matches!(positional.last(), Some(V::Kwargs(_))) {
            if let Some(V::Kwargs(map)) = &positional.pop() { keywords = (**map).clone(); }
        }
        let mut pending: Vec<(&'a Str, &'a Node)> = Vec::new();
        for (index, argument) in parameters.iter().enumerate() {
            self.budget.work(1)?;
            let Some((name, default)) = parameter(argument) else { return fail("Expected identifier for parameter declaration") };
            let mut value = positional.get(index).cloned();
            if value.is_none() && keywords.has(name) {
                value = keywords.get(name).cloned();
                keywords.delete(name);
            }
            if value.is_none() {
                if let Some(default) = default { pending.push((name, default)); }
            }
            set_var(scope, name, value.unwrap_or(V::Undefined));
        }
        if special.kwargs {
            set_var(scope, &rc("kwargs"), object_value(keywords));
        } else if let Some((key, _)) = keywords.entries.first() {
            return fail(format!("macro {display} takes no keyword argument '{}'", lossy(key)));
        }
        if special.varargs {
            let rest = positional.get(parameters.len()..).map(|rest| rest.to_vec()).unwrap_or_default();
            set_var(scope, &rc("varargs"), array_value(rest));
        } else if positional.len() > parameters.len() {
            return fail(format!("macro {display} takes not more than {} argument(s)", parameters.len()));
        }
        for (name, default) in pending {
            let value = self.eval(default, scope)?;
            set_var(scope, name, value);
        }
        Ok(())
    }

    fn namespace(&mut self, args: Vec<V<'a>>) -> R<V<'a>> {
        let mut positional = args;
        let mut keywords = None;
        if matches!(positional.last(), Some(V::Kwargs(_))) {
            if let Some(V::Kwargs(map)) = &positional.pop() { keywords = Some(map.clone()); }
        }
        if positional.len() > 1 { return fail(format!("namespace expected at most 1 argument, got {}", positional.len())); }
        let mut entries = Map::default();
        if let Some(source) = positional.first() {
            if let Some(map) = source.object() {
                for (key, value) in &map.entries { self.budget.work(entries.len())?; entries.set(key.clone(), value.clone()); }
            } else if let Some(items) = source.items() {
                for pair in items.iter() {
                    let values: Vec<V<'a>> = match pair {
                        V::Array(values) | V::Tuple(values) => values.to_vec(),
                        V::Str(text) => code_points(text).into_iter().map(|point| V::Str(Rc::from(point))).collect(),
                        _ => return fail("namespace expected an object or an iterable of [key, value] pairs"),
                    };
                    if values.len() != 2 { return fail("namespace expected an object or an iterable of [key, value] pairs"); }
                    let V::Str(key) = &values[0] else { return fail("namespace keys must be strings") };
                    self.budget.work(entries.len())?;
                    entries.set(key.clone(), values[1].clone());
                }
            } else {
                return fail(format!("'{}' object is not iterable", source.type_name()));
            }
        }
        if let Some(map) = keywords {
            for (key, value) in &map.entries { self.budget.work(entries.len())?; entries.set(key.clone(), value.clone()); }
        }
        let namespace = Rc::new(RefCell::new(entries));
        self.namespaces.push(Rc::downgrade(&namespace));
        Ok(V::Namespace(namespace))
    }

    /// `range` through `convertToRuntimeValues`, which passes payloads. Only number arguments are ported.
    fn range(&mut self, args: &[V<'a>]) -> R<V<'a>> {
        let used = &args[..args.len().min(3)];
        let mut numbers = Vec::with_capacity(used.len());
        for value in used {
            let Some(number) = value.number() else { return decline("range-argument") };
            numbers.push(number);
        }
        let (start, stop, step) = match numbers.as_slice() {
            [] => return Ok(array_value(Vec::new())),
            [stop] => (0.0, *stop, 1.0),
            [start, stop] => (*start, *stop, 1.0),
            [start, stop, step, ..] => (*start, *stop, *step),
        };
        if step == 0.0 { return fail("range() step must not be zero"); }
        let mut values = Vec::new();
        let mut index = start;
        if step > 0.0 {
            while index < stop { self.budget.work(1)?; values.push(number_value(index)); index += step; }
        } else {
            while index > stop { self.budget.work(1)?; values.push(number_value(index)); index += step; }
        }
        Ok(array_value(values))
    }

    fn strftime_now(&mut self, args: &[V<'a>]) -> R<V<'a>> {
        let Some(now) = self.options.now else { return decline("strftime-now") };
        let Some(V::Str(format)) = args.first() else { return fail("format.replace is not a function") };
        let pad = |value: u8| s(&format!("{value:02}"));
        let mut out = Vec::with_capacity(format.len());
        let mut at = 0;
        self.budget.work(format.len())?;
        while at < format.len() {
            if format[at] == 0x25 {
                if let Some(next) = format.get(at + 1) {
                    let replaced = match *next {
                        0x59 => Some(s(&now.year.to_string())),
                        0x6d => Some(pad(now.month)),
                        0x64 => Some(pad(now.day)),
                        0x48 => Some(pad(now.hour)),
                        0x4d => Some(pad(now.minute)),
                        0x25 => Some(s("%")),
                        0x62 | 0x42 => {
                            let Some(names) = &self.options.month_names else { return decline("strftime-month-name") };
                            let list = if *next == 0x42 { &names.long } else { &names.short };
                            let index = usize::from(now.month.clamp(1, 12)) - 1;
                            Some(units::collect(&list[index], self.budget)?)
                        }
                        _ => None,
                    };
                    if let Some(replaced) = replaced {
                        out.extend(replaced);
                        at += 2;
                        continue;
                    }
                }
            }
            out.push(format[at]);
            at += 1;
        }
        self.text(out)
    }

    fn string_method(&mut self, method: StrMethod, text: &Str, args: &[V<'a>]) -> R<V<'a>> {
        let budget = self.budget;
        budget.work(text.len())?;
        match method {
            StrMethod::Upper => self.text(units::case_map(text, true, self.options.case_tables, budget)?),
            StrMethod::Lower => self.text(units::case_map(text, false, self.options.case_tables, budget)?),
            StrMethod::Strip => self.text(units::trim(text).to_vec()),
            StrMethod::Rstrip => self.text(units::trim_end(text).to_vec()),
            StrMethod::Lstrip => self.text(units::trim_start(text).to_vec()),
            StrMethod::Title => {
                // /\b\w/g, c => c.toUpperCase(): ASCII word characters after a non-word character.
                let mut out = text.to_vec();
                for index in 0..out.len() {
                    if is_word(text[index]) && (index == 0 || !is_word(text[index - 1])) && (0x61..=0x7a).contains(&text[index]) {
                        out[index] -= 0x20;
                    }
                }
                self.text(out)
            }
            StrMethod::Capitalize => {
                let Some(&first) = text.first() else { return self.text(Vec::new()) };
                let mut out = if (0xd800..=0xdfff).contains(&first) { vec![first] } else { units::case_map(&[first], true, self.options.case_tables, budget)? };
                out.extend_from_slice(&text[1..]);
                self.text(out)
            }
            StrMethod::Startswith | StrMethod::Endswith => {
                let label = if method == StrMethod::Startswith { "startswith" } else { "endswith" };
                let Some(pattern) = args.first() else { return fail(format!("{label}() requires at least one argument")) };
                let matches = |needle: &[u16]| if method == StrMethod::Startswith { text.starts_with(needle) } else { text.ends_with(needle) };
                match pattern {
                    V::Str(needle) => Ok(V::Bool(matches(needle))),
                    V::Array(items) | V::Tuple(items) => {
                        for item in items.iter() {
                            budget.work(1)?;
                            let V::Str(needle) = item else { return fail(format!("{label}() tuple elements must be strings")) };
                            if matches(needle) { return Ok(V::Bool(true)); }
                        }
                        Ok(V::Bool(false))
                    }
                    _ => fail(format!("{label}() argument must be a string or tuple of strings")),
                }
            }
            StrMethod::Split => {
                let separator = args.first().cloned().unwrap_or(V::Null);
                if !matches!(separator, V::Str(_) | V::Null) { return fail("sep argument must be a string or null"); }
                let maxsplit = args.get(1).cloned().unwrap_or(V::Int(-1.0));
                let V::Int(maxsplit) = maxsplit else { return fail("maxsplit argument must be a number") };
                let mut result: Vec<Vec<u16>> = Vec::new();
                match &separator {
                    V::Null => {
                        let rest = units::trim_start(text);
                        let mut at = 0;
                        while at < rest.len() {
                            if units::is_ws(rest[at]) { at += 1; continue; }
                            let end = at + rest[at..].iter().take_while(|unit| !units::is_ws(**unit)).count();
                            budget.work(end - at)?;
                            if maxsplit != -1.0 && result.len() as f64 >= maxsplit {
                                result.push(rest[at..].to_vec());
                                break;
                            }
                            result.push(rest[at..end].to_vec());
                            at = end;
                        }
                    }
                    V::Str(separator) => {
                        if separator.is_empty() { return fail("empty separator"); }
                        result = units::split(text, separator, budget)?.into_iter().map(|part| part.to_vec()).collect();
                        if maxsplit != -1.0 && result.len() as f64 > maxsplit {
                            let length = result.len() as f64;
                            let start = if maxsplit.is_nan() { 0.0 } else { maxsplit.trunc() };
                            let start = if start < 0.0 { (length + start).max(0.0) } else { start.min(length) } as usize;
                            let removed: Vec<Vec<u16>> = result.split_off(start);
                            result.push(units::join(&removed, separator, budget)?);
                        }
                    }
                    _ => unreachable!(),
                }
                let mut values = Vec::with_capacity(result.len());
                for part in result { values.push(V::str(part, budget)?); }
                Ok(array_value(values))
            }
            StrMethod::Replace => {
                if args.len() < 2 { return fail("replace() requires at least two arguments"); }
                let (V::Str(old), V::Str(new)) = (&args[0], &args[1]) else { return fail("replace() arguments must be strings") };
                let count = match args.get(2) {
                    Some(V::Kwargs(map)) => map.get(&s("count")).cloned().unwrap_or(V::Null),
                    Some(value) => value.clone(),
                    None => V::Null,
                };
                let count = match count {
                    V::Int(count) => Some(count),
                    V::Null => None,
                    _ => return fail("replace() count argument must be a number or null"),
                };
                self.text(replace(text, old, new, count, budget)?)
            }
        }
    }

    fn object_method(&mut self, method: ObjMethod, map: &Rc<Map<'a>>, args: &[V<'a>]) -> R<V<'a>> {
        self.budget.work(map.len())?;
        let pair = |key: &Str, value: &V<'a>| array_value(vec![V::Str(key.clone()), value.clone()]);
        match method {
            ObjMethod::Get => {
                let Some(V::Str(key)) = args.first() else {
                    return fail(format!("Object key must be a string: got {}", args.first().map_or("undefined", |key| key.type_name())));
                };
                Ok(map.get(key).cloned().or_else(|| args.get(1).cloned()).unwrap_or(V::Null))
            }
            ObjMethod::Items => Ok(array_value(map.entries.iter().map(|(key, value)| pair(key, value)).collect())),
            ObjMethod::Keys => Ok(array_value(map.entries.iter().map(|(key, _)| V::Str(key.clone())).collect())),
            ObjMethod::Values => Ok(array_value(map.entries.iter().map(|(_, value)| value.clone()).collect())),
            ObjMethod::Dictsort => {
                let mut keywords = Map::default();
                let mut positional = Vec::new();
                for argument in args {
                    match argument {
                        V::Kwargs(map) => keywords = (**map).clone(),
                        other => positional.push(other.clone()),
                    }
                }
                let pick = |index: usize, name: &str, fallback: V<'a>| positional.get(index).cloned().or_else(|| keywords.get(&s(name)).cloned()).unwrap_or(fallback);
                let V::Bool(case_sensitive) = pick(0, "case_sensitive", V::Bool(false)) else { return fail("case_sensitive must be a boolean") };
                let V::Str(by) = &pick(1, "by", V::Str(rc("key"))) else { return fail("by must be a string") };
                let by_value = if is(&by, "key") { false } else if is(&by, "value") { true } else { return fail("by must be either 'key' or 'value'") };
                let V::Bool(reverse) = pick(2, "reverse", V::Bool(false)) else { return fail("reverse must be a boolean") };
                let items: Vec<V<'a>> = map.entries.iter().map(|(key, value)| pair(key, value)).collect();
                let keys: Vec<V<'a>> = map.entries.iter().map(|(key, value)| if by_value { value.clone() } else { V::Str(key.clone()) }).collect();
                Ok(array_value(self.sort(items, keys, reverse, case_sensitive)?))
            }
        }
    }

    /// `slice().sort((a, b) => compareRuntimeValues(...))` on precomputed keys. Any correct sort compares every
    /// adjacent pair of its output, so keys of two kinds always reach a throwing comparison in JS.
    fn sort(&mut self, items: Vec<V<'a>>, keys: Vec<V<'a>>, reverse: bool, case_sensitive: bool) -> R<Vec<V<'a>>> {
        if items.len() < 2 { return Ok(items); }
        let mut sort_keys = Vec::with_capacity(keys.len());
        for key in &keys {
            sort_keys.push(match key {
                V::Null => SortKey::Null,
                V::Undefined => SortKey::Undefined,
                V::Str(text) => SortKey::Text(if case_sensitive { text.to_vec() } else { units::case_map(text, false, self.options.case_tables, self.budget)? }),
                other => match other.numeric_like() { Some(number) => SortKey::Number(number), None => SortKey::Other },
            });
        }
        let kind = |key: &SortKey| match key { SortKey::Null => 0, SortKey::Undefined => 1, SortKey::Number(_) => 2, SortKey::Text(_) => 3, SortKey::Other => 4 };
        let first = kind(&sort_keys[0]);
        if first == 4 || sort_keys.iter().any(|key| kind(key) != first) { return fail("Cannot compare different types"); }
        // NaN compares equal to every number, which leaves the order to V8's algorithm.
        if sort_keys.iter().any(|key| matches!(key, SortKey::Number(number) if number.is_nan())) { return decline("sort-nan"); }
        let count = items.len();
        self.budget.work(count.saturating_mul(count.ilog2() as usize + 1))?;
        let compare = |a: &SortKey, b: &SortKey| match (a, b) {
            (SortKey::Number(x), SortKey::Number(y)) => x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
            (SortKey::Text(x), SortKey::Text(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        };
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by(|&a, &b| if reverse { compare(&sort_keys[b], &sort_keys[a]) } else { compare(&sort_keys[a], &sort_keys[b]) });
        Ok(order.into_iter().map(|index| items[index].clone()).collect())
    }

    fn apply_filter(&mut self, operand: V<'a>, filter: &'a Node, env: &Env<'a>) -> R<V<'a>> {
        self.budget.work(1)?;
        match filter {
            Node::Identifier(name) => self.identifier_filter(operand, name),
            Node::Call { callee, args } => {
                let Node::Identifier(name) = callee.as_ref() else { return fail(format!("Unknown filter: {}", callee.type_name())) };
                self.call_filter(operand, name, args, env)
            }
            other => fail(format!("Unknown filter: {}", other.type_name())),
        }
    }

    fn identifier_filter(&mut self, operand: V<'a>, name: &[u16]) -> R<V<'a>> {
        let budget = self.budget;
        let filter = String::from_utf16_lossy(name);
        let filter = filter.as_str();
        if filter == "safe" { return Ok(operand); }
        if filter == "tojson" { return self.text(to_json(&operand, &JsonOptions::default(), 0, true, budget)?); }
        match &operand {
            V::Array(items) | V::Tuple(items) => match filter {
                "list" => Ok(operand.clone()),
                // An empty array yields the JS `undefined` itself, not an UndefinedValue.
                "first" => items.first().cloned().map_or_else(|| decline("first-of-empty"), Ok),
                "last" => items.last().cloned().map_or_else(|| decline("last-of-empty"), Ok),
                "length" => Ok(V::Int(items.len() as f64)),
                "reverse" => {
                    budget.work(items.len())?;
                    Ok(array_value(items.iter().rev().cloned().collect()))
                }
                "sort" => {
                    let values = items.to_vec();
                    Ok(array_value(self.sort(values.clone(), values, false, false)?))
                }
                "join" => {
                    let parts = items.iter().map(|item| join_payload(item, budget)).collect::<Result<Vec<_>, _>>()?;
                    self.text(units::join(&parts, &[], budget)?)
                }
                "string" => self.text(to_json(&operand, &JsonOptions::default(), 0, false, budget)?),
                "unique" => {
                    let mut output: Vec<V<'a>> = Vec::new();
                    for item in items.iter() {
                        budget.work(output.len() + 1)?;
                        let mut seen = false;
                        for kept in &output {
                            if same_value_zero(kept, item)? { seen = true; break; }
                        }
                        if !seen { output.push(item.clone()); }
                    }
                    Ok(array_value(output))
                }
                _ => fail(format!("Unknown ArrayValue filter: {filter}")),
            },
            V::Str(text) => match filter {
                "length" => Ok(V::Int(text.len() as f64)),
                "upper" => self.string_method(StrMethod::Upper, text, &[]),
                "lower" => self.string_method(StrMethod::Lower, text, &[]),
                "title" => self.string_method(StrMethod::Title, text, &[]),
                "capitalize" => self.string_method(StrMethod::Capitalize, text, &[]),
                "trim" => self.text(units::trim(text).to_vec()),
                "indent" => self.text(indent(text, &s("    "), false, false, budget)?),
                "join" | "string" => Ok(operand.clone()),
                "int" => {
                    let value = units::parse_int(text);
                    Ok(V::Int(if value.is_nan() { 0.0 } else { value }))
                }
                "float" => {
                    let value = units::parse_float(text);
                    Ok(V::Float(if value.is_nan() { 0.0 } else { value }))
                }
                _ => fail(format!("Unknown StringValue filter: {filter}")),
            },
            V::Int(value) | V::Float(value) => match filter {
                "abs" => Ok(if matches!(operand, V::Int(_)) { V::Int(value.abs()) } else { V::Float(value.abs()) }),
                "int" => Ok(V::Int(value.floor())),
                "float" => Ok(V::Float(*value)),
                "string" => self.text(to_string(&operand, budget)?),
                _ => fail(format!("Unknown NumericValue filter: {filter}")),
            },
            V::Object(map) | V::Kwargs(map) => match filter {
                "items" => self.object_method(ObjMethod::Items, map, &[]),
                "length" => Ok(V::Int(map.len() as f64)),
                _ => match object_builtin(filter) {
                    Some(method) => self.object_method(method, map, &[]),
                    None => fail(format!("Unknown ObjectValue filter: {filter}")),
                },
            },
            V::Bool(flag) => match filter {
                "bool" => Ok(V::Bool(*flag)),
                "int" => Ok(V::Int(if *flag { 1.0 } else { 0.0 })),
                "float" => Ok(V::Float(if *flag { 1.0 } else { 0.0 })),
                "string" => Ok(V::Str(rc(if *flag { "true" } else { "false" }))),
                _ => fail(format!("Unknown BooleanValue filter: {filter}")),
            },
            other => fail(format!("Cannot apply filter \"{filter}\" to type: {}", other.type_name())),
        }
    }

    fn call_filter(&mut self, operand: V<'a>, name: &[u16], args: &'a [Node], env: &Env<'a>) -> R<V<'a>> {
        let budget = self.budget;
        let filter = String::from_utf16_lossy(name);
        let filter = filter.as_str();
        match filter {
            "tojson" => {
                let (_, keywords) = self.eval_arguments(args, env)?;
                let indent = match keywords.get(&s("indent")).cloned().unwrap_or(V::Null) {
                    V::Int(count) => Some(count),
                    V::Null => None,
                    _ => return fail("If set, indent must be a number"),
                };
                let V::Bool(ensure_ascii) = keywords.get(&s("ensure_ascii")).cloned().unwrap_or(V::Bool(false)) else { return fail("If set, ensure_ascii must be a boolean") };
                let V::Bool(sort_keys) = keywords.get(&s("sort_keys")).cloned().unwrap_or(V::Bool(false)) else { return fail("If set, sort_keys must be a boolean") };
                let separators = match &keywords.get(&s("separators")).cloned().unwrap_or(V::Null) {
                    V::Array(values) | V::Tuple(values) => {
                        let [V::Str(item), V::Str(key)] = values.as_slice() else { return fail("separators must be a tuple of two strings") };
                        Some((item.to_vec(), key.to_vec()))
                    }
                    V::Null => None,
                    _ => return fail("If set, separators must be a tuple of two strings"),
                };
                let options = JsonOptions { indent, ensure_ascii, separators, sort_keys };
                return self.text(to_json(&operand, &options, 0, true, budget)?);
            }
            "join" => {
                let parts: Vec<Vec<u16>> = match &operand {
                    V::Str(text) => code_points(text).into_iter().map(|point| point.to_vec()).collect(),
                    V::Array(items) | V::Tuple(items) => items.iter().map(|item| join_payload(item, budget)).collect::<Result<_, _>>()?,
                    other => return fail(format!("Cannot apply filter \"join\" to type: {}", other.type_name())),
                };
                let (positional, keywords) = self.eval_arguments(args, env)?;
                let separator = positional.first().cloned().or_else(|| keywords.get(&s("separator")).cloned()).unwrap_or(V::Str(rc("")));
                let V::Str(separator) = &separator else { return fail("separator must be a string") };
                return self.text(units::join(&parts, &separator, budget)?);
            }
            "int" | "float" => {
                let int = filter == "int";
                let (positional, keywords) = self.eval_arguments(args, env)?;
                let default = positional.first().cloned().or_else(|| keywords.get(&s("default")).cloned())
                    .unwrap_or(if int { V::Int(0.0) } else { V::Float(0.0) });
                return match &operand {
                    V::Str(text) => {
                        let value = if int { units::parse_int(text) } else { units::parse_float(text) };
                        Ok(if value.is_nan() { default } else if int { V::Int(value) } else { V::Float(value) })
                    }
                    V::Int(_) | V::Float(_) => Ok(operand.clone()),
                    V::Bool(flag) => {
                        let value = if *flag { 1.0 } else { 0.0 };
                        Ok(if int { V::Int(value) } else { V::Float(value) })
                    }
                    other => fail(format!("Cannot apply filter \"{filter}\" to type: {}", other.type_name())),
                };
            }
            "default" => {
                let (positional, keywords) = self.eval_arguments(args, env)?;
                let default = positional.first().cloned().unwrap_or(V::Str(rc("")));
                let boolean = positional.get(1).cloned().or_else(|| keywords.get(&s("boolean")).cloned()).unwrap_or(V::Bool(false));
                let V::Bool(boolean) = boolean else { return fail("`default` filter flag must be a boolean") };
                if matches!(operand, V::Undefined) || (boolean && !operand.truthy()) { return Ok(default); }
                return Ok(operand);
            }
            _ => {}
        }
        match &operand {
            V::Array(items) | V::Tuple(items) => match filter {
                "sort" => {
                    let (positional, keywords) = self.eval_arguments(args, env)?;
                    let pick = |index: usize, name: &str, fallback: V<'a>| positional.get(index).cloned().or_else(|| keywords.get(&s(name)).cloned()).unwrap_or(fallback);
                    let V::Bool(reverse) = pick(0, "reverse", V::Bool(false)) else { return fail("reverse must be a boolean") };
                    let V::Bool(case_sensitive) = pick(1, "case_sensitive", V::Bool(false)) else { return fail("case_sensitive must be a boolean") };
                    let attribute = pick(2, "attribute", V::Null);
                    let path = match &attribute {
                        V::Str(path) => Some(path.to_vec()),
                        V::Int(index) => Some(units::number(*index, budget)?),
                        V::Null => None,
                        _ => return fail("attribute must be a string, integer, or null"),
                    };
                    let mut keys = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        keys.push(match &path { Some(path) => attribute_value(item, path, budget)?, None => item.clone() });
                    }
                    Ok(array_value(self.sort(items.to_vec(), keys, reverse, case_sensitive)?))
                }
                "selectattr" | "rejectattr" => {
                    let select = filter == "selectattr";
                    if items.iter().any(|item| !item.is_container()) { return fail(format!("`{filter}` can only be applied to array of objects")); }
                    if args.iter().any(|arg| !matches!(arg, Node::Str(_))) { return fail(format!("arguments of `{filter}` must be strings")); }
                    let mut evaluated = Vec::with_capacity(args.len());
                    for arg in args { evaluated.push(self.eval(arg, env)?); }
                    let attr = evaluated.first().cloned();
                    let test_name = evaluated.get(1).cloned();
                    let value = evaluated.get(2).cloned();
                    if let Some(V::Str(test_name)) = &test_name {
                        if !TESTS.iter().any(|known| is(test_name, known)) { return fail(format!("Unknown test: {}", lossy(test_name))); }
                    }
                    let mut filtered = Vec::new();
                    for item in items.iter() {
                        budget.work(1)?;
                        let Some(V::Str(attr)) = &attr else { return fail("Cannot read properties of undefined (reading 'value')") };
                        let found = match item {
                            V::Object(map) | V::Kwargs(map) => map.get(attr).cloned(),
                            V::Namespace(map) => map.borrow().get(attr).cloned(),
                            _ => None,
                        };
                        let result = match found {
                            Some(found) => match &test_name {
                                Some(V::Str(test_name)) => self.test(test_name, &found, value.as_ref())?.unwrap_or(false),
                                _ => found.truthy(),
                            },
                            None => false,
                        };
                        if result == select { filtered.push(item.clone()); }
                    }
                    Ok(array_value(filtered))
                }
                "map" => {
                    let (_, keywords) = self.eval_arguments(args, env)?;
                    let Some(attribute) = keywords.get(&s("attribute")).cloned() else {
                        return fail("`map` expressions without `attribute` set are not currently supported.");
                    };
                    let V::Str(attribute) = &attribute else { return fail("attribute must be a string") };
                    let default = keywords.get(&s("default")).cloned();
                    let mut mapped = Vec::with_capacity(items.len());
                    for item in items.iter() {
                        if !item.is_container() { return fail("items in map must be an object"); }
                        let value = attribute_value(item, &attribute, budget)?;
                        mapped.push(if matches!(value, V::Undefined) { default.clone().unwrap_or(V::Undefined) } else { value });
                    }
                    Ok(array_value(mapped))
                }
                _ => fail(format!("Unknown ArrayValue filter: {filter}")),
            },
            V::Str(text) => match filter {
                "indent" => {
                    let (positional, keywords) = self.eval_arguments(args, env)?;
                    let pick = |index: usize, name: &str, fallback: V<'a>| positional.get(index).cloned().or_else(|| keywords.get(&s(name)).cloned()).unwrap_or(fallback);
                    let V::Int(width) = pick(0, "width", V::Int(4.0)) else { return fail("width must be a number") };
                    let first = pick(1, "first", V::Bool(false)).js_truthy();
                    let blank = pick(2, "blank", V::Bool(false)).js_truthy();
                    if width < 0.0 || width.is_infinite() { return fail("Invalid count value"); }
                    let width = if width.is_nan() { 0 } else { width.trunc() as usize };
                    budget.work(width)?;
                    let prefix = vec![0x20; width];
                    self.text(indent(text, &prefix, first, blank, budget)?)
                }
                "replace" => {
                    let (mut positional, keywords) = self.eval_arguments(args, env)?;
                    positional.push(V::Kwargs(Rc::new(keywords)));
                    self.string_method(StrMethod::Replace, text, &positional)
                }
                _ => fail(format!("Unknown StringValue filter: {filter}")),
            },
            V::Object(map) | V::Kwargs(map) => match object_builtin(filter) {
                Some(method) => {
                    let (mut positional, keywords) = self.eval_arguments(args, env)?;
                    if keywords.len() > 0 { positional.push(V::Kwargs(Rc::new(keywords))); }
                    self.object_method(method, map, &positional)
                }
                None => fail(format!("Unknown ObjectValue filter: {filter}")),
            },
            other => fail(format!("Cannot apply filter \"{filter}\" to type: {}", other.type_name())),
        }
    }
}

fn object_builtin(name: &str) -> Option<ObjMethod> {
    Some(match name { "get" => ObjMethod::Get, "items" => ObjMethod::Items, "keys" => ObjMethod::Keys, "values" => ObjMethod::Values, "dictsort" => ObjMethod::Dictsort, _ => return None })
}

/// `value.builtins.get(name)`.
fn builtin<'a>(value: &V<'a>, name: &[u16]) -> Option<V<'a>> {
    let key = String::from_utf16_lossy(name);
    match value {
        V::Str(text) => {
            let method = match key.as_str() {
                "upper" => StrMethod::Upper,
                "lower" => StrMethod::Lower,
                "strip" => StrMethod::Strip,
                "title" => StrMethod::Title,
                "capitalize" => StrMethod::Capitalize,
                "length" => return Some(V::Int(text.len() as f64)),
                "rstrip" => StrMethod::Rstrip,
                "lstrip" => StrMethod::Lstrip,
                "startswith" => StrMethod::Startswith,
                "endswith" => StrMethod::Endswith,
                "split" => StrMethod::Split,
                "replace" => StrMethod::Replace,
                _ => return None,
            };
            Some(func(Func::Str(method, text.clone())))
        }
        V::Object(map) | V::Kwargs(map) => object_builtin(&key).map(|method| func(Func::Object(method, map.clone()))),
        V::Array(items) | V::Tuple(items) => (key == "length").then(|| V::Int(items.len() as f64)),
        _ => None,
    }
}

/// `getAttributeValue(item, path)`: dotted keys through objects and array indices.
fn attribute_value<'a>(item: &V<'a>, path: &[u16], budget: &Budget) -> Result<V<'a>, JinjaError> {
    let mut value = item.clone();
    for part in units::split(path, &[0x2e], budget)? {
        budget.work(1)?;
        value = match &value {
            V::Object(map) | V::Kwargs(map) => map.get(part).cloned().unwrap_or(V::Undefined),
            V::Namespace(map) => map.borrow().get(part).cloned().unwrap_or(V::Undefined),
            V::Array(items) | V::Tuple(items) => {
                let index = units::parse_int(part);
                if !index.is_nan() && index >= 0.0 && index < items.len() as f64 { items[index as usize].clone() } else { return Ok(V::Undefined) }
            }
            _ => return Ok(V::Undefined),
        };
    }
    Ok(value)
}

/// The `indent` filter body: lines after the first and non-empty lines get `prefix`, unless `first` or `blank`.
fn indent(text: &[u16], prefix: &[u16], first: bool, blank: bool, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    let lines = units::split(text, &[0x0a], budget)?;
    let mut out: Vec<Vec<u16>> = Vec::with_capacity(lines.len());
    for (index, line) in lines.into_iter().enumerate() {
        let keep = (!first && index == 0) || (!blank && line.is_empty());
        out.push(if keep { line.to_vec() } else { units::concat(&[prefix, line], budget)? });
    }
    units::join(&out, &[0x0a], budget)
}

/// `replace(str, old, new, count)` from `utils.ts`: a global `u`-flag literal match, so matches start and end on code
/// points and an empty pattern matches between every code point.
fn replace(text: &[u16], old: &[u16], new: &[u16], count: Option<f64>, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    if count == Some(0.0) { return Ok(text.to_vec()); }
    let mut remaining = match count { None => f64::INFINITY, Some(count) if count < 0.0 => f64::INFINITY, Some(count) => count };
    let points = code_points(text);
    let pattern = code_points(old);
    let mut out: Vec<u16> = Vec::with_capacity(text.len());
    let take = |out: &mut Vec<u16>, matched: &[u16], remaining: &mut f64| -> Result<(), JinjaError> {
        if *remaining > 0.0 {
            *remaining -= 1.0;
            budget.work(new.len())?;
            out.extend_from_slice(new);
        } else {
            out.extend_from_slice(matched);
        }
        if out.len() > budget.limits().max_text_units { return Err(Error::Limit("text-units").into()); }
        Ok(())
    };
    if pattern.is_empty() {
        for point in &points {
            take(&mut out, &[], &mut remaining)?;
            out.extend_from_slice(point);
        }
        take(&mut out, &[], &mut remaining)?;
        return Ok(out);
    }
    let mut at = 0;
    while at < points.len() {
        budget.work(pattern.len())?;
        if points.len() - at >= pattern.len() && points[at..at + pattern.len()] == pattern[..] {
            let matched: Vec<u16> = pattern.concat();
            take(&mut out, &matched, &mut remaining)?;
            at += pattern.len();
        } else {
            out.extend_from_slice(points[at]);
            at += 1;
        }
    }
    Ok(out)
}
