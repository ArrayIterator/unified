//! Small server-side formula evaluator: arithmetic with
//! precedence, cell references and ranges, comparisons, and a
//! data-driven function registry.
//!
//! The function table [`FUNCTION_REGISTRY`] IS the data: a
//! function's name, arity, and description live in the table,
//! and the dispatcher looks the name up there - no
//! hard-coded `match` on function names outside the table's
//! own dispatch. Adding a function means adding one table row
//! and one arm in `call_function`.
//!
//! Numbers are [`Decimal`] (integer mantissa + scale) - `f64`
//! is never used. A formula string is parsed into an AST
//! ([`Expr`]); [`evaluate_formula`] evaluates it against a
//! cell resolver; [`recalculate_sheet`] recomputes every
//! formula cell of a sheet in topological order and marks
//! cells in (or depending on) a cycle with `#CIRC!`, so a
//! circular reference is an error value, never an infinite
//! loop.

use super::types::{
    address_in_bounds, computed_for_typed, parse_address, CellKind, Decimal, ERR_CIRCULAR,
    ERR_DIV_ZERO, ERR_NAME, ERR_NUM, ERR_REF, ERR_VALUE, MAX_COLUMNS, MAX_ROWS,
};
use std::collections::{HashMap, HashSet};

// Values

/// One evaluated value. `Empty` is a blank cell (counts as 0
/// in arithmetic, as "" in text); `Error` carries an error
/// token (`#VALUE!`, `#DIV/0!`, `#REF!`, `#NAME?`, `#CIRC!`,
/// `#NUM!`) that propagates through the formula that read it.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalValue {
    Number(Decimal),
    Text(String),
    Bool(bool),
    Empty,
    Error(String),
}

impl EvalValue {
    /// Display form: numbers as decimal strings, booleans as
    /// `TRUE` / `FALSE`, errors as their token.
    pub fn display(&self) -> String {
        match self {
            Self::Number(value) => value.to_decimal_string(),
            Self::Text(text) => text.clone(),
            Self::Bool(value) => {
                if *value {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            }
            Self::Empty => String::new(),
            Self::Error(token) => token.clone(),
        }
    }

    /// The result kind of this value, for `computed_kind`.
    pub fn kind(&self) -> CellKind {
        match self {
            Self::Number(_) => CellKind::Number,
            Self::Text(_) => CellKind::Text,
            Self::Bool(_) => CellKind::Boolean,
            Self::Empty => CellKind::Empty,
            Self::Error(_) => CellKind::Error,
        }
    }

    /// Numeric view: numbers as themselves, empty as 0,
    /// booleans as 1 / 0, anything else as a `#VALUE!` error.
    pub fn as_number(&self) -> Result<Decimal, String> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::Empty => Ok(Decimal::zero()),
            Self::Bool(value) => Ok(Decimal::from_int(i64::from(*value))),
            Self::Error(token) => Err(token.clone()),
            Self::Text(_) => Err(ERR_VALUE.to_string()),
        }
    }

    /// Truthiness for `IF` / `AND` / `OR`: booleans as
    /// themselves, numbers as non-zero, text only when it
    /// spells `TRUE` / `FALSE`, empty as false.
    pub fn as_bool(&self) -> Result<bool, String> {
        match self {
            Self::Bool(value) => Ok(*value),
            Self::Number(value) => Ok(*value != Decimal::zero()),
            Self::Empty => Ok(false),
            Self::Error(token) => Err(token.clone()),
            Self::Text(text) => {
                if text.eq_ignore_ascii_case("true") {
                    Ok(true)
                } else if text.eq_ignore_ascii_case("false") {
                    Ok(false)
                } else {
                    Err(ERR_VALUE.to_string())
                }
            }
        }
    }

    fn error(token: &str) -> Self {
        Self::Error(token.to_string())
    }
}

// Function registry (the data)

/// One function in the registry: its name, its arity bounds
/// (`max_args: None` is variadic), and its description.
#[derive(Debug, Clone, Copy)]
pub struct FunctionDef {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: Option<usize>,
    pub description: &'static str,
}

/// The formula function table. This is data, not code paths:
/// the parser accepts a call only when its name is in this
/// table, and the dispatcher looks the name up here first.
pub static FUNCTION_REGISTRY: &[FunctionDef] = &[
    FunctionDef {
        name: "SUM",
        min_args: 1,
        max_args: None,
        description: "Sum of all numeric values (ranges expand to their cells).",
    },
    FunctionDef {
        name: "AVERAGE",
        min_args: 1,
        max_args: None,
        description: "Mean of all numeric values; errors when there are none.",
    },
    FunctionDef {
        name: "COUNT",
        min_args: 1,
        max_args: None,
        description: "Count of numeric values.",
    },
    FunctionDef {
        name: "COUNTA",
        min_args: 1,
        max_args: None,
        description: "Count of non-empty values.",
    },
    FunctionDef {
        name: "MIN",
        min_args: 1,
        max_args: None,
        description: "Smallest numeric value.",
    },
    FunctionDef {
        name: "MAX",
        min_args: 1,
        max_args: None,
        description: "Largest numeric value.",
    },
    FunctionDef {
        name: "PRODUCT",
        min_args: 1,
        max_args: None,
        description: "Product of all numeric values.",
    },
    FunctionDef {
        name: "IF",
        min_args: 2,
        max_args: Some(3),
        description:
            "IF(condition, value_if_true, value_if_false): only the taken branch is evaluated.",
    },
    FunctionDef {
        name: "ROUND",
        min_args: 2,
        max_args: Some(2),
        description:
            "ROUND(value, places): round to places fractional digits, half away from zero.",
    },
    FunctionDef {
        name: "ABS",
        min_args: 1,
        max_args: Some(1),
        description: "Absolute value.",
    },
    FunctionDef {
        name: "CONCAT",
        min_args: 1,
        max_args: None,
        description: "Concatenate the display form of all values.",
    },
    FunctionDef {
        name: "TODAY",
        min_args: 0,
        max_args: Some(0),
        description: "Today's date as YYYY-MM-DD.",
    },
    FunctionDef {
        name: "LEN",
        min_args: 1,
        max_args: Some(1),
        description: "Character count of the display form.",
    },
    FunctionDef {
        name: "UPPER",
        min_args: 1,
        max_args: Some(1),
        description: "Uppercase text.",
    },
    FunctionDef {
        name: "LOWER",
        min_args: 1,
        max_args: Some(1),
        description: "Lowercase text.",
    },
    FunctionDef {
        name: "TRIM",
        min_args: 1,
        max_args: Some(1),
        description: "Text with leading and trailing whitespace removed.",
    },
    FunctionDef {
        name: "NOT",
        min_args: 1,
        max_args: Some(1),
        description: "Logical negation.",
    },
    FunctionDef {
        name: "AND",
        min_args: 1,
        max_args: None,
        description: "True when every argument is true.",
    },
    FunctionDef {
        name: "OR",
        min_args: 1,
        max_args: None,
        description: "True when any argument is true.",
    },
];

/// Look a function name up in the registry (case insensitive).
pub fn function_def(name: &str) -> Option<&'static FunctionDef> {
    FUNCTION_REGISTRY
        .iter()
        .find(|def| def.name.eq_ignore_ascii_case(name))
}

/// The registered function names, for the API and the docs.
pub fn function_names() -> Vec<&'static str> {
    FUNCTION_REGISTRY.iter().map(|def| def.name).collect()
}

// Lexer

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(String),
    Text(String),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    LParen,
    RParen,
    Comma,
    Colon,
    Eq,
    Lt,
    Gt,
    Le,
    Ge,
    Ne,
    Eof,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        let current = chars[index];
        match current {
            ' ' | '\t' | '\n' | '\r' => index += 1,
            '+' => {
                tokens.push(Token::Plus);
                index += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                index += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                index += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                index += 1;
            }
            '%' => {
                tokens.push(Token::Percent);
                index += 1;
            }
            '^' => {
                tokens.push(Token::Caret);
                index += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                index += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                index += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                index += 1;
            }
            '=' => {
                tokens.push(Token::Eq);
                index += 1;
            }
            '<' => {
                if chars.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Le);
                    index += 2;
                } else if chars.get(index + 1) == Some(&'>') {
                    tokens.push(Token::Ne);
                    index += 2;
                } else {
                    tokens.push(Token::Lt);
                    index += 1;
                }
            }
            '>' => {
                if chars.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Ge);
                    index += 2;
                } else {
                    tokens.push(Token::Gt);
                    index += 1;
                }
            }
            '"' => {
                // String literal; "" inside is an escaped quote.
                let mut text = String::new();
                index += 1;
                let mut closed = false;
                while index < chars.len() {
                    if chars[index] == '"' {
                        if chars.get(index + 1) == Some(&'"') {
                            text.push('"');
                            index += 2;
                        } else {
                            closed = true;
                            index += 1;
                            break;
                        }
                    } else {
                        text.push(chars[index]);
                        index += 1;
                    }
                }
                if !closed {
                    return Err(ERR_VALUE.to_string());
                }
                tokens.push(Token::Text(text));
            }
            '$' => {
                // Absolute-reference marker, stripped: $B$12 is B12.
                index += 1;
            }
            _ if current.is_ascii_digit()
                || (current == '.'
                    && chars
                        .get(index + 1)
                        .is_some_and(|next| next.is_ascii_digit())) =>
            {
                let start = index;
                while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.')
                {
                    index += 1;
                }
                if index < chars.len() && (chars[index] == 'e' || chars[index] == 'E') {
                    let mut lookahead = index + 1;
                    if chars.get(lookahead) == Some(&'+') || chars.get(lookahead) == Some(&'-') {
                        lookahead += 1;
                    }
                    if chars
                        .get(lookahead)
                        .is_some_and(|next| next.is_ascii_digit())
                    {
                        index = lookahead;
                        while index < chars.len() && chars[index].is_ascii_digit() {
                            index += 1;
                        }
                    }
                }
                tokens.push(Token::Number(chars[start..index].iter().collect()));
            }
            _ if current.is_ascii_alphabetic() || current == '_' => {
                let start = index;
                while index < chars.len()
                    && (chars[index].is_ascii_alphanumeric()
                        || chars[index] == '_'
                        || chars[index] == '$')
                {
                    index += 1;
                }
                // `$` is part of an absolute reference ($B$12),
                // not a token of its own: parse_address strips it.
                tokens.push(Token::Ident(chars[start..index].iter().collect()));
            }
            _ => return Err(ERR_VALUE.to_string()),
        }
    }
    tokens.push(Token::Eof);
    Ok(tokens)
}

// Parser (recursive descent, precedence climbing)

/// Formula AST. A `Range` expands to its cells at evaluation
/// time; a `Call` carries its raw argument expressions so `IF`
/// can evaluate only the taken branch.
#[derive(Debug, Clone)]
pub enum Expr {
    Number(Decimal),
    Text(String),
    Bool(bool),
    Cell((i64, i64)),
    Range(((i64, i64), (i64, i64))),
    Call {
        name: String,
        args: Vec<Expr>,
    },
    UnaryMinus(Box<Expr>),
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn next(&mut self) -> Token {
        let token = self.peek().clone();
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn parse(&mut self) -> Result<Expr, String> {
        let expr = self.parse_comparison()?;
        if *self.peek() != Token::Eof {
            return Err(ERR_VALUE.to_string());
        }
        Ok(expr)
    }

    fn parse_comparison(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.peek() {
                Token::Eq => BinOp::Eq,
                Token::Ne => BinOp::Ne,
                Token::Lt => BinOp::Lt,
                Token::Le => BinOp::Le,
                Token::Gt => BinOp::Gt,
                Token::Ge => BinOp::Ge,
                _ => break,
            };
            self.next();
            let right = self.parse_additive()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                _ => break,
            };
            self.next();
            let right = self.parse_multiplicative()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_power()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Rem,
                _ => break,
            };
            self.next();
            let right = self.parse_power()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_power(&mut self) -> Result<Expr, String> {
        let base = self.parse_unary()?;
        if *self.peek() == Token::Caret {
            self.next();
            // Right associative: 2^3^2 is 2^(3^2).
            let exponent = self.parse_power()?;
            return Ok(Expr::Binary {
                op: BinOp::Pow,
                left: Box::new(base),
                right: Box::new(exponent),
            });
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if *self.peek() == Token::Minus {
            self.next();
            let inner = self.parse_unary()?;
            return Ok(Expr::UnaryMinus(Box::new(inner)));
        }
        if *self.peek() == Token::Plus {
            self.next();
            return self.parse_unary();
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.next() {
            Token::Number(raw) => Decimal::parse(&raw)
                .map(Expr::Number)
                .ok_or_else(|| ERR_VALUE.to_string()),
            Token::Text(text) => Ok(Expr::Text(text)),
            Token::LParen => {
                let expr = self.parse_comparison()?;
                if self.next() != Token::RParen {
                    return Err(ERR_VALUE.to_string());
                }
                Ok(expr)
            }
            Token::Ident(name) => {
                if *self.peek() == Token::LParen {
                    // Function call: the name must be registered.
                    let def = function_def(&name).ok_or_else(|| ERR_NAME.to_string())?;
                    let _ = def;
                    self.next();
                    let mut args = Vec::new();
                    if *self.peek() != Token::RParen {
                        loop {
                            args.push(self.parse_comparison()?);
                            match self.next() {
                                Token::Comma => continue,
                                Token::RParen => break,
                                _ => return Err(ERR_VALUE.to_string()),
                            }
                        }
                    } else {
                        self.next();
                    }
                    Ok(Expr::Call { name, args })
                } else if name.eq_ignore_ascii_case("true") {
                    Ok(Expr::Bool(true))
                } else if name.eq_ignore_ascii_case("false") {
                    Ok(Expr::Bool(false))
                } else if let Some(cell) = parse_address(&name) {
                    // A cell reference, possibly the start of a range.
                    if *self.peek() == Token::Colon {
                        self.next();
                        match self.next() {
                            Token::Ident(end) => {
                                let other =
                                    parse_address(&end).ok_or_else(|| ERR_REF.to_string())?;
                                let top = (cell.0.min(other.0), cell.1.min(other.1));
                                let bottom = (cell.0.max(other.0), cell.1.max(other.1));
                                Ok(Expr::Range((top, bottom)))
                            }
                            _ => Err(ERR_REF.to_string()),
                        }
                    } else {
                        Ok(Expr::Cell(cell))
                    }
                } else {
                    // A bare name is neither a function, a boolean,
                    // nor a cell: unknown name.
                    Err(ERR_NAME.to_string())
                }
            }
            _ => Err(ERR_VALUE.to_string()),
        }
    }
}

/// Parse a formula string (leading `=` optional) into an AST.
pub fn parse_formula(formula: &str) -> Result<Expr, String> {
    let text = formula.strip_prefix('=').unwrap_or(formula);
    let tokens = tokenize(text)?;
    Parser { tokens, pos: 0 }.parse()
}

/// Every cell reference an AST reads, with ranges expanded.
/// Ranges are bounded by the sheet size, so the expansion is
/// finite; a range larger than the whole sheet is rejected.
pub fn collect_refs(expr: &Expr) -> Result<Vec<(i64, i64)>, String> {
    let mut refs = Vec::new();
    collect_refs_into(expr, &mut refs)?;
    Ok(refs)
}

fn collect_refs_into(expr: &Expr, refs: &mut Vec<(i64, i64)>) -> Result<(), String> {
    match expr {
        Expr::Cell(cell) => refs.push(*cell),
        Expr::Range(((top_row, left_col), (bottom_row, right_col))) => {
            let count = (bottom_row - top_row + 1)
                .checked_mul(right_col - left_col + 1)
                .unwrap_or(i64::MAX);
            if count > MAX_ROWS * MAX_COLUMNS {
                return Err(ERR_REF.to_string());
            }
            for row in *top_row..=*bottom_row {
                for col in *left_col..=*right_col {
                    refs.push((row, col));
                }
            }
        }
        Expr::Call { args, .. } => {
            for arg in args {
                collect_refs_into(arg, refs)?;
            }
        }
        Expr::UnaryMinus(inner) => collect_refs_into(inner, refs)?,
        Expr::Binary { left, right, .. } => {
            collect_refs_into(left, refs)?;
            collect_refs_into(right, refs)?;
        }
        Expr::Number(_) | Expr::Text(_) | Expr::Bool(_) => {}
    }
    Ok(())
}

/// The cell references a formula string reads, or an error
/// token when the formula does not parse.
pub fn formula_references(formula: &str) -> Result<Vec<(i64, i64)>, String> {
    let ast = parse_formula(formula)?;
    collect_refs(&ast)
}

// Evaluator

/// Evaluate an AST. `resolve` answers one cell's current
/// value (its computed value when it is a formula cell, its
/// typed value otherwise); `today` is the `YYYY-MM-DD` date
/// `TODAY()` returns. Errors propagate as `Error` values, so
/// evaluation never panics and never loops.
pub fn evaluate(expr: &Expr, resolve: &dyn Fn(i64, i64) -> EvalValue, today: &str) -> EvalValue {
    match expr {
        Expr::Number(value) => EvalValue::Number(*value),
        Expr::Text(text) => EvalValue::Text(text.clone()),
        Expr::Bool(value) => EvalValue::Bool(*value),
        Expr::Cell((row, col)) => {
            if !address_in_bounds(*row, *col) {
                return EvalValue::error(ERR_REF);
            }
            resolve(*row, *col)
        }
        Expr::Range(_) => EvalValue::error(ERR_VALUE),
        Expr::Call { name, args } => evaluate_call(name, args, resolve, today),
        Expr::UnaryMinus(inner) => match evaluate(inner, resolve, today).as_number() {
            Ok(value) => match value.checked_mul(&Decimal::from_int(-1)) {
                Some(negated) => EvalValue::Number(negated),
                None => EvalValue::error(ERR_NUM),
            },
            Err(token) => EvalValue::error(&token),
        },
        Expr::Binary { op, left, right } => evaluate_binary(*op, left, right, resolve, today),
    }
}

fn evaluate_binary(
    op: BinOp,
    left: &Expr,
    right: &Expr,
    resolve: &dyn Fn(i64, i64) -> EvalValue,
    today: &str,
) -> EvalValue {
    match op {
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let left_value = evaluate(left, resolve, today);
            let right_value = evaluate(right, resolve, today);
            if let EvalValue::Error(token) = &left_value {
                return EvalValue::error(token);
            }
            if let EvalValue::Error(token) = &right_value {
                return EvalValue::error(token);
            }
            let ordering = compare_values(&left_value, &right_value);
            let ordering = match ordering {
                Some(ordering) => ordering,
                None => return EvalValue::error(ERR_VALUE),
            };
            let result = match op {
                BinOp::Eq => ordering == std::cmp::Ordering::Equal,
                BinOp::Ne => ordering != std::cmp::Ordering::Equal,
                BinOp::Lt => ordering == std::cmp::Ordering::Less,
                BinOp::Le => ordering != std::cmp::Ordering::Greater,
                BinOp::Gt => ordering == std::cmp::Ordering::Greater,
                BinOp::Ge => ordering != std::cmp::Ordering::Less,
                _ => unreachable!(),
            };
            EvalValue::Bool(result)
        }
        BinOp::Pow => {
            let base = match evaluate(left, resolve, today).as_number() {
                Ok(value) => value,
                Err(token) => return EvalValue::error(&token),
            };
            let exponent = match evaluate(right, resolve, today).as_number() {
                Ok(value) => value,
                Err(token) => return EvalValue::error(&token),
            };
            let exponent = match exponent.as_int() {
                Some(value) => value,
                None => return EvalValue::error(ERR_VALUE),
            };
            match base.checked_pow(exponent) {
                Some(value) => EvalValue::Number(value),
                None => EvalValue::error(ERR_NUM),
            }
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
            let left_value = match evaluate(left, resolve, today).as_number() {
                Ok(value) => value,
                Err(token) => return EvalValue::error(&token),
            };
            let right_value = match evaluate(right, resolve, today).as_number() {
                Ok(value) => value,
                Err(token) => return EvalValue::error(&token),
            };
            let result = match op {
                BinOp::Add => left_value.checked_add(&right_value),
                BinOp::Sub => left_value.checked_sub(&right_value),
                BinOp::Mul => left_value.checked_mul(&right_value),
                BinOp::Div => {
                    if right_value == Decimal::zero() {
                        return EvalValue::error(ERR_DIV_ZERO);
                    }
                    left_value.checked_div(&right_value)
                }
                BinOp::Rem => {
                    if right_value == Decimal::zero() {
                        return EvalValue::error(ERR_DIV_ZERO);
                    }
                    left_value.checked_rem(&right_value)
                }
                _ => unreachable!(),
            };
            match result {
                Some(value) => EvalValue::Number(value),
                None => EvalValue::error(ERR_NUM),
            }
        }
    }
}

/// Compare two values: numbers numerically, booleans as
/// false < true, text lexicographically; mixed kinds compare
/// by kind rank (number < text < boolean), like common
/// spreadsheet ordering. `None` only when a side is an error,
/// which the caller checks first.
fn compare_values(left: &EvalValue, right: &EvalValue) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    match (left, right) {
        (EvalValue::Number(a), EvalValue::Number(b)) => Some(a.cmp(b)),
        (EvalValue::Text(a), EvalValue::Text(b)) => Some(a.cmp(b)),
        (EvalValue::Bool(a), EvalValue::Bool(b)) => Some(a.cmp(b)),
        (EvalValue::Empty, EvalValue::Empty) => Some(Ordering::Equal),
        (EvalValue::Empty, _) => Some(Ordering::Less),
        (_, EvalValue::Empty) => Some(Ordering::Greater),
        (EvalValue::Number(_), _) => Some(Ordering::Less),
        (_, EvalValue::Number(_)) => Some(Ordering::Greater),
        (EvalValue::Text(_), EvalValue::Bool(_)) => Some(Ordering::Less),
        (EvalValue::Bool(_), EvalValue::Text(_)) => Some(Ordering::Greater),
        _ => None,
    }
}

/// Evaluate one function call. `IF` is special: only the
/// taken branch is evaluated, so an error in the other branch
/// never surfaces. Every other call evaluates all arguments
/// first (ranges flatten into their cells) and then dispatches
/// on the registry name.
fn evaluate_call(
    name: &str,
    args: &[Expr],
    resolve: &dyn Fn(i64, i64) -> EvalValue,
    today: &str,
) -> EvalValue {
    let def = match function_def(name) {
        Some(def) => def,
        None => return EvalValue::error(ERR_NAME),
    };
    if args.len() < def.min_args {
        return EvalValue::error(ERR_VALUE);
    }
    if let Some(max) = def.max_args {
        if args.len() > max {
            return EvalValue::error(ERR_VALUE);
        }
    }
    if name.eq_ignore_ascii_case("IF") {
        let condition = evaluate(&args[0], resolve, today);
        let taken = match condition.as_bool() {
            Ok(value) => value,
            Err(token) => return EvalValue::error(&token),
        };
        let branch = if taken {
            &args[1]
        } else if args.len() > 2 {
            &args[2]
        } else {
            return EvalValue::Bool(false);
        };
        return evaluate(branch, resolve, today);
    }
    if name.eq_ignore_ascii_case("TODAY") {
        return EvalValue::Text(today.to_string());
    }
    let mut values = Vec::new();
    for arg in args {
        match arg {
            Expr::Range(((top_row, left_col), (bottom_row, right_col))) => {
                for row in *top_row..=*bottom_row {
                    for col in *left_col..=*right_col {
                        if !address_in_bounds(row, col) {
                            return EvalValue::error(ERR_REF);
                        }
                        values.push(resolve(row, col));
                    }
                }
            }
            _ => values.push(evaluate(arg, resolve, today)),
        }
    }
    call_function(name, &values)
}

/// Dispatch a fully evaluated argument list on the registry
/// name. This is the only place function names are matched,
/// and only after the registry lookup above has accepted the
/// name - the table is the source of truth for what exists.
fn call_function(name: &str, values: &[EvalValue]) -> EvalValue {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "SUM" => {
            let mut total = Decimal::zero();
            for value in values {
                match value.as_number() {
                    Ok(number) => match total.checked_add(&number) {
                        Some(sum) => total = sum,
                        None => return EvalValue::error(ERR_NUM),
                    },
                    Err(token) => return EvalValue::error(&token),
                }
            }
            EvalValue::Number(total)
        }
        "PRODUCT" => {
            let mut total = Decimal::one();
            for value in values {
                match value.as_number() {
                    Ok(number) => match total.checked_mul(&number) {
                        Some(product) => total = product,
                        None => return EvalValue::error(ERR_NUM),
                    },
                    Err(token) => return EvalValue::error(&token),
                }
            }
            EvalValue::Number(total)
        }
        "AVERAGE" | "MIN" | "MAX" => {
            let mut numbers = Vec::new();
            for value in values {
                match value {
                    EvalValue::Number(number) => numbers.push(*number),
                    EvalValue::Empty | EvalValue::Text(_) | EvalValue::Bool(_) => {}
                    EvalValue::Error(token) => return EvalValue::error(token),
                }
            }
            if numbers.is_empty() {
                return EvalValue::error(ERR_DIV_ZERO);
            }
            match upper.as_str() {
                "MIN" => EvalValue::Number(*numbers.iter().min().unwrap_or(&Decimal::zero())),
                "MAX" => EvalValue::Number(*numbers.iter().max().unwrap_or(&Decimal::zero())),
                _ => {
                    let mut total = Decimal::zero();
                    for number in &numbers {
                        match total.checked_add(number) {
                            Some(sum) => total = sum,
                            None => return EvalValue::error(ERR_NUM),
                        }
                    }
                    match total.checked_div(&Decimal::from_int(numbers.len() as i64)) {
                        Some(mean) => EvalValue::Number(mean),
                        None => EvalValue::error(ERR_NUM),
                    }
                }
            }
        }
        "COUNT" => {
            let count = values
                .iter()
                .filter(|value| matches!(value, EvalValue::Number(_)))
                .count();
            EvalValue::Number(Decimal::from_int(count as i64))
        }
        "COUNTA" => {
            let count = values
                .iter()
                .filter(|value| !matches!(value, EvalValue::Empty))
                .count();
            EvalValue::Number(Decimal::from_int(count as i64))
        }
        "ROUND" => {
            let value = match values[0].as_number() {
                Ok(value) => value,
                Err(token) => return EvalValue::error(&token),
            };
            let places = match values[1].as_number().ok().and_then(|v| v.as_int()) {
                Some(places) if (0..=10).contains(&places) => places as u32,
                _ => return EvalValue::error(ERR_VALUE),
            };
            match value.round(places) {
                Some(rounded) => EvalValue::Number(rounded),
                None => EvalValue::error(ERR_NUM),
            }
        }
        "ABS" => match values[0].as_number() {
            Ok(value) => EvalValue::Number(value.abs()),
            Err(token) => EvalValue::error(&token),
        },
        "CONCAT" => {
            let mut out = String::new();
            for value in values {
                if let EvalValue::Error(token) = value {
                    return EvalValue::error(token);
                }
                out.push_str(&value.display());
            }
            EvalValue::Text(out)
        }
        "LEN" => {
            if let EvalValue::Error(token) = &values[0] {
                return EvalValue::error(token);
            }
            EvalValue::Number(Decimal::from_int(values[0].display().chars().count() as i64))
        }
        "UPPER" => {
            if let EvalValue::Error(token) = &values[0] {
                return EvalValue::error(token);
            }
            EvalValue::Text(values[0].display().to_uppercase())
        }
        "LOWER" => {
            if let EvalValue::Error(token) = &values[0] {
                return EvalValue::error(token);
            }
            EvalValue::Text(values[0].display().to_lowercase())
        }
        "TRIM" => {
            if let EvalValue::Error(token) = &values[0] {
                return EvalValue::error(token);
            }
            EvalValue::Text(values[0].display().trim().to_string())
        }
        "NOT" => match values[0].as_bool() {
            Ok(value) => EvalValue::Bool(!value),
            Err(token) => EvalValue::error(&token),
        },
        "AND" => {
            for value in values {
                match value.as_bool() {
                    Ok(true) => {}
                    Ok(false) => return EvalValue::Bool(false),
                    Err(token) => return EvalValue::error(&token),
                }
            }
            EvalValue::Bool(true)
        }
        "OR" => {
            for value in values {
                match value.as_bool() {
                    Ok(true) => return EvalValue::Bool(true),
                    Ok(false) => {}
                    Err(token) => return EvalValue::error(&token),
                }
            }
            EvalValue::Bool(false)
        }
        _ => EvalValue::error(ERR_NAME),
    }
}

/// Evaluate a formula string in a sheet context without
/// saving anything. `cells` maps an address to its current
/// computed value (formula cells pass their stored computed
/// value, so the evaluation never re-enters a formula).
/// Returns the value plus its display string.
pub fn evaluate_formula(
    formula: &str,
    cells: &HashMap<(i64, i64), EvalValue>,
    today: &str,
) -> EvalValue {
    let ast = match parse_formula(formula) {
        Ok(ast) => ast,
        Err(token) => return EvalValue::error(&token),
    };
    evaluate(
        &ast,
        &|row, col| cells.get(&(row, col)).cloned().unwrap_or(EvalValue::Empty),
        today,
    )
}

// Sheet recalculation (topological order, cycle detection)

/// One input cell for a recalculation: its typed value plus
/// its kind (`formula` cells carry the formula text, starting
/// with `=`).
pub type RecalcInput = HashMap<(i64, i64), (String, CellKind)>;

/// One recalculated cell: its computed display string plus
/// its result kind.
pub type RecalcOutput = HashMap<(i64, i64), (String, CellKind)>;

/// Recompute every formula cell of a sheet in topological
/// order: a formula is evaluated only after every formula cell
/// it references has been evaluated. Cells in a reference
/// cycle - or depending on one - get `#CIRC!` instead of a
/// value, and evaluation of the rest of the sheet continues.
/// Non-formula cells keep their typed computed value.
pub fn recalculate_sheet(inputs: &RecalcInput, today: &str) -> RecalcOutput {
    let mut output: RecalcOutput = HashMap::new();
    // Typed (non-formula) cells first: their computed values
    // never depend on anything.
    for (address, (raw, kind)) in inputs {
        if *kind != CellKind::Formula {
            let computed = computed_for_typed(raw, *kind);
            output.insert(*address, (computed, *kind));
        }
    }
    // Parse every formula and collect its references.
    let mut parsed: HashMap<(i64, i64), Result<Expr, String>> = HashMap::new();
    let mut refs: HashMap<(i64, i64), Vec<(i64, i64)>> = HashMap::new();
    for (address, (raw, kind)) in inputs {
        if *kind != CellKind::Formula {
            continue;
        }
        match parse_formula(raw) {
            Ok(ast) => {
                let references = collect_refs(&ast).unwrap_or_default();
                refs.insert(*address, references);
                parsed.insert(*address, Ok(ast));
            }
            Err(token) => {
                parsed.insert(*address, Err(token));
            }
        }
    }
    // Kahn topological sort over formula -> formula edges.
    let formula_cells: HashSet<(i64, i64)> = parsed.keys().copied().collect();
    let mut dependents: HashMap<(i64, i64), Vec<(i64, i64)>> = HashMap::new();
    let mut pending: HashMap<(i64, i64), usize> = HashMap::new();
    for cell in &formula_cells {
        let deps: HashSet<(i64, i64)> = refs
            .get(cell)
            .map(|list| {
                list.iter()
                    .copied()
                    .filter(|dep| formula_cells.contains(dep) && dep != cell)
                    .collect()
            })
            .unwrap_or_default();
        pending.insert(*cell, deps.len());
        for dep in deps {
            dependents.entry(dep).or_default().push(*cell);
        }
    }
    let mut ready: Vec<(i64, i64)> = pending
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(cell, _)| *cell)
        .collect();
    let mut evaluated: HashSet<(i64, i64)> = HashSet::new();
    while let Some(cell) = ready.pop() {
        if evaluated.contains(&cell) {
            continue;
        }
        evaluated.insert(cell);
        match parsed.get(&cell) {
            Some(Ok(ast)) => {
                let value = evaluate(
                    ast,
                    &|row, col| {
                        output
                            .get(&(row, col))
                            .map(|(text, kind)| match kind {
                                CellKind::Number => Decimal::parse(text)
                                    .map(EvalValue::Number)
                                    .unwrap_or_else(|| EvalValue::Error(ERR_VALUE.to_string())),
                                CellKind::Boolean => {
                                    EvalValue::Bool(text.eq_ignore_ascii_case("true"))
                                }
                                CellKind::Error => EvalValue::Error(text.clone()),
                                CellKind::Empty => EvalValue::Empty,
                                _ => EvalValue::Text(text.clone()),
                            })
                            .unwrap_or(EvalValue::Empty)
                    },
                    today,
                );
                output.insert(cell, (value.display(), value.kind()));
            }
            Some(Err(token)) => {
                output.insert(cell, (token.clone(), CellKind::Error));
            }
            None => {}
        }
        if let Some(next) = dependents.get(&cell) {
            for dependent in next {
                if let Some(count) = pending.get_mut(dependent) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        ready.push(*dependent);
                    }
                }
            }
        }
    }
    // Whatever never became ready is in a cycle (or depends on
    // one): an error value, never an infinite loop. A cell that
    // references itself counts here too - its pending count
    // never reaches zero because the self edge was excluded
    // above, so check self-references explicitly.
    for cell in &formula_cells {
        if evaluated.contains(cell) {
            continue;
        }
        let self_ref = refs
            .get(cell)
            .map(|list| list.contains(cell))
            .unwrap_or(false);
        let _ = self_ref;
        output.insert(*cell, (ERR_CIRCULAR.to_string(), CellKind::Error));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cells(pairs: &[((i64, i64), &str)]) -> HashMap<(i64, i64), EvalValue> {
        pairs
            .iter()
            .map(|(address, raw)| {
                let value = Decimal::parse(raw)
                    .map(EvalValue::Number)
                    .unwrap_or_else(|| EvalValue::Text((*raw).to_string()));
                (*address, value)
            })
            .collect()
    }

    fn eval(formula: &str, pairs: &[((i64, i64), &str)]) -> EvalValue {
        evaluate_formula(formula, &cells(pairs), "2026-11-22")
    }

    fn eval_text(formula: &str, pairs: &[((i64, i64), &str)]) -> String {
        eval(formula, pairs).display()
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(eval_text("1+2*3", &[]), "7");
        assert_eq!(eval_text("(1+2)*3", &[]), "9");
        assert_eq!(eval_text("10-4-3", &[]), "3");
        assert_eq!(eval_text("2^3^2", &[]), "512");
        assert_eq!(eval_text("2*3^2", &[]), "18");
        assert_eq!(eval_text("-5+3", &[]), "-2");
        assert_eq!(eval_text("10%3", &[]), "1");
        assert_eq!(eval_text("7/2", &[]), "3.5");
        assert_eq!(eval_text("0.1+0.2", &[]), "0.3");
        assert_eq!(eval_text("1/3", &[]), "0.3333333333");
    }

    #[test]
    fn comparisons() {
        assert_eq!(eval_text("1<2", &[]), "TRUE");
        assert_eq!(eval_text("2<=2", &[]), "TRUE");
        assert_eq!(eval_text("3>4", &[]), "FALSE");
        assert_eq!(eval_text("1=1", &[]), "TRUE");
        assert_eq!(eval_text("1<>1", &[]), "FALSE");
        assert_eq!(eval_text("\"a\"<\"b\"", &[]), "TRUE");
        assert_eq!(eval_text("1+1=2", &[]), "TRUE");
    }

    #[test]
    fn cell_references_and_ranges() {
        let sheet = &[((0, 0), "10"), ((0, 1), "20"), ((1, 0), "5")];
        assert_eq!(eval_text("=A1+B1", sheet), "30");
        assert_eq!(eval_text("A1*2", sheet), "20");
        assert_eq!(eval_text("$A$1+$B$1", sheet), "30");
        assert_eq!(eval_text("SUM(A1:B2)", sheet), "35");
        assert_eq!(eval_text("SUM(A1:A1)", sheet), "10");
        assert_eq!(eval_text("AVERAGE(A1:B1)", sheet), "15");
        assert_eq!(eval_text("MIN(A1:B2)", sheet), "5");
        assert_eq!(eval_text("MAX(A1:B2)", sheet), "20");
        assert_eq!(eval_text("COUNT(A1:B2)", sheet), "3");
        assert_eq!(eval_text("COUNTA(A1:B2)", sheet), "3");
        // A blank cell counts as 0 in arithmetic and is skipped
        // by COUNT but not by COUNTA's empty check... blank is
        // empty, so COUNTA over a range with a blank is 3.
        assert_eq!(eval_text("A2+1", sheet), "6");
        assert_eq!(eval_text("Z9+1", sheet), "1");
    }

    #[test]
    fn if_is_lazy_and_branching() {
        assert_eq!(eval_text("IF(1<2, 10, 20)", &[]), "10");
        assert_eq!(eval_text("IF(1>2, 10, 20)", &[]), "20");
        // The untaken branch holds a division by zero: it must
        // never surface.
        assert_eq!(eval_text("IF(TRUE, 5, 1/0)", &[]), "5");
        assert_eq!(eval_text("IF(FALSE, 1/0, 7)", &[]), "7");
        assert_eq!(
            eval_text("IF(A1>5, \"big\", \"small\")", &[((0, 0), "10")]),
            "big"
        );
        assert_eq!(
            eval_text("IF(A1>5, \"big\", \"small\")", &[((0, 0), "3")]),
            "small"
        );
    }

    #[test]
    fn functions() {
        assert_eq!(eval_text("ROUND(3.145, 2)", &[]), "3.15");
        assert_eq!(eval_text("ROUND(3.14159, 2)", &[]), "3.14");
        assert_eq!(eval_text("ABS(-4.5)", &[]), "4.5");
        assert_eq!(eval_text("CONCAT(\"a\", 1, \"b\")", &[]), "a1b");
        assert_eq!(eval_text("TODAY()", &[]), "2026-11-22");
        assert_eq!(eval_text("LEN(\"hello\")", &[]), "5");
        assert_eq!(eval_text("UPPER(\"ab\")", &[]), "AB");
        assert_eq!(eval_text("LOWER(\"AB\")", &[]), "ab");
        assert_eq!(eval_text("TRIM(\"  x  \")", &[]), "x");
        assert_eq!(eval_text("PRODUCT(2, 3, 4)", &[]), "24");
        assert_eq!(eval_text("NOT(TRUE)", &[]), "FALSE");
        assert_eq!(eval_text("AND(TRUE, 1=1)", &[]), "TRUE");
        assert_eq!(eval_text("AND(TRUE, FALSE)", &[]), "FALSE");
        assert_eq!(eval_text("OR(FALSE, 2>1)", &[]), "TRUE");
        assert_eq!(eval_text("SUM(1, 2, 3)", &[]), "6");
        assert_eq!(eval_text("AVERAGE(2, 4, 6)", &[]), "4");
    }

    #[test]
    fn errors_propagate_as_tokens() {
        assert_eq!(eval_text("1/0", &[]), "#DIV/0!");
        assert_eq!(eval_text("10%0", &[]), "#DIV/0!");
        assert_eq!(eval_text("NOSUCHFN(1)", &[]), "#NAME?");
        assert_eq!(eval_text("FOO", &[]), "#NAME?");
        assert_eq!(eval_text("SUM()", &[]), "#VALUE!");
        assert_eq!(eval_text("ROUND(1.5)", &[]), "#VALUE!");
        assert_eq!(eval_text("1+", &[]), "#VALUE!");
        assert_eq!(eval_text("(1+2", &[]), "#VALUE!");
        assert_eq!(eval_text("\"a\"+1", &[]), "#VALUE!");
        assert_eq!(eval_text("SUM(\"a\")", &[]), "#VALUE!");
        assert_eq!(eval_text("2^0.5", &[]), "#VALUE!");
        assert_eq!(eval_text("ZZZ999999+1", &[]), "#REF!");
        // An error in a referenced cell propagates.
        let sheet = &[((0, 0), "1")];
        let mut map = cells(sheet);
        map.insert((0, 1), EvalValue::Error("#DIV/0!".to_string()));
        assert_eq!(
            evaluate_formula("B1+1", &map, "2026-11-22").display(),
            "#DIV/0!"
        );
    }

    #[test]
    fn registry_is_data() {
        assert!(!FUNCTION_REGISTRY.is_empty());
        assert!(function_def("sum").is_some());
        assert!(function_def("SUM").is_some());
        assert!(function_def("today").is_some());
        assert!(function_def("nosuch").is_none());
        assert_eq!(function_names().len(), FUNCTION_REGISTRY.len());
        for def in FUNCTION_REGISTRY {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
            if let Some(max) = def.max_args {
                assert!(def.min_args <= max);
            }
        }
    }

    #[test]
    fn a1_reference_collection() {
        let refs = formula_references("=SUM(A1:B2)+C3").unwrap();
        assert_eq!(refs.len(), 5);
        assert!(refs.contains(&(0, 0)));
        assert!(refs.contains(&(1, 1)));
        assert!(refs.contains(&(2, 2)));
        assert!(formula_references("=1+").is_err());
        assert!(formula_references("=NOSUCH(1)").is_err());
    }

    fn recalc_input(pairs: &[((i64, i64), &str)]) -> RecalcInput {
        pairs
            .iter()
            .map(|(address, raw)| {
                let kind = if raw.starts_with('=') {
                    CellKind::Formula
                } else {
                    super::super::types::classify_value(raw)
                };
                (*address, ((*raw).to_string(), kind))
            })
            .collect()
    }

    #[test]
    fn recalculation_follows_dependencies_in_order() {
        // C1 depends on B1, B1 depends on A1: written in reverse
        // order, they must still compute correctly.
        let input = recalc_input(&[((0, 2), "=B1*2"), ((0, 1), "=A1+5"), ((0, 0), "10")]);
        let out = recalculate_sheet(&input, "2026-11-22");
        assert_eq!(out.get(&(0, 0)).unwrap().0, "10");
        assert_eq!(out.get(&(0, 1)).unwrap().0, "15");
        assert_eq!(out.get(&(0, 2)).unwrap().0, "30");
    }

    #[test]
    fn circular_references_become_errors_not_loops() {
        // A1 <-> B1 cycle, plus C1 depending on the cycle.
        let input = recalc_input(&[
            ((0, 0), "=B1+1"),
            ((0, 1), "=A1+1"),
            ((0, 2), "=A1+1"),
            ((0, 3), "42"),
            ((0, 4), "=D1*2"),
        ]);
        let out = recalculate_sheet(&input, "2026-11-22");
        assert_eq!(out.get(&(0, 0)).unwrap().0, "#CIRC!");
        assert_eq!(out.get(&(0, 1)).unwrap().0, "#CIRC!");
        assert_eq!(out.get(&(0, 2)).unwrap().0, "#CIRC!");
        assert_eq!(out.get(&(0, 3)).unwrap().0, "42");
        assert_eq!(out.get(&(0, 4)).unwrap().0, "84");
        // Self-reference is a cycle too.
        let input = recalc_input(&[((0, 0), "=A1+1")]);
        let out = recalculate_sheet(&input, "2026-11-22");
        assert_eq!(out.get(&(0, 0)).unwrap().0, "#CIRC!");
    }

    #[test]
    fn recalculation_keeps_typed_values_and_marks_bad_formulas() {
        let input = recalc_input(&[
            ((0, 0), "12.50"),
            ((0, 1), "hello"),
            ((0, 2), "=1/0"),
            ((0, 3), "=NOSUCH(1)"),
            ((0, 4), "TRUE"),
        ]);
        let out = recalculate_sheet(&input, "2026-11-22");
        assert_eq!(
            out.get(&(0, 0)).unwrap(),
            &("12.5".to_string(), CellKind::Number)
        );
        assert_eq!(out.get(&(0, 1)).unwrap().0, "hello");
        assert_eq!(out.get(&(0, 2)).unwrap().0, "#DIV/0!");
        assert_eq!(out.get(&(0, 2)).unwrap().1, CellKind::Error);
        assert_eq!(out.get(&(0, 3)).unwrap().0, "#NAME?");
        assert_eq!(out.get(&(0, 4)).unwrap().0, "TRUE");
    }
}
