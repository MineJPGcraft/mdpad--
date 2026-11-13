//! A tiny JavaScript-subset expression evaluator for MDX and Markdoc.
//!
//! MDX props (`<Badge level={2 + 1}>`) and Markdoc conditions
//! (`{% if $page.draft == false %}`) are JavaScript expressions. This module
//! evaluates the *safe* subset of that language — literals, member access,
//! arithmetic, comparison, logic, ternaries, array/object literals and a small
//! set of pure builtins — so attribute values and conditions can be resolved
//! without embedding a JavaScript engine.
//!
//! Nothing here can call out to the host: there is no `eval`, no function
//! values, and unknown identifiers evaluate to [`Value::Null`] with a warning
//! rather than failing the whole attribute.
//!
//! # Pipeline
//!
//! ```text
//! source ──tokenize──▶ tokens ──parse──▶ Expr ──eval──▶ Value
//! ```
//!
//! Parsing is a precedence-climbing (Pratt) loop, which keeps the grammar in one
//! place instead of one function per precedence level.

use std::collections::BTreeMap;
use std::fmt;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// A JSON-like value produced by evaluating an expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    List(Vec<Value>),
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// JavaScript truthiness: `false`, `0`, `""`, `null`, `undefined`, `NaN`
    /// and empty containers are falsy.
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0 && !n.is_nan(),
            Self::Str(s) => !s.is_empty(),
            Self::List(items) => !items.is_empty(),
            Self::Object(map) => !map.is_empty(),
        }
    }

    /// How the value is written when it lands in text.
    pub fn to_display(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Bool(b) => b.to_string(),
            Self::Number(n) => format_number(*n),
            Self::Str(s) => s.clone(),
            Self::List(items) => items
                .iter()
                .map(|i| i.to_display())
                .collect::<Vec<_>>()
                .join(", "),
            Self::Object(_) => "[object]".to_owned(),
        }
    }

    /// Numeric coercion, used by arithmetic and comparison.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            Self::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Self::Str(s) => s.trim().parse::<f64>().ok(),
            Self::Null => Some(0.0),
            _ => None,
        }
    }

    /// Member lookup: object keys, list indices and `length`.
    pub fn member(&self, name: &str) -> Value {
        match self {
            Self::Object(map) => map.get(name).cloned().unwrap_or(Value::Null),
            Self::List(items) => {
                if name == "length" {
                    Value::Number(items.len() as f64)
                } else {
                    match name.parse::<usize>() {
                        Ok(i) => items.get(i).cloned().unwrap_or(Value::Null),
                        Err(_) => Value::Null,
                    }
                }
            }
            Self::Str(s) => {
                if name == "length" {
                    Value::Number(s.chars().count() as f64)
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        }
    }

    pub fn str(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    pub fn num(n: f64) -> Self {
        Self::Number(n)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_display())
    }
}

/// Format a number the way JavaScript prints it: integers without a fraction.
fn format_number(n: f64) -> String {
    if !n.is_finite() {
        return if n.is_nan() {
            "NaN".to_owned()
        } else if n > 0.0 {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let mut s = format!("{n}");
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Variables an expression can see.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    vars: BTreeMap<String, Value>,
}

impl Scope {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        self.vars.insert(name.into(), value);
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(name.trim_start_matches('$'))
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why an expression could not be evaluated. The caller turns this into a
/// warning and keeps rendering the surrounding document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalError {
    pub message: String,
}

impl EvalError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Punct(&'static str),
}

fn tokenize(src: &str) -> Result<Vec<Tok>, EvalError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let n = text
                .parse::<f64>()
                .map_err(|_| EvalError::new(format!("无法解析数字 {text}")))?;
            out.push(Tok::Num(n));
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            i += 1;
            let mut text = String::new();
            let mut closed = false;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' && i + 1 < chars.len() {
                    let esc = chars[i + 1];
                    text.push(match esc {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                    i += 2;
                    continue;
                }
                if d == quote {
                    i += 1;
                    closed = true;
                    break;
                }
                text.push(d);
                i += 1;
            }
            if !closed {
                return Err(EvalError::new("字符串缺少结束引号"));
            }
            out.push(Tok::Str(text));
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            out.push(Tok::Ident(chars[start..i].iter().collect()));
            continue;
        }
        // Three-character operators first: `===` must not be read as `==` + `=`.
        let three: String = chars[i..(i + 3).min(chars.len())].iter().collect();
        if three == "===" || three == "!==" {
            let op: &'static str = if three == "===" { "===" } else { "!==" };
            out.push(Tok::Punct(op));
            i += 3;
            continue;
        }
        // Two-character operators next.
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        let two_op = match two.as_str() {
            "==" | "!=" | "<=" | ">=" | "&&" | "||" | "===" | "!==" => Some(two.as_str()),
            _ => None,
        };
        if let Some(op) = two_op {
            let op: &'static str = match op {
                "==" => "==",
                "!=" => "!=",
                "<=" => "<=",
                ">=" => ">=",
                "&&" => "&&",
                "||" => "||",
                "===" => "===",
                _ => "!==",
            };
            out.push(Tok::Punct(op));
            i += 2;
            continue;
        }
        let op = match c {
            '+' => "+",
            '-' => "-",
            '*' => "*",
            '/' => "/",
            '%' => "%",
            '!' => "!",
            '<' => "<",
            '>' => ">",
            '?' => "?",
            ':' => ":",
            '(' => "(",
            ')' => ")",
            '[' => "[",
            ']' => "]",
            '{' => "{",
            '}' => "}",
            ',' => ",",
            '.' => ".",
            _ => {
                return Err(EvalError::new(format!("无法识别的字符 '{c}'")));
            }
        };
        out.push(Tok::Punct(op));
        i += 1;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Literal(Value),
    Ident(String),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
    List(Vec<Expr>),
    Object(Vec<(String, Expr)>),
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
    /// Guards against `((((…` growing the stack without bound.
    depth: u32,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Punct(q)) if *q == p) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// Precedence-climbing entry point.
    fn expr(&mut self, min_prec: u8) -> Result<Expr, EvalError> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Err(EvalError::new("表达式嵌套过深"));
        }
        let result = self.expr_inner(min_prec);
        self.depth -= 1;
        result
    }

    fn expr_inner(&mut self, min_prec: u8) -> Result<Expr, EvalError> {
        let mut left = self.unary()?;
        loop {
            // Ternary binds loosest.
            if min_prec == 0 && self.eat_punct("?") {
                let then = self.expr(0)?;
                if !self.eat_punct(":") {
                    return Err(EvalError::new("三元表达式缺少 ':'"));
                }
                let other = self.expr(0)?;
                left = Expr::Ternary(Box::new(left), Box::new(then), Box::new(other));
                continue;
            }
            let Some((op, prec)) = self.binary_op() else {
                break;
            };
            if prec < min_prec {
                break;
            }
            self.i += 1;
            let right = self.expr(prec + 1)?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn binary_op(&self) -> Option<(&'static str, u8)> {
        let Some(Tok::Punct(p)) = self.peek() else {
            return None;
        };
        Some(match *p {
            "||" => ("||", 1),
            "&&" => ("&&", 2),
            "==" | "===" => ("==", 3),
            "!=" | "!==" => ("!=", 3),
            "<" => ("<", 4),
            ">" => (">", 4),
            "<=" => ("<=", 4),
            ">=" => (">=", 4),
            "+" => ("+", 5),
            "-" => ("-", 5),
            "*" => ("*", 6),
            "/" => ("/", 6),
            "%" => ("%", 6),
            _ => return None,
        })
    }

    fn unary(&mut self) -> Result<Expr, EvalError> {
        if self.eat_punct("!") {
            return Ok(Expr::Unary("!", Box::new(self.unary()?)));
        }
        if self.eat_punct("-") {
            return Ok(Expr::Unary("-", Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, EvalError> {
        let mut expr = self.primary()?;
        loop {
            if self.eat_punct(".") {
                let Some(Tok::Ident(name)) = self.peek().cloned() else {
                    return Err(EvalError::new("'.' 后缺少属性名"));
                };
                self.i += 1;
                expr = Expr::Member(Box::new(expr), name);
                continue;
            }
            if self.eat_punct("[") {
                let index = self.expr(0)?;
                if !self.eat_punct("]") {
                    return Err(EvalError::new("下标缺少 ']'"));
                }
                expr = Expr::Index(Box::new(expr), Box::new(index));
                continue;
            }
            if self.eat_punct("(") {
                // `Math.round(…)` parses as a member chain; flatten it back into
                // a dotted name so the builtin table can match on one string.
                let name = match &expr {
                    Expr::Ident(name) => name.clone(),
                    Expr::Member(base, prop) => match base.as_ref() {
                        Expr::Ident(root) => format!("{root}.{prop}"),
                        _ => return Err(EvalError::new("只支持调用内置函数")),
                    },
                    _ => return Err(EvalError::new("只支持调用内置函数")),
                };
                let mut args = Vec::new();
                if !self.eat_punct(")") {
                    loop {
                        args.push(self.expr(0)?);
                        if self.eat_punct(")") {
                            break;
                        }
                        if !self.eat_punct(",") {
                            return Err(EvalError::new("函数参数缺少 ','"));
                        }
                    }
                }
                expr = Expr::Call(name, args);
                continue;
            }
            break;
        }
        Ok(expr)
    }

    fn primary(&mut self) -> Result<Expr, EvalError> {
        let tok = self
            .peek()
            .cloned()
            .ok_or_else(|| EvalError::new("表达式不完整"))?;
        match tok {
            Tok::Num(n) => {
                self.i += 1;
                Ok(Expr::Literal(Value::Number(n)))
            }
            Tok::Str(s) => {
                self.i += 1;
                Ok(Expr::Literal(Value::Str(s)))
            }
            Tok::Ident(name) => {
                self.i += 1;
                Ok(match name.as_str() {
                    "true" => Expr::Literal(Value::Bool(true)),
                    "false" => Expr::Literal(Value::Bool(false)),
                    "null" | "undefined" => Expr::Literal(Value::Null),
                    _ => Expr::Ident(name),
                })
            }
            Tok::Punct("(") => {
                self.i += 1;
                let inner = self.expr(0)?;
                if !self.eat_punct(")") {
                    return Err(EvalError::new("括号缺少 ')'"));
                }
                Ok(inner)
            }
            Tok::Punct("[") => {
                self.i += 1;
                let mut items = Vec::new();
                if !self.eat_punct("]") {
                    loop {
                        items.push(self.expr(0)?);
                        if self.eat_punct("]") {
                            break;
                        }
                        if !self.eat_punct(",") {
                            return Err(EvalError::new("数组缺少 ',' 或 ']'"));
                        }
                        if self.eat_punct("]") {
                            break;
                        }
                    }
                }
                Ok(Expr::List(items))
            }
            Tok::Punct("{") => {
                self.i += 1;
                let mut fields = Vec::new();
                if !self.eat_punct("}") {
                    loop {
                        let key = match self.peek().cloned() {
                            Some(Tok::Ident(k)) | Some(Tok::Str(k)) => {
                                self.i += 1;
                                k
                            }
                            _ => return Err(EvalError::new("对象缺少键名")),
                        };
                        if !self.eat_punct(":") {
                            return Err(EvalError::new("对象键缺少 ':'"));
                        }
                        let value = self.expr(0)?;
                        fields.push((key, value));
                        if self.eat_punct("}") {
                            break;
                        }
                        if !self.eat_punct(",") {
                            return Err(EvalError::new("对象缺少 ',' 或 '}'"));
                        }
                        if self.eat_punct("}") {
                            break;
                        }
                    }
                }
                Ok(Expr::Object(fields))
            }
            other => Err(EvalError::new(format!("无法解析的记号 {other:?}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Evaluate `src` against `scope`.
pub fn eval(src: &str, scope: &Scope) -> Result<Value, EvalError> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Ok(Value::Null);
    }
    let toks = tokenize(trimmed)?;
    let mut parser = Parser {
        toks,
        i: 0,
        depth: 0,
    };
    let expr = parser.expr(0)?;
    if parser.i < parser.toks.len() {
        return Err(EvalError::new("表达式末尾多余内容"));
    }
    eval_expr(&expr, scope, 0)
}

/// Evaluate an expression, treating a failure as `Null`.
///
/// A broken attribute should never take the document down; the caller collects
/// the error as a warning instead.
pub fn eval_or_null(src: &str, scope: &Scope) -> (Value, Option<EvalError>) {
    match eval(src, scope) {
        Ok(v) => (v, None),
        Err(e) => (Value::Null, Some(e)),
    }
}

const MAX_DEPTH: u32 = 48;
/// Parser nesting bound; `((((…` would otherwise recurse without limit.
const MAX_PARSE_DEPTH: u32 = 48;

fn eval_expr(expr: &Expr, scope: &Scope, depth: u32) -> Result<Value, EvalError> {
    if depth > MAX_DEPTH {
        return Err(EvalError::new("表达式嵌套过深"));
    }
    let deeper = depth + 1;
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Ident(name) => Ok(scope.get(name).cloned().unwrap_or(Value::Null)),
        Expr::Unary(op, inner) => {
            let v = eval_expr(inner, scope, deeper)?;
            Ok(match *op {
                "!" => Value::Bool(!v.truthy()),
                "-" => Value::Number(-v.as_number().unwrap_or(0.0)),
                _ => Value::Null,
            })
        }
        Expr::Binary(op, left, right) => {
            // `&&` / `||` short-circuit and return the operand, as in JS.
            if *op == "&&" {
                let l = eval_expr(left, scope, deeper)?;
                return if l.truthy() {
                    eval_expr(right, scope, deeper)
                } else {
                    Ok(l)
                };
            }
            if *op == "||" {
                let l = eval_expr(left, scope, deeper)?;
                return if l.truthy() {
                    Ok(l)
                } else {
                    eval_expr(right, scope, deeper)
                };
            }
            let l = eval_expr(left, scope, deeper)?;
            let r = eval_expr(right, scope, deeper)?;
            Ok(match *op {
                "+" => match (&l, &r) {
                    // `+` concatenates as soon as one side is a string.
                    (Value::Str(_), _) | (_, Value::Str(_)) => {
                        Value::Str(format!("{}{}", l.to_display(), r.to_display()))
                    }
                    _ => Value::Number(l.as_number().unwrap_or(0.0) + r.as_number().unwrap_or(0.0)),
                },
                "-" => Value::Number(l.as_number().unwrap_or(0.0) - r.as_number().unwrap_or(0.0)),
                "*" => Value::Number(l.as_number().unwrap_or(0.0) * r.as_number().unwrap_or(0.0)),
                "/" => {
                    let d = r.as_number().unwrap_or(0.0);
                    Value::Number(if d == 0.0 {
                        f64::NAN
                    } else {
                        l.as_number().unwrap_or(0.0) / d
                    })
                }
                "%" => {
                    let d = r.as_number().unwrap_or(0.0);
                    Value::Number(if d == 0.0 {
                        f64::NAN
                    } else {
                        l.as_number().unwrap_or(0.0) % d
                    })
                }
                "==" | "===" => Value::Bool(loose_eq(&l, &r)),
                "!=" | "!==" => Value::Bool(!loose_eq(&l, &r)),
                "<" => Value::Bool(compare(&l, &r) == Some(std::cmp::Ordering::Less)),
                ">" => Value::Bool(compare(&l, &r) == Some(std::cmp::Ordering::Greater)),
                "<=" => Value::Bool(matches!(
                    compare(&l, &r),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )),
                ">=" => Value::Bool(matches!(
                    compare(&l, &r),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                )),
                _ => Value::Null,
            })
        }
        Expr::Ternary(cond, then, other) => {
            if eval_expr(cond, scope, deeper)?.truthy() {
                eval_expr(then, scope, deeper)
            } else {
                eval_expr(other, scope, deeper)
            }
        }
        Expr::Member(base, name) => Ok(eval_expr(base, scope, deeper)?.member(name)),
        Expr::Index(base, index) => {
            let base = eval_expr(base, scope, deeper)?;
            let index = eval_expr(index, scope, deeper)?;
            let key = match index {
                Value::Str(s) => s,
                other => other.to_display(),
            };
            Ok(base.member(&key))
        }
        Expr::Call(name, args) => {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(eval_expr(arg, scope, deeper)?);
            }
            call_builtin(name, &values)
        }
        Expr::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(eval_expr(item, scope, deeper)?);
            }
            Ok(Value::List(out))
        }
        Expr::Object(fields) => {
            let mut map = BTreeMap::new();
            for (key, value) in fields {
                let v = eval_expr(value, scope, deeper)?;
                map.insert(key.clone(), v);
            }
            Ok(Value::Object(map))
        }
    }
}

fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => x == y,
        (Value::Object(x), Value::Object(y)) => x == y,
        _ => match (a.as_number(), b.as_number()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

fn compare(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Value::Str(x), Value::Str(y)) = (a, b) {
        return Some(x.cmp(y));
    }
    let (x, y) = (a.as_number()?, b.as_number()?);
    x.partial_cmp(&y)
}

/// The only callables: pure helpers with no access to the document or the host.
fn call_builtin(name: &str, args: &[Value]) -> Result<Value, EvalError> {
    let first = args.first().cloned().unwrap_or(Value::Null);
    Ok(match name {
        "String" => Value::Str(first.to_display()),
        "Number" => Value::Number(first.as_number().unwrap_or(f64::NAN)),
        "Boolean" => Value::Bool(first.truthy()),
        "Math.max" => Value::Number(
            args.iter()
                .filter_map(|v| v.as_number())
                .fold(f64::NEG_INFINITY, f64::max),
        ),
        "Math.min" => Value::Number(
            args.iter()
                .filter_map(|v| v.as_number())
                .fold(f64::INFINITY, f64::min),
        ),
        "Math.round" => Value::Number(first.as_number().unwrap_or(0.0).round()),
        "Math.floor" => Value::Number(first.as_number().unwrap_or(0.0).floor()),
        "Math.ceil" => Value::Number(first.as_number().unwrap_or(0.0).ceil()),
        "Math.abs" => Value::Number(first.as_number().unwrap_or(0.0).abs()),
        "Math.trunc" => Value::Number(first.as_number().unwrap_or(0.0).trunc()),
        "Math.sqrt" => Value::Number(first.as_number().unwrap_or(0.0).sqrt()),
        "Array.isArray" => Value::Bool(matches!(first, Value::List(_))),
        // A dotted name reaches here because `Math.max(…)` parses as a member
        // call chain; anything else is unknown.
        other => {
            return Err(EvalError::new(format!("未知函数 {other}")));
        }
    })
}

// ---------------------------------------------------------------------------
// Interpolation
// ---------------------------------------------------------------------------

/// Replace `{{ expr }}` and `${ expr }` runs inside `text` with their values.
///
/// Markdoc writes `{% $x %}` in prose; that form is handled by the caller, which
/// strips the delimiters and calls [`eval`] directly.
pub fn interpolate(text: &str, scope: &Scope) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut warnings = Vec::new();
    let mut rest = text;
    while let Some(start) = find_interpolation(rest) {
        let (prefix, open_len, close) = start;
        out.push_str(&rest[..prefix]);
        let body = &rest[prefix + open_len..];
        let Some(end) = body.find(close) else {
            out.push_str(&rest[prefix..]);
            return (out, warnings);
        };
        let expr = &body[..end];
        match eval(expr, scope) {
            Ok(v) => out.push_str(&v.to_display()),
            Err(e) => {
                warnings.push(format!("插值 {{{expr}}} 求值失败: {e}"));
                out.push_str(&rest[prefix..prefix + open_len + end + close.len()]);
            }
        }
        rest = &body[end + close.len()..];
    }
    out.push_str(rest);
    (out, warnings)
}

/// Find the next interpolation: `{{ … }}` or `${ … }`.
fn find_interpolation(text: &str) -> Option<(usize, usize, &'static str)> {
    let double = text.find("{{").map(|i| (i, 2, "}}"));
    let single = text.find("${").map(|i| (i, 2, "}"));
    match (double, single) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        let mut s = Scope::new();
        s.set("page", {
            let mut map = BTreeMap::new();
            map.insert("draft".to_owned(), Value::Bool(false));
            map.insert("views".to_owned(), Value::Number(42.0));
            map.insert("title".to_owned(), Value::str("Hello"));
            Value::Object(map)
        });
        s.set("items", Value::List(vec![Value::str("a"), Value::str("b")]));
        s.set("count", Value::Number(3.0));
        s
    }

    #[test]
    fn literals_and_identifiers_resolve() {
        let s = scope();
        assert_eq!(eval("42", &s).unwrap(), Value::Number(42.0));
        assert_eq!(eval("'text'", &s).unwrap(), Value::Str("text".to_owned()));
        assert_eq!(eval("true", &s).unwrap(), Value::Bool(true));
        assert_eq!(eval("null", &s).unwrap(), Value::Null);
        assert_eq!(eval("count", &s).unwrap(), Value::Number(3.0));
        // Unknown identifiers degrade to null instead of failing.
        assert_eq!(eval("missing", &s).unwrap(), Value::Null);
    }

    #[test]
    fn arithmetic_and_precedence() {
        let s = scope();
        assert_eq!(eval("1 + 2 * 3", &s).unwrap(), Value::Number(7.0));
        assert_eq!(eval("(1 + 2) * 3", &s).unwrap(), Value::Number(9.0));
        assert_eq!(eval("7 / 2", &s).unwrap(), Value::Number(3.5));
        assert_eq!(eval("7 % 4", &s).unwrap(), Value::Number(3.0));
        assert_eq!(eval("-count", &s).unwrap(), Value::Number(-3.0));
        assert_eq!(eval("2 + 2 + 'px'", &s).unwrap(), Value::Str("4px".to_owned()));
    }

    #[test]
    fn comparison_and_logic() {
        let s = scope();
        assert_eq!(eval("count > 2", &s).unwrap(), Value::Bool(true));
        assert_eq!(eval("count >= 3", &s).unwrap(), Value::Bool(true));
        assert_eq!(eval("count === 3", &s).unwrap(), Value::Bool(true));
        assert_eq!(eval("count !== 3", &s).unwrap(), Value::Bool(false));
        assert_eq!(eval("!page.draft", &s).unwrap(), Value::Bool(true));
        // Short-circuit returns the operand, as JS does.
        assert_eq!(eval("0 || 'fallback'", &s).unwrap(), Value::str("fallback"));
        assert_eq!(eval("'x' && 'y'", &s).unwrap(), Value::str("y"));
    }

    #[test]
    fn ternary_and_member_access() {
        let s = scope();
        assert_eq!(
            eval("page.draft ? 'draft' : 'live'", &s).unwrap(),
            Value::str("live")
        );
        assert_eq!(eval("page.views", &s).unwrap(), Value::Number(42.0));
        assert_eq!(eval("items.length", &s).unwrap(), Value::Number(2.0));
        assert_eq!(eval("items[1]", &s).unwrap(), Value::str("b"));
        assert_eq!(eval("page['title']", &s).unwrap(), Value::str("Hello"));
    }

    #[test]
    fn arrays_objects_and_builtins() {
        let s = scope();
        assert_eq!(
            eval("[1, 2, 3]", &s).unwrap(),
            Value::List(vec![Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)])
        );
        let obj = eval("{ a: 1, b: 'x' }", &s).unwrap();
        assert_eq!(obj.member("b"), Value::str("x"));
        assert_eq!(eval("Math.max(3, 9, 4)", &s).unwrap(), Value::Number(9.0));
        assert_eq!(eval("Math.round(2.6)", &s).unwrap(), Value::Number(3.0));
        assert_eq!(eval("Number('12')", &s).unwrap(), Value::Number(12.0));
        assert_eq!(eval("String(5) + '!'", &s).unwrap(), Value::str("5!"));
    }

    #[test]
    fn broken_expressions_report_instead_of_panicking() {
        for src in ["1 +", "'unterminated", "((1)", "1 +* 2", "a b", "? :"] {
            // Either a clean error or a value, but never a panic.
            let _ = eval(src, &scope());
        }
        assert!(eval("1 +", &scope()).is_err());
        assert!(eval("'unterminated", &scope()).is_err());
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let src = format!("{}1{}", "(".repeat(200), ")".repeat(200));
        assert!(eval(&src, &scope()).is_err());
    }

    #[test]
    fn numbers_print_like_javascript() {
        assert_eq!(Value::Number(3.0).to_display(), "3");
        assert_eq!(Value::Number(3.5).to_display(), "3.5");
        assert_eq!(Value::Number(-0.0).to_display(), "0");
    }

    #[test]
    fn interpolation_replaces_known_values() {
        let s = scope();
        let (text, warnings) = interpolate("一共 {{ count + 1 }} 项，标题是 ${page.title}", &s);
        assert_eq!(text, "一共 4 项，标题是 Hello");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn interpolation_keeps_the_original_on_error() {
        let s = scope();
        let (text, warnings) = interpolate("坏掉的 {{ 1 + }} 表达式", &s);
        assert_eq!(text, "坏掉的 {{ 1 + }} 表达式");
        assert_eq!(warnings.len(), 1);
    }
}
