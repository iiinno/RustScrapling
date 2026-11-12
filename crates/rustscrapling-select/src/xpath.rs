//! XPath 1.0 engine over the arena DOM (Plan section 5: own
//! implementation for the default mode; compat mode uses libxml2 in
//! `rustscrapling-dom::compat`).
//!
//! Implements location paths, all axes, node tests, predicates, the
//! union operator, variables, and the XPath 1.0 core function library.
//! Final node-sets are returned in document order, as lxml does, while
//! predicates on reverse axes see nodes nearest-first, as the spec
//! requires.

use crate::{syntax, Result, SelectError, Selection, SelectionNode};
use rustscrapling_dom::{Dom, NodeId, NodeKind};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum XNode {
    Node(NodeId),
    Attr(NodeId, usize),
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Nodes(Vec<XNode>),
    Str(String),
    Num(f64),
    Bool(bool),
}

impl Value {
    fn to_bool(&self, _dom: &Dom) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::Str(s) => !s.is_empty(),
            Value::Nodes(ns) => !ns.is_empty(),
        }
    }
    fn to_number(&self, dom: &Dom) -> f64 {
        match self {
            Value::Num(n) => *n,
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Str(s) => parse_xpath_number(s),
            Value::Nodes(ns) => ns
                .first()
                .map(|n| parse_xpath_number(&string_value(dom, *n)))
                .unwrap_or(f64::NAN),
        }
    }
    fn to_string(&self, dom: &Dom) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Num(n) => format_xpath_number(*n),
            Value::Bool(b) => b.to_string(),
            Value::Nodes(ns) => ns
                .first()
                .map(|n| string_value(dom, *n))
                .unwrap_or_default(),
        }
    }
}

fn parse_xpath_number(s: &str) -> f64 {
    let t = s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
    // XPath numbers: optional minus, digits with optional fraction.
    let mut chars = t.chars().peekable();
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    let mut seen_digit = false;
    let mut seen_dot = false;
    for c in chars {
        if c.is_ascii_digit() {
            seen_digit = true;
        } else if c == '.' && !seen_dot {
            seen_dot = true;
        } else {
            return f64::NAN;
        }
    }
    if !seen_digit {
        return f64::NAN;
    }
    t.parse().unwrap_or(f64::NAN)
}

fn format_xpath_number(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn string_value(dom: &Dom, n: XNode) -> String {
    match n {
        XNode::Attr(owner, idx) => dom
            .node(owner)
            .attrs
            .get(idx)
            .map(|(_, v)| v.clone())
            .unwrap_or_default(),
        XNode::Node(id) => {
            let node = dom.node(id);
            match node.kind {
                NodeKind::Text | NodeKind::Comment => node.data.clone(),
                _ => dom.text_content(id),
            }
        }
    }
}

fn order_key(n: XNode) -> (u64, u64) {
    match n {
        XNode::Node(id) => (id as u64, 0),
        XNode::Attr(owner, idx) => (owner as u64, idx as u64 + 1),
    }
}

fn sort_document_order(nodes: &mut Vec<XNode>) {
    nodes.sort_by_key(|n| order_key(*n));
    nodes.dedup();
}

// ---------- lexer ----------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Slash,
    SlashSlash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    At,
    Dot,
    DotDot,
    ColonColon,
    Pipe,
    Plus,
    Minus,
    Star,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Comma,
    Dollar,
    Name(String),
    Literal(String),
    Number(f64),
    Eof,
}

fn lex(src: &str) -> Result<Vec<Tok>> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'/' => {
                if b.get(i + 1) == Some(&b'/') {
                    out.push(Tok::SlashSlash);
                    i += 2;
                } else {
                    out.push(Tok::Slash);
                    i += 1;
                }
            }
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b'[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            b']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            b'@' => {
                out.push(Tok::At);
                i += 1;
            }
            b'|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            b'+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            b'-' => {
                out.push(Tok::Minus);
                i += 1;
            }
            b'*' => {
                out.push(Tok::Star);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'$' => {
                out.push(Tok::Dollar);
                i += 1;
            }
            b'=' => {
                out.push(Tok::Eq);
                i += 1;
            }
            b'!' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ne);
                    i += 2;
                } else {
                    return syntax("unexpected '!'");
                }
            }
            b'<' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Le);
                    i += 2;
                } else {
                    out.push(Tok::Lt);
                    i += 1;
                }
            }
            b'>' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            b':' => {
                if b.get(i + 1) == Some(&b':') {
                    out.push(Tok::ColonColon);
                    i += 2;
                } else {
                    // Namespace colon inside a name; handled by name reader.
                    return syntax("unexpected ':'");
                }
            }
            b'.' => {
                if b.get(i + 1) == Some(&b'.') {
                    out.push(Tok::DotDot);
                    i += 2;
                } else if b.get(i + 1).map(|d| d.is_ascii_digit()).unwrap_or(false) {
                    let (n, ni) = lex_number(src, i)?;
                    out.push(Tok::Number(n));
                    i = ni;
                } else {
                    out.push(Tok::Dot);
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let q = c;
                let mut j = i + 1;
                let mut s = String::new();
                while j < b.len() && b[j] != q {
                    let ch_len = utf8_len(b[j]);
                    s.push_str(&src[j..(j + ch_len).min(src.len())]);
                    j += ch_len;
                }
                if j >= b.len() {
                    return syntax("unterminated literal");
                }
                out.push(Tok::Literal(s));
                i = j + 1;
            }
            _ if c.is_ascii_digit() => {
                let (n, ni) = lex_number(src, i)?;
                out.push(Tok::Number(n));
                i = ni;
            }
            _ if is_name_start(c) => {
                let mut j = i;
                let mut name = String::new();
                while j < b.len() {
                    let cj = b[j];
                    if is_name_char(cj) {
                        let ch_len = utf8_len(cj);
                        name.push_str(&src[j..(j + ch_len).min(src.len())]);
                        j += ch_len;
                    } else if cj == b':' && b.get(j + 1) != Some(&b':') {
                        name.push(':');
                        j += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Name(name));
                i = j;
            }
            _ => return syntax(format!("unexpected character '{}' in xpath", c as char)),
        }
    }
    out.push(Tok::Eof);
    Ok(out)
}

fn lex_number(src: &str, start: usize) -> Result<(f64, usize)> {
    let b = src.as_bytes();
    let mut j = start;
    while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
        j += 1;
    }
    let text = &src[start..j];
    text.parse::<f64>()
        .map(|n| (n, j))
        .map_err(|_| SelectError::Syntax(format!("bad number {}", text)))
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' || c >= 0x80
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ---------- AST ----------

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Eq(Box<Expr>, Box<Expr>, bool), // bool: true for '=', false for '!='
    Rel(Box<Expr>, Box<Expr>, RelOp),
    Add(Box<Expr>, Box<Expr>, bool), // true '+', false '-'
    Mul(Box<Expr>, Box<Expr>, MulOp),
    Neg(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    Path { absolute: bool, steps: Vec<Step> },
    Filter(Box<Expr>, Vec<Expr>),
    SlashPath(Box<Expr>, Vec<Step>), // FilterExpr followed by a relative path
    Variable(String),
    Literal(String),
    Number(f64),
    Func(String, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RelOp {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum MulOp {
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, PartialEq)]
struct Step {
    axis: Axis,
    test: NodeTest,
    predicates: Vec<Expr>,
    /// True when this step was preceded by '//' (descendant-or-self
    /// expansion applies before the step).
    via_slashslash: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Child,
    Descendant,
    DescendantOrSelf,
    Ancestor,
    AncestorOrSelf,
    Parent,
    Current,
    FollowingSibling,
    PrecedingSibling,
    Following,
    Preceding,
    Attribute,
    Namespace,
}

#[derive(Debug, Clone, PartialEq)]
enum NodeTest {
    Name(String),
    Wildcard,
    PrefixWildcard(String),
    Node,
    Text,
    Comment,
    Pi(Option<String>),
}

// ---------- parser ----------

struct XParser {
    toks: Vec<Tok>,
    pos: usize,
}

impl XParser {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos]
    }
    fn peek2(&self) -> &Tok {
        self.toks.get(self.pos + 1).unwrap_or(&Tok::Eof)
    }
    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == t {
            self.next();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &Tok) -> Result<()> {
        if self.eat(t) {
            Ok(())
        } else {
            syntax(format!("expected {:?}, got {:?}", t, self.peek()))
        }
    }
    fn peek_name(&self, name: &str) -> bool {
        matches!(self.peek(), Tok::Name(n) if n == name)
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.peek_name("or") {
            self.next();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_equality()?;
        while self.peek_name("and") {
            self.next();
            let right = self.parse_equality()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Expr> {
        let mut left = self.parse_relational()?;
        loop {
            if self.eat(&Tok::Eq) {
                let r = self.parse_relational()?;
                left = Expr::Eq(Box::new(left), Box::new(r), true);
            } else if self.eat(&Tok::Ne) {
                let r = self.parse_relational()?;
                left = Expr::Eq(Box::new(left), Box::new(r), false);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_relational(&mut self) -> Result<Expr> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Tok::Lt => RelOp::Lt,
                Tok::Le => RelOp::Le,
                Tok::Gt => RelOp::Gt,
                Tok::Ge => RelOp::Ge,
                _ => break,
            };
            self.next();
            let r = self.parse_additive()?;
            left = Expr::Rel(Box::new(left), Box::new(r), op);
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr> {
        let mut left = self.parse_multiplicative()?;
        loop {
            if self.eat(&Tok::Plus) {
                let r = self.parse_multiplicative()?;
                left = Expr::Add(Box::new(left), Box::new(r), true);
            } else if self.eat(&Tok::Minus) {
                let r = self.parse_multiplicative()?;
                left = Expr::Add(Box::new(left), Box::new(r), false);
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = if self.eat(&Tok::Star) {
                MulOp::Mul
            } else if self.peek_name("div") {
                self.next();
                MulOp::Div
            } else if self.peek_name("mod") {
                self.next();
                MulOp::Mod
            } else {
                break;
            };
            let r = self.parse_unary()?;
            left = Expr::Mul(Box::new(left), Box::new(r), op);
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if self.eat(&Tok::Minus) {
            let inner = self.parse_unary()?;
            return Ok(Expr::Neg(Box::new(inner)));
        }
        self.parse_union()
    }

    fn parse_union(&mut self) -> Result<Expr> {
        let mut left = self.parse_path_expr()?;
        while self.eat(&Tok::Pipe) {
            let right = self.parse_path_expr()?;
            left = Expr::Union(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_path_expr(&mut self) -> Result<Expr> {
        match self.peek().clone() {
            Tok::Slash => {
                self.next();
                if self.at_step_start() {
                    let steps = self.parse_relative_path(false)?;
                    Ok(Expr::Path {
                        absolute: true,
                        steps,
                    })
                } else {
                    Ok(Expr::Path {
                        absolute: true,
                        steps: Vec::new(),
                    })
                }
            }
            Tok::SlashSlash => {
                self.next();
                let steps = self.parse_relative_path(true)?;
                Ok(Expr::Path {
                    absolute: true,
                    steps,
                })
            }
            _ => {
                // Either a location path (relative) or a filter expression.
                if self.at_step_start() && !self.at_function_call() {
                    let steps = self.parse_relative_path(false)?;
                    return Ok(Expr::Path {
                        absolute: false,
                        steps,
                    });
                }
                let primary = self.parse_primary()?;
                let mut expr = primary;
                // Predicates on a filter expression.
                let preds = self.parse_predicates()?;
                if !preds.is_empty() {
                    expr = Expr::Filter(Box::new(expr), preds);
                }
                // Optional continuation path.
                match self.peek().clone() {
                    Tok::Slash => {
                        self.next();
                        let steps = self.parse_relative_path(false)?;
                        expr = Expr::SlashPath(Box::new(expr), steps);
                    }
                    Tok::SlashSlash => {
                        self.next();
                        let steps = self.parse_relative_path(true)?;
                        expr = Expr::SlashPath(Box::new(expr), steps);
                    }
                    _ => {}
                }
                Ok(expr)
            }
        }
    }

    fn at_function_call(&self) -> bool {
        // Name '(' where the name is not a node-type test is a function
        // call; node tests are handled as steps instead.
        if let Tok::Name(name) = self.peek() {
            if matches!(self.peek2(), Tok::LParen) {
                return !matches!(
                    name.as_str(),
                    "node" | "text" | "comment" | "processing-instruction"
                );
            }
        }
        false
    }

    fn at_step_start(&self) -> bool {
        matches!(self.peek(), Tok::Dot | Tok::DotDot | Tok::At | Tok::Star)
            || matches!(self.peek(), Tok::Name(_))
    }

    fn parse_relative_path(&mut self, first_via_slashslash: bool) -> Result<Vec<Step>> {
        let mut steps = Vec::new();
        let mut via = first_via_slashslash;
        loop {
            let mut step = self.parse_step()?;
            step.via_slashslash = via;
            steps.push(step);
            match self.peek().clone() {
                Tok::Slash => {
                    self.next();
                    via = false;
                }
                Tok::SlashSlash => {
                    self.next();
                    via = true;
                }
                _ => break,
            }
        }
        Ok(steps)
    }

    fn parse_step(&mut self) -> Result<Step> {
        // Abbreviated steps.
        if self.eat(&Tok::Dot) {
            return Ok(Step {
                axis: Axis::Current,
                test: NodeTest::Node,
                predicates: Vec::new(),
                via_slashslash: false,
            });
        }
        if self.eat(&Tok::DotDot) {
            return Ok(Step {
                axis: Axis::Parent,
                test: NodeTest::Node,
                predicates: Vec::new(),
                via_slashslash: false,
            });
        }
        let mut axis = Axis::Child;
        if self.eat(&Tok::At) {
            axis = Axis::Attribute;
        } else if let Tok::Name(name) = self.peek().clone() {
            if self.peek2() == &Tok::ColonColon {
                axis = match name.as_str() {
                    "child" => Axis::Child,
                    "descendant" => Axis::Descendant,
                    "descendant-or-self" => Axis::DescendantOrSelf,
                    "ancestor" => Axis::Ancestor,
                    "ancestor-or-self" => Axis::AncestorOrSelf,
                    "parent" => Axis::Parent,
                    "self" => Axis::Current,
                    "following-sibling" => Axis::FollowingSibling,
                    "preceding-sibling" => Axis::PrecedingSibling,
                    "following" => Axis::Following,
                    "preceding" => Axis::Preceding,
                    "attribute" => Axis::Attribute,
                    "namespace" => Axis::Namespace,
                    _ => return syntax(format!("unknown axis {}", name)),
                };
                self.next();
                self.next();
            }
        }
        let test = self.parse_node_test()?;
        let predicates = self.parse_predicates()?;
        Ok(Step {
            axis,
            test,
            predicates,
            via_slashslash: false,
        })
    }

    fn parse_node_test(&mut self) -> Result<NodeTest> {
        match self.next() {
            Tok::Star => Ok(NodeTest::Wildcard),
            Tok::Name(name) => {
                if self.peek() == &Tok::LParen {
                    match name.as_str() {
                        "node" => {
                            self.next();
                            self.expect(&Tok::RParen)?;
                            Ok(NodeTest::Node)
                        }
                        "text" => {
                            self.next();
                            self.expect(&Tok::RParen)?;
                            Ok(NodeTest::Text)
                        }
                        "comment" => {
                            self.next();
                            self.expect(&Tok::RParen)?;
                            Ok(NodeTest::Comment)
                        }
                        "processing-instruction" => {
                            self.next();
                            let arg = match self.next() {
                                Tok::Literal(s) => Some(s),
                                Tok::RParen => return Ok(NodeTest::Pi(None)),
                                _ => return syntax("bad processing-instruction test"),
                            };
                            self.expect(&Tok::RParen)?;
                            Ok(NodeTest::Pi(arg))
                        }
                        _ => syntax(format!("{} is not a node test", name)),
                    }
                } else if let Some((prefix, _)) = name.split_once(':') {
                    if self.eat(&Tok::Star) {
                        Ok(NodeTest::PrefixWildcard(prefix.to_string()))
                    } else {
                        Ok(NodeTest::Name(name))
                    }
                } else {
                    Ok(NodeTest::Name(name))
                }
            }
            t => syntax(format!("expected node test, got {:?}", t)),
        }
    }

    fn parse_predicates(&mut self) -> Result<Vec<Expr>> {
        let mut preds = Vec::new();
        while self.eat(&Tok::LBracket) {
            let e = self.parse_expr()?;
            self.expect(&Tok::RBracket)?;
            preds.push(e);
        }
        Ok(preds)
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        match self.next() {
            Tok::Dollar => match self.next() {
                Tok::Name(name) => Ok(Expr::Variable(name)),
                t => syntax(format!("expected variable name, got {:?}", t)),
            },
            Tok::LParen => {
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Literal(s) => Ok(Expr::Literal(s)),
            Tok::Number(n) => Ok(Expr::Number(n)),
            Tok::Name(name) => {
                if self.eat(&Tok::LParen) {
                    let mut args = Vec::new();
                    if self.peek() != &Tok::RParen {
                        loop {
                            args.push(self.parse_expr()?);
                            if !self.eat(&Tok::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    Ok(Expr::Func(name, args))
                } else {
                    syntax(format!("unexpected bare name {}", name))
                }
            }
            t => syntax(format!("unexpected token {:?}", t)),
        }
    }
}

// ---------- evaluation ----------

#[derive(Clone)]
struct Ctx {
    node: XNode,
    position: usize,
    size: usize,
    vars: HashMap<String, Value>,
}

pub fn xpath(
    dom: &Dom,
    scope: NodeId,
    expr: &str,
    vars: &[(String, String)],
) -> Result<Vec<Selection>> {
    Ok(select_nodes(dom, scope, expr, vars)?
        .iter()
        .map(|node| node.selection(dom))
        .collect())
}

pub(crate) fn select_nodes(
    dom: &Dom,
    scope: NodeId,
    expr: &str,
    vars: &[(String, String)],
) -> Result<Vec<SelectionNode>> {
    let toks = lex(expr)?;
    let mut parser = XParser { toks, pos: 0 };
    let ast = parser.parse_expr()?;
    if parser.peek() != &Tok::Eof {
        return syntax(format!("trailing tokens in xpath: {:?}", parser.peek()));
    }
    let mut varmap: HashMap<String, Value> = HashMap::new();
    for (k, v) in vars {
        varmap.insert(k.clone(), Value::Str(v.clone()));
    }
    let ctx = Ctx {
        node: XNode::Node(scope),
        position: 1,
        size: 1,
        vars: varmap,
    };
    let value = eval(dom, &ast, &ctx)?;
    match value {
        Value::Nodes(nodes) => Ok(nodes
            .iter()
            .map(|n| match n {
                XNode::Node(id) => SelectionNode::Node(*id),
                XNode::Attr(owner, idx) => {
                    SelectionNode::Attribute(*owner, dom.node(*owner).attrs[*idx].0.clone())
                },
            })
            .collect()),
        other => Err(SelectError::Type(format!(
            "xpath result is not a node-set (got {})",
            other.to_string(dom)
        ))),
    }
}

fn eval(dom: &Dom, expr: &Expr, ctx: &Ctx) -> Result<Value> {
    match expr {
        Expr::Literal(s) => Ok(Value::Str(s.clone())),
        Expr::Number(n) => Ok(Value::Num(*n)),
        Expr::Variable(name) => ctx
            .vars
            .get(name)
            .cloned()
            .ok_or_else(|| SelectError::Syntax(format!("undefined variable ${}", name))),
        Expr::Neg(e) => {
            let v = eval(dom, e, ctx)?;
            Ok(Value::Num(-v.to_number(dom)))
        }
        Expr::Or(a, b) => {
            let va = eval(dom, a, ctx)?.to_bool(dom);
            let vb = eval(dom, b, ctx)?.to_bool(dom);
            Ok(Value::Bool(va || vb))
        }
        Expr::And(a, b) => {
            let va = eval(dom, a, ctx)?.to_bool(dom);
            let vb = eval(dom, b, ctx)?.to_bool(dom);
            Ok(Value::Bool(va && vb))
        }
        Expr::Add(a, b, plus) => {
            let x = eval(dom, a, ctx)?.to_number(dom);
            let y = eval(dom, b, ctx)?.to_number(dom);
            Ok(Value::Num(if *plus { x + y } else { x - y }))
        }
        Expr::Mul(a, b, op) => {
            let x = eval(dom, a, ctx)?.to_number(dom);
            let y = eval(dom, b, ctx)?.to_number(dom);
            Ok(Value::Num(match op {
                MulOp::Mul => x * y,
                MulOp::Div => x / y,
                MulOp::Mod => x % y,
            }))
        }
        Expr::Eq(a, b, is_eq) => {
            let va = eval(dom, a, ctx)?;
            let vb = eval(dom, b, ctx)?;
            let eq = values_equal(dom, &va, &vb);
            Ok(Value::Bool(if *is_eq { eq } else { !eq }))
        }
        Expr::Rel(a, b, op) => {
            let va = eval(dom, a, ctx)?;
            let vb = eval(dom, b, ctx)?;
            Ok(Value::Bool(values_rel(dom, &va, &vb, *op)))
        }
        Expr::Union(a, b) => {
            let va = eval(dom, a, ctx)?;
            let vb = eval(dom, b, ctx)?;
            match (va, vb) {
                (Value::Nodes(mut x), Value::Nodes(y)) => {
                    x.extend(y);
                    sort_document_order(&mut x);
                    Ok(Value::Nodes(x))
                }
                _ => Err(SelectError::Type("union operands must be node-sets".into())),
            }
        }
        Expr::Path { absolute, steps } => {
            let start = if *absolute {
                vec![XNode::Node(dom.root)]
            } else {
                vec![ctx.node]
            };
            let nodes = eval_steps(dom, start, steps, ctx)?;
            Ok(Value::Nodes(nodes))
        }
        Expr::Filter(base, preds) => {
            let base_val = eval(dom, base, ctx)?;
            let nodes = match base_val {
                Value::Nodes(n) => n,
                _ => return Err(SelectError::Type("predicates need a node-set".into())),
            };
            let filtered = apply_predicates(dom, nodes, preds, ctx)?;
            Ok(Value::Nodes(filtered))
        }
        Expr::SlashPath(base, steps) => {
            let base_val = eval(dom, base, ctx)?;
            let nodes = match base_val {
                Value::Nodes(n) => n,
                _ => return Err(SelectError::Type("path base must be a node-set".into())),
            };
            let out = eval_steps_from_bases(dom, nodes, steps, ctx)?;
            Ok(Value::Nodes(out))
        }
        Expr::Func(name, args) => eval_function(dom, name, args, ctx),
    }
}

fn eval_steps(dom: &Dom, start: Vec<XNode>, steps: &[Step], ctx: &Ctx) -> Result<Vec<XNode>> {
    eval_steps_from_bases(dom, start, steps, ctx)
}

fn eval_steps_from_bases(
    dom: &Dom,
    start: Vec<XNode>,
    steps: &[Step],
    ctx: &Ctx,
) -> Result<Vec<XNode>> {
    let mut current = start;
    for step in steps {
        if step.via_slashslash {
            // '//' expands to descendant-or-self::node() before the step.
            let mut expanded = Vec::new();
            for n in &current {
                expanded.extend(axis_nodes(dom, *n, Axis::DescendantOrSelf));
            }
            sort_document_order(&mut expanded);
            current = expanded;
        }
        let mut next: Vec<XNode> = Vec::new();
        for n in &current {
            let axis_list: Vec<XNode> = axis_nodes(dom, *n, step.axis)
                .into_iter()
                .filter(|c| node_test_matches(dom, *c, &step.test))
                .collect();
            let filtered = apply_predicates(dom, axis_list, &step.predicates, ctx)?;
            next.extend(filtered);
        }
        sort_document_order(&mut next);
        current = next;
    }
    Ok(current)
}

fn apply_predicates(
    dom: &Dom,
    mut nodes: Vec<XNode>,
    preds: &[Expr],
    ctx: &Ctx,
) -> Result<Vec<XNode>> {
    for pred in preds {
        let size = nodes.len();
        let mut kept = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let mut sub = ctx.clone();
            sub.node = *n;
            sub.position = i + 1;
            sub.size = size;
            let v = eval(dom, pred, &sub)?;
            let keep = match v {
                Value::Num(num) => (i + 1) as f64 == num,
                other => other.to_bool(dom),
            };
            if keep {
                kept.push(*n);
            }
        }
        nodes = kept;
    }
    Ok(nodes)
}

fn axis_nodes(dom: &Dom, base: XNode, axis: Axis) -> Vec<XNode> {
    let id = match base {
        XNode::Node(id) => id,
        XNode::Attr(owner, _) => {
            // Axes from an attribute node: only parent/self are meaningful
            // for our purposes.
            return match axis {
                Axis::Parent | Axis::Ancestor => {
                    let mut out = vec![XNode::Node(owner)];
                    if axis == Axis::Ancestor {
                        out.extend(dom.ancestors(owner).into_iter().map(XNode::Node));
                    }
                    out
                }
                Axis::Current => vec![base],
                _ => Vec::new(),
            };
        }
    };
    match axis {
        Axis::Current => vec![XNode::Node(id)],
        Axis::Parent => dom
            .parent(id)
            .map(|p| vec![XNode::Node(p)])
            .unwrap_or_default(),
        Axis::Child => dom.children(id).iter().map(|&c| XNode::Node(c)).collect(),
        Axis::Descendant => {
            let mut all = dom.descendants_or_self(id);
            all.remove(0);
            all.into_iter().map(XNode::Node).collect()
        }
        Axis::DescendantOrSelf => dom
            .descendants_or_self(id)
            .into_iter()
            .map(XNode::Node)
            .collect(),
        Axis::Ancestor => {
            // Reverse axis: nearest first.
            dom.ancestors(id).into_iter().map(XNode::Node).collect()
        }
        Axis::AncestorOrSelf => {
            let mut out = vec![XNode::Node(id)];
            out.extend(dom.ancestors(id).into_iter().map(XNode::Node));
            out
        }
        Axis::FollowingSibling => match dom.parent(id) {
            Some(p) => {
                let sibs = dom.children(p);
                let pos = sibs.iter().position(|&s| s == id).unwrap_or(sibs.len());
                sibs.iter().skip(pos + 1).map(|&s| XNode::Node(s)).collect()
            }
            None => Vec::new(),
        },
        Axis::PrecedingSibling => match dom.parent(id) {
            Some(p) => {
                let sibs = dom.children(p);
                let pos = sibs.iter().position(|&s| s == id).unwrap_or(0);
                // Reverse axis: nearest first.
                sibs.iter()
                    .take(pos)
                    .rev()
                    .map(|&s| XNode::Node(s))
                    .collect()
            }
            None => Vec::new(),
        },
        Axis::Following => {
            // All nodes after this node's subtree in document order.
            let subtree: Vec<NodeId> = dom.descendants_or_self(id);
            let last = *subtree.last().unwrap_or(&id);
            dom.nodes
                .iter()
                .enumerate()
                .skip(last as usize + 1)
                .map(|(i, _)| XNode::Node(i as NodeId))
                .collect()
        }
        Axis::Preceding => {
            // All nodes before this node, excluding ancestors; nearest first.
            let ancestors: Vec<NodeId> = dom.ancestors(id);
            (0..id)
                .rev()
                .filter(|n| !ancestors.contains(n))
                .map(XNode::Node)
                .collect()
        }
        Axis::Attribute => dom
            .node(id)
            .attrs
            .iter()
            .enumerate()
            .map(|(i, _)| XNode::Attr(id, i))
            .collect(),
        Axis::Namespace => Vec::new(),
    }
}

fn node_test_matches(dom: &Dom, n: XNode, test: &NodeTest) -> bool {
    match (n, test) {
        (XNode::Attr(owner, idx), NodeTest::Name(name)) => dom
            .node(owner)
            .attrs
            .get(idx)
            .map(|(k, _)| k == name)
            .unwrap_or(false),
        (XNode::Attr(_, _), NodeTest::Wildcard) => true,
        (XNode::Attr(_, _), NodeTest::Node) => true,
        (XNode::Attr(_, _), _) => false,
        (XNode::Node(id), NodeTest::Wildcard) => dom.node(id).is_element(),
        (XNode::Node(id), NodeTest::Name(name)) => {
            let node = dom.node(id);
            if let Some((prefix, local)) = name.split_once(':') {
                let _ = prefix;
                node.is_element() && node.tag == local
            } else {
                node.is_element() && node.tag == *name
            }
        }
        (XNode::Node(id), NodeTest::PrefixWildcard(_)) => dom.node(id).is_element(),
        (XNode::Node(_), NodeTest::Node) => true,
        (XNode::Node(id), NodeTest::Text) => dom.node(id).kind == NodeKind::Text,
        (XNode::Node(id), NodeTest::Comment) => dom.node(id).kind == NodeKind::Comment,
        (XNode::Node(_), NodeTest::Pi(_)) => false,
    }
}

fn values_equal(dom: &Dom, a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nodes(x), Value::Nodes(y)) => x.iter().any(|nx| {
            y.iter()
                .any(|ny| string_value(dom, *nx) == string_value(dom, *ny))
        }),
        (Value::Nodes(x), Value::Bool(bb)) => (!x.is_empty()) == *bb,
        (Value::Bool(bb), Value::Nodes(x)) => (!x.is_empty()) == *bb,
        (Value::Nodes(x), Value::Num(n)) => x
            .iter()
            .any(|nx| parse_xpath_number(&string_value(dom, *nx)) == *n),
        (Value::Num(n), Value::Nodes(x)) => x
            .iter()
            .any(|nx| parse_xpath_number(&string_value(dom, *nx)) == *n),
        (Value::Nodes(x), Value::Str(s)) => x.iter().any(|nx| string_value(dom, *nx) == *s),
        (Value::Str(s), Value::Nodes(x)) => x.iter().any(|nx| string_value(dom, *nx) == *s),
        (Value::Bool(x), _) => *x == b.to_bool(dom),
        (_, Value::Bool(y)) => a.to_bool(dom) == *y,
        (Value::Num(_), _) | (_, Value::Num(_)) => {
            let x = a.to_number(dom);
            let y = b.to_number(dom);
            x == y
        }
        _ => a.to_string(dom) == b.to_string(dom),
    }
}

fn values_rel(dom: &Dom, a: &Value, b: &Value, op: RelOp) -> bool {
    let cmp = |x: f64, y: f64| match op {
        RelOp::Lt => x < y,
        RelOp::Le => x <= y,
        RelOp::Gt => x > y,
        RelOp::Ge => x >= y,
    };
    match (a, b) {
        (Value::Nodes(x), Value::Nodes(y)) => x.iter().any(|nx| {
            y.iter().any(|ny| {
                cmp(
                    parse_xpath_number(&string_value(dom, *nx)),
                    parse_xpath_number(&string_value(dom, *ny)),
                )
            })
        }),
        (Value::Nodes(x), other) => {
            let y = other.to_number(dom);
            x.iter()
                .any(|nx| cmp(parse_xpath_number(&string_value(dom, *nx)), y))
        }
        (other, Value::Nodes(y)) => {
            let x = other.to_number(dom);
            y.iter()
                .any(|ny| cmp(x, parse_xpath_number(&string_value(dom, *ny))))
        }
        _ => cmp(a.to_number(dom), b.to_number(dom)),
    }
}

fn eval_function(dom: &Dom, name: &str, arg_exprs: &[Expr], ctx: &Ctx) -> Result<Value> {
    // last() and position() take no arguments and use the context.
    match name {
        "last" => return Ok(Value::Num(ctx.size as f64)),
        "position" => return Ok(Value::Num(ctx.position as f64)),
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    let mut args: Vec<Value> = Vec::new();
    for e in arg_exprs {
        args.push(eval(dom, e, ctx)?);
    }
    let arg_str = |i: usize| -> String {
        args.get(i)
            .map(|v| v.to_string(dom))
            .unwrap_or_else(|| string_value(dom, ctx.node))
    };
    let arg_num = |i: usize| -> f64 {
        args.get(i)
            .map(|v| v.to_number(dom))
            .unwrap_or_else(|| ctx.node_string_number(dom))
    };
    match name {
        "count" => match args.into_iter().next() {
            Some(Value::Nodes(ns)) => Ok(Value::Num(ns.len() as f64)),
            _ => Err(SelectError::Type("count() needs a node-set".into())),
        },
        "sum" => match args.into_iter().next() {
            Some(Value::Nodes(ns)) => Ok(Value::Num(
                ns.iter()
                    .map(|n| parse_xpath_number(&string_value(dom, *n)))
                    .sum(),
            )),
            _ => Err(SelectError::Type("sum() needs a node-set".into())),
        },
        "string" => Ok(Value::Str(arg_str(0))),
        "concat" => Ok(Value::Str(
            args.iter()
                .map(|v| v.to_string(dom))
                .collect::<Vec<_>>()
                .join(""),
        )),
        "starts-with" => Ok(Value::Bool(arg_str(0).starts_with(&arg_str(1)))),
        "contains" => Ok(Value::Bool(arg_str(0).contains(&arg_str(1)))),
        "substring-before" => {
            let s = arg_str(0);
            let needle = arg_str(1);
            Ok(Value::Str(
                s.split(&needle).next().unwrap_or("").to_string(),
            ))
        }
        "substring-after" => {
            let s = arg_str(0);
            let needle = arg_str(1);
            if needle.is_empty() {
                return Ok(Value::Str(s));
            }
            Ok(Value::Str(match s.find(&needle) {
                Some(pos) => s[pos + needle.len()..].to_string(),
                None => String::new(),
            }))
        }
        "substring" => {
            let s = arg_str(0);
            let start = xpath_round(arg_num(1));
            let chars: Vec<char> = s.chars().collect();
            let out: String = chars
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    let pos = (*i as f64) + 1.0;
                    if args.len() >= 3 {
                        let len = xpath_round(arg_num(2));
                        pos >= start && pos < start + len
                    } else {
                        pos >= start
                    }
                })
                .map(|(_, c)| c)
                .collect();
            Ok(Value::Str(out))
        }
        "string-length" => Ok(Value::Num(arg_str(0).chars().count() as f64)),
        "normalize-space" => Ok(Value::Str(normalize_space(&arg_str(0)))),
        "translate" => {
            let s = arg_str(0);
            let from: Vec<char> = arg_str(1).chars().collect();
            let to: Vec<char> = arg_str(2).chars().collect();
            let mut out = String::new();
            for c in s.chars() {
                if let Some(pos) = from.iter().position(|&f| f == c) {
                    if let Some(t) = to.get(pos) {
                        out.push(*t);
                    }
                    // No counterpart: the character is removed.
                } else {
                    out.push(c);
                }
            }
            Ok(Value::Str(out))
        }
        "boolean" => Ok(Value::Bool(
            args.into_iter()
                .next()
                .map(|v| v.to_bool(dom))
                .unwrap_or(false),
        )),
        "not" => Ok(Value::Bool(
            !args
                .into_iter()
                .next()
                .map(|v| v.to_bool(dom))
                .unwrap_or(false),
        )),
        "number" => Ok(Value::Num(arg_num(0))),
        "floor" => Ok(Value::Num(arg_num(0).floor())),
        "ceiling" => Ok(Value::Num(arg_num(0).ceil())),
        "round" => Ok(Value::Num(xpath_round(arg_num(0)))),
        "name" | "local-name" | "namespace-uri" => {
            let node = match args.into_iter().next() {
                Some(Value::Nodes(ns)) => ns.first().copied(),
                _ => Some(ctx.node),
            };
            let value = match (name, node) {
                (_, None) => String::new(),
                ("namespace-uri", _) => String::new(),
                ("name", Some(XNode::Attr(owner, idx))) => dom
                    .node(owner)
                    .attrs
                    .get(idx)
                    .map(|(k, _)| k.clone())
                    .unwrap_or_default(),
                ("name", Some(XNode::Node(id))) => {
                    let n = dom.node(id);
                    if n.is_element() {
                        n.tag.clone()
                    } else {
                        String::new()
                    }
                }
                ("local-name", Some(XNode::Attr(owner, idx))) => dom
                    .node(owner)
                    .attrs
                    .get(idx)
                    .map(|(k, _)| k.rsplit(':').next().unwrap_or(k).to_string())
                    .unwrap_or_default(),
                ("local-name", Some(XNode::Node(id))) => {
                    let n = dom.node(id);
                    if n.is_element() {
                        n.tag.clone()
                    } else {
                        String::new()
                    }
                }
                _ => String::new(),
            };
            Ok(Value::Str(value))
        }
        "id" => {
            let wanted: Vec<String> = arg_str(0)
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            let mut out = Vec::new();
            for el in dom.descendants_or_self(dom.root) {
                if let Some(v) = dom.attr(el, "id") {
                    if wanted.iter().any(|w| w == v) {
                        out.push(XNode::Node(el));
                    }
                }
            }
            Ok(Value::Nodes(out))
        }
        "lang" => {
            let lang = arg_str(0);
            let mut cur = match ctx.node {
                XNode::Node(id) => Some(id),
                XNode::Attr(owner, _) => Some(owner),
            };
            let mut result = false;
            while let Some(n) = cur {
                if let Some(v) = dom.attr(n, "lang").or_else(|| dom.attr(n, "xml:lang")) {
                    let v_lower = v.to_ascii_lowercase();
                    let l_lower = lang.to_ascii_lowercase();
                    result = v_lower == l_lower || v_lower.starts_with(&format!("{}-", l_lower));
                    break;
                }
                cur = dom.parent(n);
            }
            Ok(Value::Bool(result))
        }
        _ => syntax(format!("unknown function {}()", name)),
    }
}

fn xpath_round(x: f64) -> f64 {
    // XPath round(): nearest integer, .5 rounds towards positive infinity.
    (x + 0.5).floor()
}

fn normalize_space(s: &str) -> String {
    s.split([' ', '\t', '\n', '\r'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

trait CtxNum {
    fn node_string_number(&self, dom: &Dom) -> f64;
}

impl CtxNum for Ctx {
    fn node_string_number(&self, dom: &Dom) -> f64 {
        parse_xpath_number(&string_value(dom, self.node))
    }
}
