//! `parser.ts`, function for function. Reading past the last token is an error, as the JS `tokens[current].type`
//! TypeError is.

use crate::ast::{self, Node};
use crate::error::{syntax, too_deep, JinjaError};
use crate::lexer::{Kind, Token};
use crate::units::{lossy, Str};
use risunest_ir::Budget;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Call, Parameters }

struct Parser<'b> {
    tokens: Vec<Token>,
    current: usize,
    budget: &'b Budget,
    depth: usize,
}

type P<T> = Result<T, JinjaError>;

fn name(value: &[u16]) -> Str { Rc::from(value) }
fn eq(value: &[u16], ascii: &str) -> bool { value.iter().copied().eq(ascii.encode_utf16()) }
fn boxed(node: Node) -> Box<Node> { Box::new(node) }

pub(crate) fn parse(tokens: Vec<Token>, budget: &Budget) -> P<Vec<Node>> {
    let mut parser = Parser { tokens, current: 0, budget, depth: 0 };
    let mut program = Vec::new();
    while parser.current < parser.tokens.len() {
        program.push(parser.parse_any()?);
    }
    // Evaluation recurses along the tree, so a left-associative chain counts as nesting too.
    if ast::depth(&program, budget)? > budget.limits().max_depth.saturating_mul(4) { return Err(too_deep()); }
    Ok(program)
}

impl Parser<'_> {
    fn enter(&mut self) -> P<()> {
        self.budget.work(1)?;
        self.depth += 1;
        if self.depth > self.budget.limits().max_depth { return Err(too_deep()); }
        Ok(())
    }
    fn leave(&mut self) { self.depth -= 1; }

    fn token(&self, at: usize) -> P<&Token> {
        self.tokens.get(at).ok_or_else(|| syntax("Unexpected end of template"))
    }
    fn kind(&self) -> P<Kind> { Ok(self.token(self.current)?.kind) }

    fn expect(&mut self, kind: Kind, error: &str) -> P<Token> {
        let token = self.token(self.current)?.clone();
        self.current += 1;
        if token.kind != kind { return Err(syntax(format!("Parser Error: {error}. {:?} !== {kind:?}.", token.kind))); }
        Ok(token)
    }
    fn expect_identifier(&mut self, value: &str) -> P<()> {
        if !self.is_identifier(&[value]) { return Err(syntax(format!("Expected {value}"))); }
        self.current += 1;
        Ok(())
    }
    fn is(&self, kinds: &[Kind]) -> bool {
        self.current + kinds.len() <= self.tokens.len() && kinds.iter().enumerate().all(|(i, kind)| self.tokens[self.current + i].kind == *kind)
    }
    fn is_statement(&self, names: &[&str]) -> bool {
        self.tokens.get(self.current).is_some_and(|token| token.kind == Kind::OpenStatement)
            && self.tokens.get(self.current + 1).is_some_and(|token| token.kind == Kind::Identifier && names.iter().any(|name| eq(&token.value, name)))
    }
    fn is_identifier(&self, names: &[&str]) -> bool {
        self.current + names.len() <= self.tokens.len()
            && names.iter().enumerate().all(|(i, name)| {
                let token = &self.tokens[self.current + i];
                token.kind == Kind::Identifier && eq(&token.value, name)
            })
    }

    fn parse_any(&mut self) -> P<Node> {
        self.enter()?;
        let result = match self.kind()? {
            Kind::Comment => {
                self.current += 1;
                Ok(Node::Comment)
            }
            Kind::Text => Ok(Node::Str(name(&self.expect(Kind::Text, "Expected text token")?.value))),
            Kind::OpenStatement => self.parse_jinja_statement(),
            Kind::OpenExpression => self.parse_jinja_expression(),
            other => Err(syntax(format!("Unexpected token type: {other:?}"))),
        };
        self.leave();
        result
    }

    fn parse_body_until(&mut self, names: &[&str]) -> P<Vec<Node>> {
        let mut body = Vec::new();
        while !self.is_statement(names) { body.push(self.parse_any()?); }
        Ok(body)
    }

    fn parse_jinja_statement(&mut self) -> P<Node> {
        self.expect(Kind::OpenStatement, "Expected opening statement token")?;
        let token = self.token(self.current)?;
        if token.kind != Kind::Identifier { return Err(syntax(format!("Unknown statement, got {:?}", token.kind))); }
        let statement = token.value.clone();
        let result = if eq(&statement, "set") {
            self.current += 1;
            self.parse_set_statement()?
        } else if eq(&statement, "if") {
            self.current += 1;
            let result = self.parse_if_statement()?;
            self.expect(Kind::OpenStatement, "Expected {% token")?;
            self.expect_identifier("endif")?;
            self.expect(Kind::CloseStatement, "Expected %} token")?;
            result
        } else if eq(&statement, "macro") {
            self.current += 1;
            let result = self.parse_macro_statement()?;
            self.expect(Kind::OpenStatement, "Expected {% token")?;
            self.expect_identifier("endmacro")?;
            self.expect(Kind::CloseStatement, "Expected %} token")?;
            result
        } else if eq(&statement, "for") {
            self.current += 1;
            let result = self.parse_for_statement()?;
            self.expect(Kind::OpenStatement, "Expected {% token")?;
            self.expect_identifier("endfor")?;
            self.expect(Kind::CloseStatement, "Expected %} token")?;
            result
        } else if eq(&statement, "call") {
            self.current += 1;
            let caller_args = if self.is(&[Kind::OpenParen]) { Some(self.parse_args(Mode::Parameters)?) } else { None };
            let callee = self.parse_primary_expression()?;
            if !matches!(callee, Node::Identifier(_)) { return Err(syntax("Expected identifier following call statement")); }
            let call_args = self.parse_args(Mode::Call)?;
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            let body = self.parse_body_until(&["endcall"])?;
            self.expect(Kind::OpenStatement, "Expected '{%'")?;
            self.expect_identifier("endcall")?;
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            Node::CallStatement { callee: boxed(callee), call_args, caller_args, body }
        } else if eq(&statement, "break") {
            self.current += 1;
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            Node::Break
        } else if eq(&statement, "continue") {
            self.current += 1;
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            Node::Continue
        } else if eq(&statement, "filter") {
            self.current += 1;
            let mut filter = self.parse_primary_expression()?;
            if matches!(filter, Node::Identifier(_)) && self.is(&[Kind::OpenParen]) { filter = self.parse_call_expression(filter)?; }
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            let body = self.parse_body_until(&["endfilter"])?;
            self.expect(Kind::OpenStatement, "Expected '{%'")?;
            self.expect_identifier("endfilter")?;
            self.expect(Kind::CloseStatement, "Expected '%}'")?;
            Node::FilterStatement { filter: boxed(filter), body }
        } else {
            return Err(syntax(format!("Unknown statement type: {}", lossy(&statement))));
        };
        Ok(result)
    }

    fn parse_jinja_expression(&mut self) -> P<Node> {
        self.expect(Kind::OpenExpression, "Expected opening expression token")?;
        let result = self.parse_expression()?;
        self.expect(Kind::CloseExpression, "Expected closing expression token")?;
        Ok(result)
    }

    fn parse_set_statement(&mut self) -> P<Node> {
        let left = self.parse_expression_sequence(false)?;
        let mut value = None;
        let mut body = Vec::new();
        if self.is(&[Kind::Equals]) {
            self.current += 1;
            value = Some(boxed(self.parse_expression_sequence(false)?));
        } else {
            self.expect(Kind::CloseStatement, "Expected %} token")?;
            body = self.parse_body_until(&["endset"])?;
            self.expect(Kind::OpenStatement, "Expected {% token")?;
            self.expect_identifier("endset")?;
        }
        self.expect(Kind::CloseStatement, "Expected closing statement token")?;
        Ok(Node::Set { assignee: boxed(left), value, body })
    }

    fn parse_if_statement(&mut self) -> P<Node> {
        let test = self.parse_expression()?;
        self.expect(Kind::CloseStatement, "Expected closing statement token")?;
        let body = self.parse_body_until(&["elif", "else", "endif"])?;
        let mut alternate = Vec::new();
        if self.is_statement(&["elif"]) {
            self.current += 2;
            // Each `elif` nests one more `If`, as `parse_any` does for a statement in a body.
            self.enter()?;
            alternate.push(self.parse_if_statement()?);
            self.leave();
        } else if self.is_statement(&["else"]) {
            self.current += 2;
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            alternate = self.parse_body_until(&["endif"])?;
        }
        Ok(Node::If { test: boxed(test), body, alternate })
    }

    fn parse_macro_statement(&mut self) -> P<Node> {
        let name = self.parse_primary_expression()?;
        let Node::Identifier(name) = &name else { return Err(syntax("Expected identifier following macro statement")) };
        let name = name.clone();
        let args = self.parse_args(Mode::Parameters)?;
        self.expect(Kind::CloseStatement, "Expected closing statement token")?;
        let body = self.parse_body_until(&["endmacro"])?;
        Ok(Node::Macro { name, args, body })
    }

    fn parse_expression_sequence(&mut self, primary: bool) -> P<Node> {
        let parse = |parser: &mut Self| if primary { parser.parse_primary_expression() } else { parser.parse_expression() };
        let first = parse(self)?;
        let tuple = self.is(&[Kind::Comma]);
        if !tuple { return Ok(first); }
        let mut expressions = vec![first];
        loop {
            self.current += 1;
            expressions.push(parse(self)?);
            if !self.is(&[Kind::Comma]) { break; }
        }
        Ok(Node::Tuple(expressions))
    }

    fn parse_for_statement(&mut self) -> P<Node> {
        let loopvar = self.parse_expression_sequence(true)?;
        if !matches!(loopvar, Node::Identifier(_) | Node::Tuple(_)) {
            return Err(syntax(format!("Expected identifier/tuple for the loop variable, got {} instead", loopvar.type_name())));
        }
        if !self.is_identifier(&["in"]) { return Err(syntax("Expected `in` keyword following loop variable")); }
        self.current += 1;
        let iterable = self.parse_expression()?;
        self.expect(Kind::CloseStatement, "Expected closing statement token")?;
        let body = self.parse_body_until(&["endfor", "else"])?;
        let mut default_block = Vec::new();
        if self.is_statement(&["else"]) {
            self.current += 2;
            self.expect(Kind::CloseStatement, "Expected closing statement token")?;
            default_block = self.parse_body_until(&["endfor"])?;
        }
        Ok(Node::For { loopvar: boxed(loopvar), iterable: boxed(iterable), body, default_block })
    }

    fn parse_expression(&mut self) -> P<Node> { self.parse_if_expression() }

    fn parse_if_expression(&mut self) -> P<Node> {
        let a = self.parse_logical_or_expression()?;
        if self.is_identifier(&["if"]) {
            self.current += 1;
            let test = self.parse_logical_or_expression()?;
            if self.is_identifier(&["else"]) {
                self.current += 1;
                self.enter()?;
                let false_expr = self.parse_if_expression()?;
                self.leave();
                return Ok(Node::Ternary { condition: boxed(test), true_expr: boxed(a), false_expr: boxed(false_expr) });
            }
            return Ok(Node::Select { lhs: boxed(a), test: boxed(test) });
        }
        Ok(a)
    }

    fn binary(&mut self, left: Node, operator: &[u16], next: fn(&mut Self) -> P<Node>) -> P<Node> {
        self.budget.work(1)?;
        let right = next(self)?;
        Ok(Node::Binary { operator: name(operator), left: boxed(left), right: boxed(right) })
    }

    fn parse_logical_or_expression(&mut self) -> P<Node> {
        let mut left = self.parse_logical_and_expression()?;
        while self.is_identifier(&["or"]) {
            self.current += 1;
            left = self.binary(left, &crate::units::s("or"), Self::parse_logical_and_expression)?;
        }
        Ok(left)
    }

    fn parse_logical_and_expression(&mut self) -> P<Node> {
        let mut left = self.parse_logical_negation_expression()?;
        while self.is_identifier(&["and"]) {
            self.current += 1;
            left = self.binary(left, &crate::units::s("and"), Self::parse_logical_negation_expression)?;
        }
        Ok(left)
    }

    fn parse_logical_negation_expression(&mut self) -> P<Node> {
        // `not a not b` keeps only the last negation, as the JS loop overwrites `right`.
        let mut right = None;
        while self.is_identifier(&["not"]) {
            self.current += 1;
            self.enter()?;
            let argument = self.parse_logical_negation_expression()?;
            self.leave();
            right = Some(Node::Unary { operator: name(&crate::units::s("not")), argument: boxed(argument) });
        }
        match right {
            Some(node) => Ok(node),
            None => self.parse_comparison_expression(),
        }
    }

    fn parse_comparison_expression(&mut self) -> P<Node> {
        let mut left = self.parse_additive_expression()?;
        loop {
            let operator = if self.is_identifier(&["not", "in"]) {
                self.current += 2;
                crate::units::s("not in")
            } else if self.is_identifier(&["in"]) || self.is(&[Kind::ComparisonBinaryOperator]) {
                let value = self.tokens[self.current].value.clone();
                self.current += 1;
                value
            } else {
                break;
            };
            left = self.binary(left, &operator, Self::parse_additive_expression)?;
        }
        Ok(left)
    }

    fn parse_additive_expression(&mut self) -> P<Node> {
        let mut left = self.parse_multiplicative_expression()?;
        while self.is(&[Kind::AdditiveBinaryOperator]) {
            let operator = self.tokens[self.current].value.clone();
            self.current += 1;
            left = self.binary(left, &operator, Self::parse_multiplicative_expression)?;
        }
        Ok(left)
    }

    fn parse_call_member_expression(&mut self) -> P<Node> {
        let primary = self.parse_primary_expression()?;
        let member = self.parse_member_expression(primary)?;
        if self.is(&[Kind::OpenParen]) { return self.parse_call_expression(member); }
        Ok(member)
    }

    fn parse_call_expression(&mut self, callee: Node) -> P<Node> {
        self.enter()?;
        let args = self.parse_args(Mode::Call)?;
        let mut expression = self.parse_member_expression(Node::Call { callee: boxed(callee), args })?;
        if self.is(&[Kind::OpenParen]) { expression = self.parse_call_expression(expression)?; }
        self.leave();
        Ok(expression)
    }

    fn parse_args(&mut self, mode: Mode) -> P<Vec<Node>> {
        self.expect(Kind::OpenParen, "Expected opening parenthesis for arguments list")?;
        let args = self.parse_arguments_list(mode)?;
        self.expect(Kind::CloseParen, "Expected closing parenthesis for arguments list")?;
        Ok(args)
    }

    fn parse_arguments_list(&mut self, mode: Mode) -> P<Vec<Node>> {
        let mut args = Vec::new();
        let mut parameter_names: Vec<Str> = Vec::new();
        let mut saw_keyword_or_default = false;
        let mut saw_spread = false;
        while !self.is(&[Kind::CloseParen]) {
            self.budget.work(1)?;
            let keyword_spread = self.is(&[Kind::ExponentiationBinaryOperator]);
            let positional_spread = self.is(&[Kind::MultiplicativeBinaryOperator]) && eq(&self.tokens[self.current].value, "*");
            if mode == Mode::Parameters && (keyword_spread || positional_spread) {
                return Err(syntax("Argument unpacking is not allowed in parameter declarations"));
            }
            if keyword_spread {
                self.current += 1;
                args.push(Node::KeywordSpread(boxed(self.parse_expression()?)));
                if self.is(&[Kind::Comma]) { self.current += 1; }
                if !self.is(&[Kind::CloseParen]) {
                    return Err(syntax("Expected closing parenthesis: `**` must be applied to the final argument"));
                }
                break;
            }
            let argument = if positional_spread {
                if saw_spread { return Err(syntax("Only one `*` argument unpacking is allowed")); }
                saw_spread = true;
                self.current += 1;
                Node::Spread(boxed(self.parse_expression()?))
            } else {
                let mut argument = self.parse_expression()?;
                if self.is(&[Kind::Equals]) {
                    self.current += 1;
                    let Node::Identifier(key) = &argument else {
                        return Err(syntax(if mode == Mode::Parameters { "Expected identifier for parameter declaration" } else { "Expected identifier for keyword argument" }));
                    };
                    let key = key.clone();
                    let value = self.parse_expression()?;
                    argument = Node::KeywordArgument { key, value: boxed(value) };
                }
                if mode == Mode::Parameters {
                    let parameter_name = match &argument {
                        Node::Identifier(value) => value.clone(),
                        Node::KeywordArgument { key, .. } => key.clone(),
                        _ => return Err(syntax("Expected identifier for parameter declaration")),
                    };
                    self.budget.work(parameter_names.len())?;
                    if parameter_names.iter().any(|existing| existing[..] == parameter_name[..]) {
                        return Err(syntax(format!("Duplicate parameter name: {}", lossy(&parameter_name))));
                    }
                    parameter_names.push(parameter_name);
                }
                if matches!(argument, Node::KeywordArgument { .. }) {
                    saw_keyword_or_default = true;
                } else if saw_keyword_or_default {
                    return Err(syntax(if mode == Mode::Call { "Positional arguments must come before keyword arguments" } else { "Non-default argument follows default argument" }));
                } else if saw_spread {
                    return Err(syntax("Positional arguments must not follow `*` argument unpacking"));
                }
                argument
            };
            args.push(argument);
            if self.is(&[Kind::Comma]) { self.current += 1; }
        }
        Ok(args)
    }

    fn parse_member_expression_arguments_list(&mut self) -> P<Node> {
        let mut slices: Vec<Option<Node>> = Vec::new();
        let mut is_slice = false;
        while !self.is(&[Kind::CloseSquareBracket]) {
            self.budget.work(1)?;
            if self.is(&[Kind::Colon]) {
                slices.push(None);
                self.current += 1;
                is_slice = true;
            } else {
                slices.push(Some(self.parse_expression()?));
                if self.is(&[Kind::Colon]) {
                    self.current += 1;
                    is_slice = true;
                }
            }
        }
        if slices.is_empty() { return Err(syntax("Expected at least one argument for member/slice expression")); }
        if is_slice {
            if slices.len() > 3 { return Err(syntax("Expected 0-3 arguments for slice expression")); }
            let mut parts = slices.into_iter();
            let start = parts.next().flatten().map(boxed);
            let stop = parts.next().flatten().map(boxed);
            let step = parts.next().flatten().map(boxed);
            return Ok(Node::Slice { start, stop, step });
        }
        // `slices[0]` of a non-slice list: the first expression (an `undefined` first entry implies a slice).
        Ok(slices.into_iter().next().flatten().expect("a non-slice list starts with an expression"))
    }

    fn parse_member_expression(&mut self, mut object: Node) -> P<Node> {
        while self.is(&[Kind::Dot]) || self.is(&[Kind::OpenSquareBracket]) {
            self.budget.work(1)?;
            let computed = self.tokens[self.current].kind == Kind::OpenSquareBracket;
            self.current += 1;
            let property = if computed {
                self.enter()?;
                let property = self.parse_member_expression_arguments_list()?;
                self.leave();
                self.expect(Kind::CloseSquareBracket, "Expected closing square bracket")?;
                property
            } else {
                let property = self.parse_primary_expression()?;
                if !matches!(property, Node::Identifier(_) | Node::Integer(_)) {
                    return Err(syntax("Expected identifier or integer following dot operator"));
                }
                property
            };
            object = Node::Member { object: boxed(object), property: boxed(property), computed };
        }
        Ok(object)
    }

    fn parse_multiplicative_expression(&mut self) -> P<Node> {
        let mut left = self.parse_power_expression()?;
        while self.is(&[Kind::MultiplicativeBinaryOperator]) {
            let operator = self.tokens[self.current].value.clone();
            self.current += 1;
            left = self.binary(left, &operator, Self::parse_power_expression)?;
        }
        Ok(left)
    }

    fn parse_power_expression(&mut self) -> P<Node> {
        let mut left = self.parse_test_expression()?;
        while self.is(&[Kind::ExponentiationBinaryOperator]) {
            let operator = self.tokens[self.current].value.clone();
            self.current += 1;
            left = self.binary(left, &operator, Self::parse_test_expression)?;
        }
        Ok(left)
    }

    fn parse_test_expression(&mut self) -> P<Node> {
        let mut operand = self.parse_filter_expression()?;
        while self.is_identifier(&["is"]) {
            self.budget.work(1)?;
            self.current += 1;
            let negate = self.is_identifier(&["not"]);
            if negate { self.current += 1; }
            let filter = self.parse_primary_expression()?;
            let Node::Identifier(test) = &filter else { return Err(syntax("Expected identifier for the test")) };
            let test = test.clone();
            operand = Node::Test { operand: boxed(operand), negate, test };
        }
        Ok(operand)
    }

    fn parse_filter_expression(&mut self) -> P<Node> {
        let mut operand = self.parse_unary_sign_expression()?;
        while self.is(&[Kind::Pipe]) {
            self.budget.work(1)?;
            self.current += 1;
            let mut filter = self.parse_primary_expression()?;
            if !matches!(filter, Node::Identifier(_)) { return Err(syntax("Expected identifier for the filter")); }
            if self.is(&[Kind::OpenParen]) { filter = self.parse_call_expression(filter)?; }
            operand = Node::Filter { operand: boxed(operand), filter: boxed(filter) };
        }
        Ok(operand)
    }

    fn parse_unary_sign_expression(&mut self) -> P<Node> {
        if let Some(token) = self.tokens.get(self.current) {
            if matches!(token.kind, Kind::UnaryOperator | Kind::AdditiveBinaryOperator) && (eq(&token.value, "-") || eq(&token.value, "+")) {
                let operator = token.value.clone();
                self.current += 1;
                self.enter()?;
                let argument = self.parse_unary_sign_expression()?;
                self.leave();
                return Ok(Node::Unary { operator: name(&operator), argument: boxed(argument) });
            }
        }
        self.parse_call_member_expression()
    }

    fn parse_primary_expression(&mut self) -> P<Node> {
        let token = self.token(self.current)?.clone();
        self.current += 1;
        match token.kind {
            Kind::NumericLiteral => {
                let text = String::from_utf16_lossy(&token.value);
                let value: f64 = text.parse().unwrap_or(f64::NAN);
                Ok(if token.value.contains(&0x2e) { Node::Float(value) } else { Node::Integer(value) })
            }
            Kind::StringLiteral => {
                let mut value = token.value;
                while self.is(&[Kind::StringLiteral]) {
                    let next = &self.tokens[self.current].value;
                    self.budget.work(value.len() + next.len())?;
                    value.extend_from_slice(next);
                    self.current += 1;
                }
                Ok(Node::Str(crate::units::finish(value, self.budget)?))
            }
            Kind::Identifier => Ok(Node::Identifier(name(&token.value))),
            Kind::OpenParen => {
                self.enter()?;
                let expression = self.parse_expression_sequence(false)?;
                self.leave();
                self.expect(Kind::CloseParen, "Expected closing parenthesis")?;
                Ok(expression)
            }
            Kind::OpenSquareBracket => {
                self.enter()?;
                let mut values = Vec::new();
                while !self.is(&[Kind::CloseSquareBracket]) {
                    values.push(self.parse_expression()?);
                    if self.is(&[Kind::Comma]) { self.current += 1; }
                }
                self.current += 1;
                self.leave();
                Ok(Node::Array(values))
            }
            Kind::OpenCurlyBracket => {
                self.enter()?;
                let mut values = Vec::new();
                while !self.is(&[Kind::CloseCurlyBracket]) {
                    let key = self.parse_expression()?;
                    self.expect(Kind::Colon, "Expected colon between key and value in object literal")?;
                    let value = self.parse_expression()?;
                    values.push((key, value));
                    if self.is(&[Kind::Comma]) { self.current += 1; }
                }
                self.current += 1;
                self.leave();
                Ok(Node::Object(values))
            }
            other => Err(syntax(format!("Unexpected token: {other:?}"))),
        }
    }
}
