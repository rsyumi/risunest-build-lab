//! `ast.ts`. Statements and expressions share one node type, as the JS parser and interpreter mix them freely.

use crate::units::Str;

#[derive(Debug)]
pub(crate) enum Node {
    Comment,
    If { test: Box<Node>, body: Vec<Node>, alternate: Vec<Node> },
    For { loopvar: Box<Node>, iterable: Box<Node>, body: Vec<Node>, default_block: Vec<Node> },
    Break,
    Continue,
    Set { assignee: Box<Node>, value: Option<Box<Node>>, body: Vec<Node> },
    /// `args` holds `Identifier` and `KeywordArgument` nodes.
    Macro { name: Str, args: Vec<Node>, body: Vec<Node> },
    CallStatement { callee: Box<Node>, call_args: Vec<Node>, caller_args: Option<Vec<Node>>, body: Vec<Node> },
    FilterStatement { filter: Box<Node>, body: Vec<Node> },
    Member { object: Box<Node>, property: Box<Node>, computed: bool },
    Call { callee: Box<Node>, args: Vec<Node> },
    Identifier(Str),
    Integer(f64),
    Float(f64),
    Str(Str),
    Array(Vec<Node>),
    Tuple(Vec<Node>),
    /// `Map<Expression, Expression>` keyed by node, so a repeated key is a second entry.
    Object(Vec<(Node, Node)>),
    Binary { operator: Str, left: Box<Node>, right: Box<Node> },
    Filter { operand: Box<Node>, filter: Box<Node> },
    Select { lhs: Box<Node>, test: Box<Node> },
    Test { operand: Box<Node>, negate: bool, test: Str },
    Unary { operator: Str, argument: Box<Node> },
    Slice { start: Option<Box<Node>>, stop: Option<Box<Node>>, step: Option<Box<Node>> },
    KeywordArgument { key: Str, value: Box<Node> },
    Spread(Box<Node>),
    KeywordSpread(Box<Node>),
    Ternary { condition: Box<Node>, true_expr: Box<Node>, false_expr: Box<Node> },
}

impl Node {
    /// The JS `type` string.
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            Node::Comment => "Comment",
            Node::If { .. } => "If",
            Node::For { .. } => "For",
            Node::Break => "Break",
            Node::Continue => "Continue",
            Node::Set { .. } => "Set",
            Node::Macro { .. } => "Macro",
            Node::CallStatement { .. } => "CallStatement",
            Node::FilterStatement { .. } => "FilterStatement",
            Node::Member { .. } => "MemberExpression",
            Node::Call { .. } => "CallExpression",
            Node::Identifier(_) => "Identifier",
            Node::Integer(_) => "IntegerLiteral",
            Node::Float(_) => "FloatLiteral",
            Node::Str(_) => "StringLiteral",
            Node::Array(_) => "ArrayLiteral",
            Node::Tuple(_) => "TupleLiteral",
            Node::Object(_) => "ObjectLiteral",
            Node::Binary { .. } => "BinaryExpression",
            Node::Filter { .. } => "FilterExpression",
            Node::Select { .. } => "SelectExpression",
            Node::Test { .. } => "TestExpression",
            Node::Unary { .. } => "UnaryExpression",
            Node::Slice { .. } => "SliceExpression",
            Node::KeywordArgument { .. } => "KeywordArgumentExpression",
            Node::Spread(_) => "SpreadExpression",
            Node::KeywordSpread(_) => "KeywordSpreadExpression",
            Node::Ternary { .. } => "Ternary",
        }
    }
}

/// Which of `kwargs` and `varargs` a macro or call block body loads before declaring
/// (`findAccessedSpecialMacroArguments`, in Jinja's AST visit order).
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Special {
    pub(crate) kwargs: bool,
    pub(crate) varargs: bool,
}

struct Visitor {
    declared: Special,
    accessed: Special,
}

fn is_name(name: &[u16], ascii: &str) -> bool { name.iter().copied().eq(ascii.encode_utf16()) }

impl Visitor {
    fn declare_name(&mut self, name: &[u16]) {
        if is_name(name, "kwargs") { self.declared.kwargs = true; }
        if is_name(name, "varargs") { self.declared.varargs = true; }
    }
    fn declare_target(&mut self, target: &Node) {
        match target {
            Node::Identifier(name) => self.declare_name(name),
            Node::Tuple(items) => for item in items { self.declare_target(item); },
            _ => {}
        }
    }
    fn declare_argument(&mut self, argument: &Node) {
        if let Some((name, _)) = parameter(argument) { self.declare_name(name); }
    }
    fn visit_defaults(&mut self, args: &[Node]) {
        for argument in args {
            if let Some((_, Some(default))) = parameter(argument) { self.visit(default); }
        }
    }
    fn visit_filter(&mut self, filter: &Node) {
        if let Node::Call { args, .. } = filter { self.visit_all(args); }
    }
    fn visit_all(&mut self, nodes: &[Node]) { for node in nodes { self.visit(node); } }
    fn visit_option(&mut self, node: &Option<Box<Node>>) { if let Some(node) = node { self.visit(node); } }

    fn visit(&mut self, node: &Node) {
        match node {
            Node::For { loopvar, iterable, body, default_block } => {
                self.declare_target(loopvar);
                if let Node::Select { lhs, test } = iterable.as_ref() {
                    self.visit(lhs);
                    self.visit_all(body);
                    self.visit_all(default_block);
                    self.visit(test);
                    return;
                }
                self.visit(iterable);
                self.visit_all(body);
                self.visit_all(default_block);
            }
            Node::Set { assignee, value, body } => {
                self.declare_target(assignee);
                self.visit_option(value);
                self.visit_all(body);
            }
            Node::Macro { args, body, .. } => {
                for argument in args { self.declare_argument(argument); }
                self.visit_defaults(args);
                self.visit_all(body);
            }
            Node::CallStatement { callee, call_args, caller_args, body } => {
                // `visit(statement.call)` walks the CallExpression's callee, then its args.
                self.visit(callee);
                self.visit_all(call_args);
                let parameters = caller_args.as_deref().unwrap_or(&[]);
                for argument in parameters { self.declare_argument(argument); }
                self.visit_defaults(parameters);
                self.visit_all(body);
            }
            Node::FilterStatement { filter, body } => {
                self.visit_all(body);
                self.visit_filter(filter);
            }
            Node::Identifier(name) => {
                if is_name(name, "kwargs") && !self.declared.kwargs { self.accessed.kwargs = true; }
                if is_name(name, "varargs") && !self.declared.varargs { self.accessed.varargs = true; }
            }
            Node::Member { object, property, computed } => {
                self.visit(object);
                if *computed { self.visit(property); }
            }
            Node::Filter { operand, filter } => {
                self.visit(operand);
                self.visit_filter(filter);
            }
            Node::Test { operand, .. } => self.visit(operand),
            Node::Select { lhs, test } => {
                self.visit(test);
                self.visit(lhs);
            }
            Node::KeywordArgument { value, .. } => self.visit(value),
            // `Object.values(node)` in constructor parameter order.
            Node::If { test, body, alternate } => {
                self.visit(test);
                self.visit_all(body);
                self.visit_all(alternate);
            }
            Node::Call { callee, args } => {
                self.visit(callee);
                self.visit_all(args);
            }
            Node::Array(items) | Node::Tuple(items) => self.visit_all(items),
            Node::Object(entries) => for (key, value) in entries {
                self.visit(key);
                self.visit(value);
            },
            Node::Binary { left, right, .. } => {
                self.visit(left);
                self.visit(right);
            }
            Node::Unary { argument, .. } | Node::Spread(argument) | Node::KeywordSpread(argument) => self.visit(argument),
            Node::Slice { start, stop, step } => {
                self.visit_option(start);
                self.visit_option(stop);
                self.visit_option(step);
            }
            Node::Ternary { condition, true_expr, false_expr } => {
                self.visit(condition);
                self.visit(true_expr);
                self.visit(false_expr);
            }
            Node::Comment | Node::Break | Node::Continue | Node::Integer(_) | Node::Float(_) | Node::Str(_) => {}
        }
    }
}

/// `getParameterDescriptor`: the name and default of an `Identifier` or `KeywordArgument` parameter.
pub(crate) fn parameter(argument: &Node) -> Option<(&Str, Option<&Node>)> {
    match argument {
        Node::Identifier(name) => Some((name, None)),
        Node::KeywordArgument { key, value } => Some((key, Some(value))),
        _ => None,
    }
}

pub(crate) fn special_arguments(parameters: &[Node], body: &[Node]) -> Special {
    let mut visitor = Visitor { declared: Special::default(), accessed: Special::default() };
    for parameter in parameters { visitor.declare_argument(parameter); }
    visitor.visit_all(body);
    visitor.accessed
}

impl Node {
    // Moves every child out so that dropping a deep tree never recurses.
    fn take_children(&mut self, out: &mut Vec<Node>) {
        fn boxed(node: &mut Box<Node>, out: &mut Vec<Node>) { out.push(std::mem::replace(&mut **node, Node::Comment)); }
        fn optional(node: &mut Option<Box<Node>>, out: &mut Vec<Node>) { if let Some(node) = node.take() { out.push(*node); } }
        match self {
            Node::Comment | Node::Break | Node::Continue | Node::Identifier(_) | Node::Integer(_) | Node::Float(_) | Node::Str(_) => {}
            Node::If { test, body, alternate } => {
                boxed(test, out);
                out.append(body);
                out.append(alternate);
            }
            Node::For { loopvar, iterable, body, default_block } => {
                boxed(loopvar, out);
                boxed(iterable, out);
                out.append(body);
                out.append(default_block);
            }
            Node::Set { assignee, value, body } => {
                boxed(assignee, out);
                optional(value, out);
                out.append(body);
            }
            Node::Macro { args, body, .. } => {
                out.append(args);
                out.append(body);
            }
            Node::CallStatement { callee, call_args, caller_args, body } => {
                boxed(callee, out);
                out.append(call_args);
                if let Some(args) = caller_args { out.append(args); }
                out.append(body);
            }
            Node::FilterStatement { filter, body } => {
                boxed(filter, out);
                out.append(body);
            }
            Node::Member { object, property, .. } => {
                boxed(object, out);
                boxed(property, out);
            }
            Node::Call { callee, args } => {
                boxed(callee, out);
                out.append(args);
            }
            Node::Array(items) | Node::Tuple(items) => out.append(items),
            Node::Object(entries) => for (key, value) in std::mem::take(entries) {
                out.push(key);
                out.push(value);
            },
            Node::Binary { left, right, .. } => {
                boxed(left, out);
                boxed(right, out);
            }
            Node::Filter { operand, filter } => {
                boxed(operand, out);
                boxed(filter, out);
            }
            Node::Select { lhs, test } => {
                boxed(lhs, out);
                boxed(test, out);
            }
            Node::Test { operand, .. } | Node::Unary { argument: operand, .. } | Node::KeywordArgument { value: operand, .. }
            | Node::Spread(operand) | Node::KeywordSpread(operand) => boxed(operand, out),
            Node::Slice { start, stop, step } => {
                optional(start, out);
                optional(stop, out);
                optional(step, out);
            }
            Node::Ternary { condition, true_expr, false_expr } => {
                boxed(condition, out);
                boxed(true_expr, out);
                boxed(false_expr, out);
            }
        }
    }

    /// Visits every child once, without recursion, for the depth check.
    fn children<'n>(&'n self, out: &mut Vec<(&'n Node, usize)>, depth: usize) {
        let mut push = |node: &'n Node| out.push((node, depth + 1));
        match self {
            Node::Comment | Node::Break | Node::Continue | Node::Identifier(_) | Node::Integer(_) | Node::Float(_) | Node::Str(_) => {}
            Node::If { test, body, alternate } => { push(test); body.iter().for_each(&mut push); alternate.iter().for_each(&mut push); }
            Node::For { loopvar, iterable, body, default_block } => {
                push(loopvar); push(iterable); body.iter().for_each(&mut push); default_block.iter().for_each(&mut push);
            }
            Node::Set { assignee, value, body } => { push(assignee); if let Some(value) = value { push(value); } body.iter().for_each(&mut push); }
            Node::Macro { args, body, .. } => { args.iter().for_each(&mut push); body.iter().for_each(&mut push); }
            Node::CallStatement { callee, call_args, caller_args, body } => {
                push(callee); call_args.iter().for_each(&mut push);
                if let Some(args) = caller_args { args.iter().for_each(&mut push); }
                body.iter().for_each(&mut push);
            }
            Node::FilterStatement { filter, body } => { push(filter); body.iter().for_each(&mut push); }
            Node::Member { object, property, .. } => { push(object); push(property); }
            Node::Call { callee, args } => { push(callee); args.iter().for_each(&mut push); }
            Node::Array(items) | Node::Tuple(items) => items.iter().for_each(&mut push),
            Node::Object(entries) => for (key, value) in entries { push(key); push(value); },
            Node::Binary { left, right, .. } => { push(left); push(right); }
            Node::Filter { operand, filter } => { push(operand); push(filter); }
            Node::Select { lhs, test } => { push(lhs); push(test); }
            Node::Test { operand, .. } | Node::Unary { argument: operand, .. } | Node::KeywordArgument { value: operand, .. }
            | Node::Spread(operand) | Node::KeywordSpread(operand) => push(operand),
            Node::Slice { start, stop, step } => for node in [start, stop, step].into_iter().flatten() { push(node); },
            Node::Ternary { condition, true_expr, false_expr } => { push(condition); push(true_expr); push(false_expr); }
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let mut stack = Vec::new();
        self.take_children(&mut stack);
        while let Some(mut node) = stack.pop() { node.take_children(&mut stack); }
    }
}

/// The deepest nesting in `program`, a top-level statement being depth 1.
pub(crate) fn depth(program: &[Node], budget: &risunest_ir::Budget) -> Result<usize, risunest_ir::Error> {
    let mut stack: Vec<(&Node, usize)> = program.iter().map(|node| (node, 1)).collect();
    let mut deepest = 0;
    while let Some((node, level)) = stack.pop() {
        budget.work(1)?;
        deepest = deepest.max(level);
        node.children(&mut stack, level);
    }
    Ok(deepest)
}
