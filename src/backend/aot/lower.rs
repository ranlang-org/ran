//! Lowering: a checked Ran program (D2 subset) -> portable C source.
//!
//! D1 supported a monomorphic scalar core (functions/recursion, `if`/`while`/
//! `for x in range(...)`, checked int arithmetic, bool, strings + concat +
//! `echo`). D2 keeps that **unboxed fast path** — a value the analyzer proves is
//! a plain `int`/`bool`/`float`/`str` is still a raw C scalar — and extends the
//! subset to the data-type layer using a tagged `RanValue` (see `ran_rt.h`):
//!
//!   * `float` arithmetic + comparisons (unboxed `double`);
//!   * exact `decimal` money math via the `dec(...)` builtin + operators
//!     (`RanValue` RAN_DEC, exact rounding; overflow E1003, /0 E1002);
//!   * array literals, bounds-checked indexing (E1012), and `len(...)`;
//!   * struct-literal construction + field access (`obj.field`);
//!   * `match` (literal / binding / wildcard patterns, incl. `return` from an arm).
//!
//! Memory: heap `RanValue` payloads (string/array/object) are reference-counted
//! in the runtime. Generated code follows a strict discipline — a variable owns
//! one reference (retained on store, released on reassignment and at scope/
//! function exit); operations borrow their operands; per-statement temporaries
//! are released at the end of the statement — so there is no leak, double-free,
//! or use-after-free.
//!
//! Anything outside this subset is a HARD build error (`E0606`) via
//! [`supported`] — never a silent fallback to the interpreter.

use crate::frontend::ast::{
    BinaryOperator, Expression, MatchArm, Param, Pattern, Statement, Stmt, TypeExpr, UnaryOperator,
};
use crate::semantics::analyzer::CheckedProgram;
use crate::support::decimal::Decimal;
use crate::support::diagnostics::{Diagnostic, SourceLoc};
use std::collections::HashMap;

/// Lowered C types. Scalars are unboxed; `Value` is a tagged `RanValue` used for
/// decimal, arrays, and struct/object values (and dynamically-typed results of
/// indexing / field access).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CType {
    Int,
    Bool,
    Float,
    Str,
    Value,
}

impl CType {
    fn c_decl(self) -> &'static str {
        match self {
            CType::Int => "int64_t",
            CType::Bool => "bool",
            CType::Float => "double",
            CType::Str => "const char*",
            CType::Value => "RanValue",
        }
    }
}

#[derive(Debug, Clone)]
struct FnSig {
    params: Vec<(String, CType)>,
    ret: Option<CType>,
}

/// Struct definitions: type name -> ordered field names.
type StructDefs = HashMap<String, Vec<String>>;

fn map_type(t: &TypeExpr, structs: &StructDefs) -> Option<CType> {
    match t {
        TypeExpr::Named(n) => match n.as_str() {
            "int" | "i64" | "i32" | "int64" => Some(CType::Int),
            "bool" => Some(CType::Bool),
            "float" | "f64" | "f32" | "double" => Some(CType::Float),
            "str" | "string" | "String" => Some(CType::Str),
            "decimal" | "Decimal" => Some(CType::Value),
            other if structs.contains_key(other) => Some(CType::Value),
            _ => None,
        },
        TypeExpr::Array(_) => Some(CType::Value),
        _ => None,
    }
}

fn c_ident(name: &str) -> String {
    if name == "main" {
        "main".to_string()
    } else {
        format!("r_{}", name)
    }
}

/// A C identifier for a struct's static field-name table.
fn struct_fields_sym(name: &str) -> String {
    format!("_ranf_{}", name)
}

fn has_interpolation(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            let n = chars[i + 1];
            if n == '{' || n.is_alphanumeric() || n == '_' {
                return true;
            }
        }
        i += 1;
    }
    false
}

// ===========================================================================
// Pre-flight: supported(checked) — reject anything outside the D2 subset.
// ===========================================================================

fn unsupported(file: &str, span_line: usize, span_col: usize, what: &str) -> Diagnostic {
    Diagnostic::from_code("E0606", format!("native codegen does not yet support {}", what))
        .with_loc(SourceLoc::new(
            file,
            span_line.max(1),
            span_col.max(1),
            span_col.max(1) + 1,
        ))
        .with_help(
            "this construct is outside the D2 native subset (functions, control flow, \
             int/float/bool/str, decimal, arrays, structs, match); build without \
             `--native` to use the interpreter-backed binary, or wait for a later \
             native iteration (D3+)",
        )
}

/// Verify a program lies entirely within the D2 native subset.
pub fn supported(checked: &CheckedProgram, file: &str) -> Result<(), Diagnostic> {
    let structs = collect_structs(checked);
    let fns = collect_fn_names(checked);
    for stmt in &checked.program.statements {
        match &stmt.kind {
            Statement::FnDecl { body, params, return_type, .. } => {
                for p in params {
                    if let Some(t) = &p.type_annotation {
                        if map_type(t, &structs).is_none() {
                            return Err(unsupported(
                                file,
                                stmt.span.line,
                                stmt.span.col,
                                &format!("parameter type `{}`", type_name(t)),
                            ));
                        }
                    }
                }
                if let Some(rt) = return_type {
                    if map_type(rt, &structs).is_none() {
                        return Err(unsupported(
                            file,
                            stmt.span.line,
                            stmt.span.col,
                            &format!("return type `{}`", type_name(rt)),
                        ));
                    }
                }
                check_block(body, file, &fns, &structs)?;
            }
            // Struct declarations are metadata for codegen; allowed at top level.
            Statement::StructDecl { .. } => {}
            other => {
                return Err(unsupported(
                    file,
                    stmt.span.line,
                    stmt.span.col,
                    &format!(
                        "top-level {} (only function and struct declarations are supported)",
                        stmt_kind(other)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn type_name(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::Ref { .. } => "reference".into(),
        TypeExpr::Array(_) => "array".into(),
        TypeExpr::Generic { name, .. } => name.clone(),
        TypeExpr::Function { .. } => "function".into(),
        TypeExpr::Channel(_) => "channel".into(),
    }
}

fn stmt_kind(s: &Statement) -> &'static str {
    match s {
        Statement::VarDecl { .. } => "variable declaration",
        Statement::FnDecl { .. } => "nested function",
        Statement::StructDecl { .. } => "struct declaration",
        Statement::EnumDecl { .. } => "enum declaration",
        Statement::ImplBlock { .. } => "impl block",
        Statement::TraitDecl { .. } => "trait declaration",
        Statement::Expr(_) => "expression statement",
        Statement::Echo { .. } => "echo",
        Statement::Return(_) => "return",
        Statement::Break => "break",
        Statement::Continue => "continue",
        Statement::If { .. } => "if",
        Statement::For { .. } => "for",
        Statement::While { .. } => "while",
        Statement::Spawn { .. } => "spawn",
        Statement::Import { .. } => "import",
    }
}

fn collect_fn_names(checked: &CheckedProgram) -> Vec<String> {
    checked
        .program
        .statements
        .iter()
        .filter_map(|s| match &s.kind {
            Statement::FnDecl { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn collect_structs(checked: &CheckedProgram) -> StructDefs {
    let mut m = HashMap::new();
    for s in &checked.program.statements {
        if let Statement::StructDecl { name, fields, .. } = &s.kind {
            m.insert(name.clone(), fields.iter().map(|f| f.name.clone()).collect());
        }
    }
    m
}

fn is_builtin_call(name: &str) -> bool {
    matches!(name, "dec" | "decimal" | "len")
}

fn check_block(body: &[Stmt], file: &str, fns: &[String], structs: &StructDefs) -> Result<(), Diagnostic> {
    for stmt in body {
        check_stmt(stmt, file, fns, structs)?;
    }
    Ok(())
}

fn check_stmt(stmt: &Stmt, file: &str, fns: &[String], structs: &StructDefs) -> Result<(), Diagnostic> {
    let (l, c) = (stmt.span.line, stmt.span.col);
    match &stmt.kind {
        Statement::VarDecl { value, .. } => check_expr(value, file, fns, structs, l, c),
        Statement::Echo { expr, .. } => {
            if let Expression::StringLiteral(_) = expr {
                Ok(())
            } else {
                check_expr(expr, file, fns, structs, l, c)
            }
        }
        Statement::Return(Some(e)) => check_expr(e, file, fns, structs, l, c),
        Statement::Return(None) => Ok(()),
        Statement::Break | Statement::Continue => Ok(()),
        // `match` is supported as a statement (it lowers to an if/else chain).
        Statement::Expr(Expression::Match { subject, arms }) => {
            check_match(subject, arms, file, fns, structs, l, c)
        }
        Statement::Expr(e) => check_expr(e, file, fns, structs, l, c),
        Statement::If { condition, then_body, else_body } => {
            check_expr(condition, file, fns, structs, l, c)?;
            check_block(then_body, file, fns, structs)?;
            if let Some(eb) = else_body {
                check_block(eb, file, fns, structs)?;
            }
            Ok(())
        }
        Statement::While { condition, body } => {
            check_expr(condition, file, fns, structs, l, c)?;
            check_block(body, file, fns, structs)
        }
        Statement::For { iterable, body, .. } => match iterable {
            Expression::FnCall { callee, args } => {
                if let Expression::Variable(name) = callee.as_ref() {
                    if name == "range" && (args.len() == 1 || args.len() == 2) {
                        for a in args {
                            check_expr(a, file, fns, structs, l, c)?;
                        }
                        return check_block(body, file, fns, structs);
                    }
                }
                Err(unsupported(file, l, c, "for-in iterables other than `range(...)`"))
            }
            _ => Err(unsupported(file, l, c, "for-in iterables other than `range(...)`")),
        },
        other => Err(unsupported(file, l, c, stmt_kind(other))),
    }
}

fn check_match(
    subject: &Expression,
    arms: &[MatchArm],
    file: &str,
    fns: &[String],
    structs: &StructDefs,
    l: usize,
    c: usize,
) -> Result<(), Diagnostic> {
    check_expr(subject, file, fns, structs, l, c)?;
    for arm in arms {
        match &arm.pattern {
            Pattern::Wildcard | Pattern::Variable(_) => {}
            Pattern::Literal(expr) => match expr {
                Expression::IntLiteral(_)
                | Expression::BoolLiteral(_)
                | Expression::StringLiteral(_)
                | Expression::FloatLiteral(_) => {}
                _ => {
                    return Err(unsupported(
                        file,
                        l,
                        c,
                        "match patterns other than literals, bindings, and `_` \
                         (enum-variant patterns arrive in a later iteration)",
                    ))
                }
            },
        }
        check_block(&arm.body, file, fns, structs)?;
    }
    Ok(())
}

fn check_expr(
    expr: &Expression,
    file: &str,
    fns: &[String],
    structs: &StructDefs,
    l: usize,
    c: usize,
) -> Result<(), Diagnostic> {
    match expr {
        Expression::IntLiteral(_)
        | Expression::BoolLiteral(_)
        | Expression::FloatLiteral(_)
        | Expression::Variable(_) => Ok(()),
        Expression::StringLiteral(s) => {
            if has_interpolation(s) {
                Err(unsupported(
                    file,
                    l,
                    c,
                    "string interpolation outside `echo` (only `echo \"... $x\"` is supported)",
                ))
            } else {
                Ok(())
            }
        }
        Expression::BinaryOp { left, op: _, right } => {
            check_expr(left, file, fns, structs, l, c)?;
            check_expr(right, file, fns, structs, l, c)
        }
        Expression::UnaryOp { op, operand } => match op {
            UnaryOperator::Neg | UnaryOperator::Not => check_expr(operand, file, fns, structs, l, c),
            UnaryOperator::Ref | UnaryOperator::Deref | UnaryOperator::MutRef => {
                Err(unsupported(file, l, c, "reference / dereference operators"))
            }
        },
        Expression::Array(items) => {
            for it in items {
                check_expr(it, file, fns, structs, l, c)?;
            }
            Ok(())
        }
        Expression::Index { object, index } => {
            check_expr(object, file, fns, structs, l, c)?;
            check_expr(index, file, fns, structs, l, c)
        }
        Expression::FieldAccess { object, .. } => check_expr(object, file, fns, structs, l, c),
        Expression::StructInit { name, fields } => {
            if !structs.contains_key(name) {
                return Err(unsupported(
                    file,
                    l,
                    c,
                    &format!("construction of unknown struct `{}`", name),
                ));
            }
            for (_, fexpr) in fields {
                check_expr(fexpr, file, fns, structs, l, c)?;
            }
            Ok(())
        }
        Expression::FnCall { callee, args } => match callee.as_ref() {
            Expression::Variable(name) if fns.iter().any(|f| f == name) || is_builtin_call(name) => {
                for a in args {
                    check_expr(a, file, fns, structs, l, c)?;
                }
                Ok(())
            }
            Expression::Variable(name) => Err(unsupported(
                file,
                l,
                c,
                &format!("call to `{}` (built-in / stdlib calls are not in the native subset)", name),
            )),
            _ => Err(unsupported(file, l, c, "indirect / computed calls")),
        },
        Expression::MethodCall { .. } => Err(unsupported(file, l, c, "method calls")),
        Expression::Pipe { .. } => Err(unsupported(file, l, c, "pipe expressions")),
        Expression::ChanSend { .. } | Expression::ChanRecv { .. } => {
            Err(unsupported(file, l, c, "channels"))
        }
        Expression::Lambda { .. } => Err(unsupported(file, l, c, "closures")),
        Expression::Await(_) => Err(unsupported(file, l, c, "await")),
        Expression::Match { .. } => Err(unsupported(
            file,
            l,
            c,
            "`match` as a value expression (use it as a statement)",
        )),
    }
}

// ===========================================================================
// Scopes — lexical type stack + tracking of Value-typed locals for refcount
// release. A function `let` that holds a heap `RanValue` is recorded so the
// emitter can release it on scope/function exit, on reassignment, and on
// break/continue/return.
// ===========================================================================

struct Scopes {
    stack: Vec<HashMap<String, CType>>,
    /// Parallel to `stack`: C identifiers of Value-typed `let` locals declared
    /// in each scope (params are NOT tracked — the caller owns them).
    val_locals: Vec<Vec<String>>,
}

impl Scopes {
    fn new() -> Self {
        Scopes { stack: vec![HashMap::new()], val_locals: vec![Vec::new()] }
    }
    fn push(&mut self) {
        self.stack.push(HashMap::new());
        self.val_locals.push(Vec::new());
    }
    fn pop(&mut self) {
        self.stack.pop();
        self.val_locals.pop();
    }
    fn lookup(&self, name: &str) -> Option<CType> {
        for scope in self.stack.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(*t);
            }
        }
        None
    }
    /// Declare a type binding only (used during inference; no release tracking).
    fn declare(&mut self, name: &str, t: CType) {
        self.stack.last_mut().unwrap().insert(name.to_string(), t);
    }
    /// Declare a parameter (typed, never released by the callee).
    fn declare_param(&mut self, name: &str, t: CType) {
        self.stack.last_mut().unwrap().insert(name.to_string(), t);
    }
    /// Declare a `let` local; Value-typed locals are tracked for release.
    fn declare_local(&mut self, name: &str, t: CType) {
        self.stack.last_mut().unwrap().insert(name.to_string(), t);
        if t == CType::Value {
            let cn = c_ident(name);
            let top = self.val_locals.last_mut().unwrap();
            if !top.contains(&cn) {
                top.push(cn);
            }
        }
    }
}

// ===========================================================================
// Type inference (subset-only).
// ===========================================================================

fn infer_expr(
    expr: &Expression,
    scopes: &Scopes,
    sigs: &HashMap<String, FnSig>,
    structs: &StructDefs,
) -> Result<CType, String> {
    match expr {
        Expression::IntLiteral(_) => Ok(CType::Int),
        Expression::FloatLiteral(_) => Ok(CType::Float),
        Expression::BoolLiteral(_) => Ok(CType::Bool),
        Expression::StringLiteral(_) => Ok(CType::Str),
        Expression::Variable(name) => scopes
            .lookup(name)
            .ok_or_else(|| format!("unknown variable `{}`", name)),
        Expression::Array(_) => Ok(CType::Value),
        Expression::Index { .. } => Ok(CType::Value),
        Expression::FieldAccess { .. } => Ok(CType::Value),
        Expression::StructInit { .. } => Ok(CType::Value),
        Expression::UnaryOp { op, operand } => match op {
            UnaryOperator::Not => Ok(CType::Bool),
            UnaryOperator::Neg => {
                let t = infer_expr(operand, scopes, sigs, structs).unwrap_or(CType::Int);
                match t {
                    CType::Float => Ok(CType::Float),
                    CType::Value => Ok(CType::Value),
                    _ => Ok(CType::Int),
                }
            }
            _ => Err("unsupported unary operator".into()),
        },
        Expression::BinaryOp { left, op, right } => {
            use BinaryOperator::*;
            match op {
                Eq | Neq | Lt | Lte | Gt | Gte | And | Or => Ok(CType::Bool),
                Add | Sub | Mul | Div | Mod => {
                    let lt = infer_expr(left, scopes, sigs, structs)?;
                    let rt = infer_expr(right, scopes, sigs, structs)?;
                    if lt == CType::Value || rt == CType::Value {
                        Ok(CType::Value)
                    } else if lt == CType::Str || rt == CType::Str {
                        Ok(CType::Str)
                    } else if lt == CType::Float || rt == CType::Float {
                        Ok(CType::Float)
                    } else {
                        Ok(CType::Int)
                    }
                }
            }
        }
        Expression::FnCall { callee, .. } => {
            if let Expression::Variable(name) = callee.as_ref() {
                match name.as_str() {
                    "len" => Ok(CType::Int),
                    "dec" | "decimal" => Ok(CType::Value),
                    _ => match sigs.get(name) {
                        Some(sig) => sig
                            .ret
                            .ok_or_else(|| format!("function `{}` returns nothing", name)),
                        None => Err(format!("unknown function `{}`", name)),
                    },
                }
            } else {
                Err("indirect call".into())
            }
        }
        _ => Err("expression not in native subset".into()),
    }
}

// ===========================================================================
// Function signature resolution (with return-type inference + recursion).
// ===========================================================================

fn param_ctype(p: &Param, structs: &StructDefs) -> CType {
    p.type_annotation
        .as_ref()
        .and_then(|t| map_type(t, structs))
        .unwrap_or(CType::Int)
}

fn resolve_signatures(
    checked: &CheckedProgram,
    structs: &StructDefs,
) -> Result<HashMap<String, FnSig>, Diagnostic> {
    let mut sigs: HashMap<String, FnSig> = HashMap::new();

    for stmt in &checked.program.statements {
        if let Statement::FnDecl { name, params, return_type, .. } = &stmt.kind {
            let p: Vec<(String, CType)> = params
                .iter()
                .map(|pp| (pp.name.clone(), param_ctype(pp, structs)))
                .collect();
            let ret = return_type.as_ref().and_then(|t| map_type(t, structs));
            sigs.insert(name.clone(), FnSig { params: p, ret });
        }
    }

    let unannotated: Vec<String> = checked
        .program
        .statements
        .iter()
        .filter_map(|s| match &s.kind {
            Statement::FnDecl { name, return_type, .. } if return_type.is_none() => Some(name.clone()),
            _ => None,
        })
        .collect();

    for _ in 0..(unannotated.len() + 1) {
        let mut changed = false;
        for stmt in &checked.program.statements {
            if let Statement::FnDecl { name, params, return_type, body, .. } = &stmt.kind {
                if return_type.is_some() {
                    continue;
                }
                if sigs.get(name).and_then(|s| s.ret).is_some() {
                    continue;
                }
                let mut scopes = Scopes::new();
                for pp in params {
                    scopes.declare(&pp.name, param_ctype(pp, structs));
                }
                if let Some(t) = infer_return_type(body, &mut scopes, &sigs, structs) {
                    if let Some(sig) = sigs.get_mut(name) {
                        sig.ret = Some(t);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    Ok(sigs)
}

fn infer_return_type(
    body: &[Stmt],
    scopes: &mut Scopes,
    sigs: &HashMap<String, FnSig>,
    structs: &StructDefs,
) -> Option<CType> {
    for stmt in body {
        match &stmt.kind {
            Statement::Return(Some(e)) => {
                if let Ok(t) = infer_expr(e, scopes, sigs, structs) {
                    return Some(t);
                }
            }
            Statement::VarDecl { name, value, .. } => {
                if let Ok(t) = infer_expr(value, scopes, sigs, structs) {
                    scopes.declare(name, t);
                }
            }
            Statement::If { then_body, else_body, .. } => {
                if let Some(t) = infer_return_type(then_body, scopes, sigs, structs) {
                    return Some(t);
                }
                if let Some(eb) = else_body {
                    if let Some(t) = infer_return_type(eb, scopes, sigs, structs) {
                        return Some(t);
                    }
                }
            }
            Statement::While { body, .. } => {
                if let Some(t) = infer_return_type(body, scopes, sigs, structs) {
                    return Some(t);
                }
            }
            Statement::For { variable, body, .. } => {
                scopes.push();
                scopes.declare(variable, CType::Int);
                let t = infer_return_type(body, scopes, sigs, structs);
                scopes.pop();
                if t.is_some() {
                    return t;
                }
            }
            Statement::Expr(Expression::Match { arms, .. }) => {
                for arm in arms {
                    if let Some(t) = infer_return_type(&arm.body, scopes, sigs, structs) {
                        return Some(t);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ===========================================================================
// Emission: CheckedProgram -> C source string.
// ===========================================================================

fn codegen_err(file: &str, line: usize, col: usize, msg: &str) -> Diagnostic {
    Diagnostic::from_code("E0602", format!("native code emission failed: {}", msg))
        .with_loc(SourceLoc::new(file, line.max(1), col.max(1), col.max(1) + 1))
        .with_help("add a type annotation, or build without `--native` to use the interpreter-backed binary")
}

fn indent(n: usize) -> String {
    "    ".repeat(n)
}

/// Convert a C value expression of the given (scalar) type to a `const char*`.
fn to_cstr(val: &str, ty: CType) -> String {
    match ty {
        CType::Str => val.to_string(),
        CType::Int => format!("ran_int_to_str({})", val),
        CType::Bool => format!("ran_bool_to_str({})", val),
        CType::Float => format!("ran_float_to_str({})", val),
        CType::Value => format!("ran_value_to_str({})", val),
    }
}

/// Render a Ran string value as a C string literal.
fn c_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{:03o}", b)),
        }
    }
    out.push('"');
    out
}

/// Emit a C `double` literal that exactly reproduces the f64 bit pattern.
fn fmt_float_literal(f: f64) -> String {
    // Rust's `{:?}` for f64 is a shortest round-trippable form that always
    // carries a decimal point (e.g. 10.0 -> "10.0"), valid as a C double.
    format!("({:?})", f)
}

enum Seg {
    Lit(String),
    Var(String),
}

/// Compile-time interpolation of a string literal (mirrors the interpreter's
/// simple-variable substitution). A Value-typed variable is rendered via
/// `ran_value_to_str`; scalars via their type-specific converter.
fn build_interpolated(s: &str, scopes: &Scopes) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut segs: Vec<Seg> = Vec::new();
    let mut lit = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            i += 1;
            let mut path = String::new();
            if chars[i] == '{' {
                i += 1;
                while i < chars.len() && chars[i] != '}' {
                    path.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
            } else {
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                {
                    path.push(chars[i]);
                    i += 1;
                }
                while path.ends_with('.') {
                    path.pop();
                    i -= 1;
                }
            }
            let resolved = if !path.contains('.') {
                scopes.lookup(&path).map(|t| (path.clone(), t))
            } else {
                None
            };
            match resolved {
                Some((name, _)) => {
                    if !lit.is_empty() {
                        segs.push(Seg::Lit(std::mem::take(&mut lit)));
                    }
                    segs.push(Seg::Var(name));
                }
                None => {
                    lit.push('$');
                    lit.push_str(&path);
                }
            }
        } else {
            lit.push(chars[i]);
            i += 1;
        }
    }
    if !lit.is_empty() {
        segs.push(Seg::Lit(lit));
    }
    if segs.is_empty() {
        return "\"\"".to_string();
    }
    let mut expr: Option<String> = None;
    for seg in segs {
        let piece = match seg {
            Seg::Lit(text) => c_string_literal(&text),
            Seg::Var(name) => {
                let ty = scopes.lookup(&name).unwrap_or(CType::Str);
                to_cstr(&c_ident(&name), ty)
            }
        };
        expr = Some(match expr {
            None => piece,
            Some(prev) => format!("ran_concat({}, {})", prev, piece),
        });
    }
    expr.unwrap()
}

struct Emit<'a> {
    out: String,
    sigs: &'a HashMap<String, FnSig>,
    structs: &'a StructDefs,
    file: &'a str,
    tmp: usize,
    svar: usize,
    scopes: Scopes,
    ret_ty: Option<CType>,
    is_main: bool,
    /// Scope-stack index recorded at each enclosing loop body (for break/continue).
    loop_marks: Vec<usize>,
}

impl<'a> Emit<'a> {
    fn new(sigs: &'a HashMap<String, FnSig>, structs: &'a StructDefs, file: &'a str) -> Self {
        Emit {
            out: String::new(),
            sigs,
            structs,
            file,
            tmp: 0,
            svar: 0,
            scopes: Scopes::new(),
            ret_ty: None,
            is_main: false,
            loop_marks: Vec::new(),
        }
    }

    fn fresh_t(&mut self) -> String {
        let n = self.tmp;
        self.tmp += 1;
        format!("_t{}", n)
    }
    fn fresh_s(&mut self) -> String {
        let n = self.svar;
        self.svar += 1;
        format!("_s{}", n)
    }
    fn line(&mut self, depth: usize, s: &str) {
        self.out.push_str(&indent(depth));
        self.out.push_str(s);
        self.out.push('\n');
    }
    fn infer(&self, e: &Expression) -> Result<CType, Diagnostic> {
        infer_expr(e, &self.scopes, self.sigs, self.structs)
            .map_err(|m| codegen_err(self.file, 0, 0, &m))
    }
    fn release_range(&mut self, depth: usize, start: usize, end: usize) {
        for k in (start..end).rev() {
            self.line(depth, &format!("ran_release(_t{});", k));
        }
    }
    /// Release the Value locals of the current top scope (does NOT pop).
    fn release_top_scope(&mut self, depth: usize) {
        let names: Vec<String> = self.scopes.val_locals.last().cloned().unwrap_or_default();
        for n in names.iter().rev() {
            self.line(depth, &format!("ran_release({});", n));
        }
    }
    fn scope_pop(&mut self, depth: usize) {
        self.release_top_scope(depth);
        self.scopes.pop();
    }
    /// Release all Value locals in scope (used before a `return`).
    fn emit_return_cleanup(&mut self, depth: usize) {
        let snapshot: Vec<Vec<String>> = self.scopes.val_locals.clone();
        for scope in snapshot.iter().rev() {
            for n in scope.iter().rev() {
                self.line(depth, &format!("ran_release({});", n));
            }
        }
    }
    /// Release Value locals from the top scope down to the innermost loop body
    /// (used before a `break`/`continue`).
    fn emit_loop_cleanup(&mut self, depth: usize) {
        let mark = *self.loop_marks.last().unwrap_or(&0);
        let snapshot: Vec<Vec<String>> = self.scopes.val_locals.clone();
        for idx in (mark..snapshot.len()).rev() {
            for n in snapshot[idx].iter().rev() {
                self.line(depth, &format!("ran_release({});", n));
            }
        }
    }

    // -- Scalar emission: returns inline C code for a scalar-typed expression. --
    fn emit_scalar(&mut self, expr: &Expression, depth: usize) -> Result<(String, CType), Diagnostic> {
        let (l, c) = (0usize, 0usize);
        match expr {
            Expression::IntLiteral(n) => Ok((format!("INT64_C({})", n), CType::Int)),
            Expression::FloatLiteral(f) => Ok((fmt_float_literal(*f), CType::Float)),
            Expression::BoolLiteral(b) => {
                Ok(((if *b { "true" } else { "false" }).to_string(), CType::Bool))
            }
            Expression::StringLiteral(s) => Ok((c_string_literal(s), CType::Str)),
            Expression::Variable(name) => match self.scopes.lookup(name) {
                Some(CType::Value) => Err(codegen_err(
                    self.file,
                    l,
                    c,
                    &format!("value variable `{}` used in scalar context", name),
                )),
                Some(t) => Ok((c_ident(name), t)),
                None => Err(codegen_err(self.file, l, c, &format!("unknown variable `{}`", name))),
            },
            Expression::UnaryOp { op, operand } => {
                let (v, t) = self.emit_scalar(operand, depth)?;
                match op {
                    UnaryOperator::Neg => Ok((format!("(-({}))", v), t)),
                    UnaryOperator::Not => Ok((format!("(!({}))", v), CType::Bool)),
                    _ => Err(codegen_err(self.file, l, c, "unsupported unary operator")),
                }
            }
            Expression::BinaryOp { left, op, right } => self.emit_binary_scalar(left, op, right, depth),
            Expression::FnCall { callee, args } => {
                let name = match callee.as_ref() {
                    Expression::Variable(n) => n,
                    _ => return Err(codegen_err(self.file, l, c, "indirect call")),
                };
                if name == "len" {
                    if let Some(arg) = args.first() {
                        let aty = self.infer(arg)?;
                        if aty == CType::Value {
                            let t = self.emit_value(arg, depth)?;
                            return Ok((format!("ran_len({})", t), CType::Int));
                        } else if aty == CType::Str {
                            let (code, _) = self.emit_scalar(arg, depth)?;
                            return Ok((format!("((int64_t)strlen({}))", code), CType::Int));
                        }
                    }
                    return Ok(("INT64_C(0)".to_string(), CType::Int));
                }
                // User function returning a scalar.
                let sig = self
                    .sigs
                    .get(name)
                    .ok_or_else(|| codegen_err(self.file, l, c, &format!("unknown function `{}`", name)))?
                    .clone();
                let parts = self.emit_call_args(&sig, args, depth)?;
                let ty = sig.ret.ok_or_else(|| {
                    codegen_err(self.file, l, c, &format!("`{}` returns nothing and cannot be used as a value", name))
                })?;
                Ok((format!("{}({})", c_ident(name), parts.join(", ")), ty))
            }
            _ => Err(codegen_err(self.file, l, c, "expression not in native subset")),
        }
    }

    fn emit_binary_scalar(
        &mut self,
        left: &Expression,
        op: &BinaryOperator,
        right: &Expression,
        depth: usize,
    ) -> Result<(String, CType), Diagnostic> {
        use BinaryOperator::*;
        let lt = self.infer(left)?;
        let rt = self.infer(right)?;
        let value_operand = lt == CType::Value || rt == CType::Value;

        // Logical operators -> bool.
        if matches!(op, And | Or) {
            let connector = if matches!(op, And) { "&&" } else { "||" };
            if value_operand {
                let lv = self.emit_value(left, depth)?;
                let rv = self.emit_value(right, depth)?;
                return Ok((
                    format!("(ran_truthy({}) {} ran_truthy({}))", lv, connector, rv),
                    CType::Bool,
                ));
            }
            let (lv, _) = self.emit_scalar(left, depth)?;
            let (rv, _) = self.emit_scalar(right, depth)?;
            return Ok((format!("({} {} {})", lv, connector, rv), CType::Bool));
        }

        // Comparisons -> bool.
        if matches!(op, Eq | Neq | Lt | Lte | Gt | Gte) {
            if value_operand {
                let lv = self.emit_value(left, depth)?;
                let rv = self.emit_value(right, depth)?;
                let helper = match op {
                    Eq => "ran_eq",
                    Neq => "ran_neq",
                    Lt => "ran_lt",
                    Lte => "ran_lte",
                    Gt => "ran_gt",
                    Gte => "ran_gte",
                    _ => unreachable!(),
                };
                return Ok((format!("{}({}, {})", helper, lv, rv), CType::Bool));
            }
            let (lv, lty) = self.emit_scalar(left, depth)?;
            let (rv, rty) = self.emit_scalar(right, depth)?;
            // String comparison via strcmp.
            if lty == CType::Str && rty == CType::Str {
                let cmp = format!("strcmp({}, {})", lv, rv);
                let c = match op {
                    Eq => format!("({} == 0)", cmp),
                    Neq => format!("({} != 0)", cmp),
                    Lt => format!("({} < 0)", cmp),
                    Lte => format!("({} <= 0)", cmp),
                    Gt => format!("({} > 0)", cmp),
                    Gte => format!("({} >= 0)", cmp),
                    _ => unreachable!(),
                };
                return Ok((c, CType::Bool));
            }
            // Numeric (int/float). Mixed int/float compare as double.
            let (lc, rc) = if lty == CType::Float || rty == CType::Float {
                (format!("(double)({})", lv), format!("(double)({})", rv))
            } else {
                (lv, rv)
            };
            let sym = match op {
                Eq => "==",
                Neq => "!=",
                Lt => "<",
                Lte => "<=",
                Gt => ">",
                Gte => ">=",
                _ => unreachable!(),
            };
            return Ok((format!("({} {} {})", lc, sym, rc), CType::Bool));
        }

        // Arithmetic with a scalar result (no Value operand here).
        let (lv, lty) = self.emit_scalar(left, depth)?;
        let (rv, rty) = self.emit_scalar(right, depth)?;

        // String concatenation (+).
        if lty == CType::Str || rty == CType::Str {
            if matches!(op, Add) {
                let l = to_cstr(&lv, lty);
                let r = to_cstr(&rv, rty);
                return Ok((format!("ran_concat({}, {})", l, r), CType::Str));
            }
            return Err(codegen_err(self.file, 0, 0, "unsupported string operator"));
        }

        // Float / mixed arithmetic.
        if lty == CType::Float || rty == CType::Float {
            let l = format!("(double)({})", lv);
            let r = format!("(double)({})", rv);
            let code = match op {
                Add => format!("({} + {})", l, r),
                Sub => format!("({} - {})", l, r),
                Mul => format!("({} * {})", l, r),
                Div => format!("({} / {})", l, r),
                Mod => format!("fmod({}, {})", l, r),
                _ => return Err(codegen_err(self.file, 0, 0, "unsupported float operator")),
            };
            return Ok((code, CType::Float));
        }

        // Checked integer arithmetic.
        let helper = match op {
            Add => "ran_checked_add",
            Sub => "ran_checked_sub",
            Mul => "ran_checked_mul",
            Div => "ran_checked_div",
            Mod => "ran_checked_mod",
            _ => return Err(codegen_err(self.file, 0, 0, "unsupported arithmetic operator")),
        };
        Ok((format!("{}({}, {})", helper, lv, rv), CType::Int))
    }

    fn emit_call_args(
        &mut self,
        sig: &FnSig,
        args: &[Expression],
        depth: usize,
    ) -> Result<Vec<String>, Diagnostic> {
        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let pty = sig.params.get(i).map(|p| p.1).unwrap_or(CType::Int);
            if pty == CType::Value {
                parts.push(self.emit_value(a, depth)?);
            } else {
                parts.push(self.emit_scalar(a, depth)?.0);
            }
        }
        Ok(parts)
    }

    // -- Value emission: returns the name of an owned `RanValue` temporary. --
    fn emit_value(&mut self, expr: &Expression, depth: usize) -> Result<String, Diagnostic> {
        let ty = self.infer(expr)?;
        if ty != CType::Value {
            // Box a scalar into a RanValue temporary.
            let (code, sty) = self.emit_scalar(expr, depth)?;
            let t = self.fresh_t();
            let boxed = match sty {
                CType::Int => format!("ran_from_int({})", code),
                CType::Bool => format!("ran_from_bool({})", code),
                CType::Float => format!("ran_from_float({})", code),
                CType::Str => format!("ran_from_str({})", code),
                CType::Value => unreachable!(),
            };
            self.line(depth, &format!("RanValue {} = {};", t, boxed));
            return Ok(t);
        }

        match expr {
            Expression::Variable(name) => {
                let t = self.fresh_t();
                self.line(depth, &format!("RanValue {} = ran_clone({});", t, c_ident(name)));
                Ok(t)
            }
            Expression::Array(items) => {
                let arr = self.fresh_t();
                self.line(depth, &format!("RanValue {} = ran_array_new({});", arr, items.len()));
                for it in items {
                    let elem = self.emit_value(it, depth)?;
                    self.line(depth, &format!("ran_array_push({}, ran_clone({}));", arr, elem));
                }
                Ok(arr)
            }
            Expression::Index { object, index } => {
                let arrt = self.emit_value(object, depth)?;
                let (idxcode, idxty) = self.emit_scalar(index, depth)?;
                if idxty != CType::Int {
                    return Err(codegen_err(self.file, 0, 0, "array index must be an integer"));
                }
                let t = self.fresh_t();
                self.line(depth, &format!("RanValue {} = ran_index({}, {});", t, arrt, idxcode));
                Ok(t)
            }
            Expression::FieldAccess { object, field } => {
                let objt = self.emit_value(object, depth)?;
                let t = self.fresh_t();
                self.line(
                    depth,
                    &format!("RanValue {} = ran_field({}, {});", t, objt, c_string_literal(field)),
                );
                Ok(t)
            }
            Expression::StructInit { name, fields } => {
                let decl = self
                    .structs
                    .get(name)
                    .ok_or_else(|| codegen_err(self.file, 0, 0, &format!("unknown struct `{}`", name)))?
                    .clone();
                let o = self.fresh_t();
                self.line(
                    depth,
                    &format!(
                        "RanValue {} = ran_object_new({}, {}, {});",
                        o,
                        c_string_literal(name),
                        decl.len(),
                        struct_fields_sym(name)
                    ),
                );
                for (fname, fexpr) in fields {
                    if let Some(idx) = decl.iter().position(|n| n == fname) {
                        let val = self.emit_value(fexpr, depth)?;
                        self.line(depth, &format!("ran_object_set({}, {}, ran_clone({}));", o, idx, val));
                    }
                }
                Ok(o)
            }
            Expression::BinaryOp { left, op, right } => {
                use BinaryOperator::*;
                let helper = match op {
                    Add => "ran_add",
                    Sub => "ran_sub",
                    Mul => "ran_mul",
                    Div => "ran_div",
                    Mod => "ran_mod",
                    _ => return Err(codegen_err(self.file, 0, 0, "unsupported value operator")),
                };
                let lv = self.emit_value(left, depth)?;
                let rv = self.emit_value(right, depth)?;
                let t = self.fresh_t();
                self.line(depth, &format!("RanValue {} = {}({}, {});", t, helper, lv, rv));
                Ok(t)
            }
            Expression::FnCall { callee, args } => {
                let name = match callee.as_ref() {
                    Expression::Variable(n) => n.clone(),
                    _ => return Err(codegen_err(self.file, 0, 0, "indirect call")),
                };
                if name == "dec" || name == "decimal" {
                    return self.emit_dec_call(args, depth);
                }
                let sig = self
                    .sigs
                    .get(&name)
                    .ok_or_else(|| codegen_err(self.file, 0, 0, &format!("unknown function `{}`", name)))?
                    .clone();
                let parts = self.emit_call_args(&sig, args, depth)?;
                let t = self.fresh_t();
                self.line(depth, &format!("RanValue {} = {}({});", t, c_ident(&name), parts.join(", ")));
                Ok(t)
            }
            _ => Err(codegen_err(self.file, 0, 0, "expression not in native subset")),
        }
    }

    fn emit_dec_call(&mut self, args: &[Expression], depth: usize) -> Result<String, Diagnostic> {
        let t = self.fresh_t();
        let init = match args.first() {
            None => "ran_dec_make(\"0\", 0)".to_string(),
            Some(Expression::StringLiteral(s)) => {
                // Parse with the authoritative Rust decimal so mantissa/scale
                // match the interpreter exactly, then reconstruct in C.
                match Decimal::parse(s) {
                    Ok(d) => {
                        let (mant, scale) = decimal_parts(&d);
                        format!("ran_dec_make({}, {})", c_string_literal(&mant), scale)
                    }
                    Err(_) => {
                        // Let the runtime raise the same E1004 the interpreter would.
                        format!("ran_dec_parse({})", c_string_literal(s))
                    }
                }
            }
            Some(arg) => {
                let aty = self.infer(arg)?;
                match aty {
                    CType::Int => {
                        let (code, _) = self.emit_scalar(arg, depth)?;
                        format!("ran_dec_from_int({})", code)
                    }
                    CType::Float => {
                        let (code, _) = self.emit_scalar(arg, depth)?;
                        format!("ran_dec_parse(ran_float_to_str({}))", code)
                    }
                    CType::Str => {
                        let (code, _) = self.emit_scalar(arg, depth)?;
                        format!("ran_dec_parse({})", code)
                    }
                    CType::Value => {
                        // Already a decimal value: clone it.
                        let v = self.emit_value(arg, depth)?;
                        format!("ran_clone({})", v)
                    }
                    CType::Bool => return Err(codegen_err(self.file, 0, 0, "cannot make a decimal from bool")),
                }
            }
        };
        self.line(depth, &format!("RanValue {} = {};", t, init));
        Ok(t)
    }

    // -- Condition emission: returns a C bool expression (may create temps). --
    fn emit_condition(&mut self, expr: &Expression, depth: usize) -> Result<String, Diagnostic> {
        let ty = self.infer(expr)?;
        match ty {
            CType::Bool => Ok(self.emit_scalar(expr, depth)?.0),
            CType::Int | CType::Float => Ok(self.emit_scalar(expr, depth)?.0),
            CType::Str => {
                let (code, _) = self.emit_scalar(expr, depth)?;
                Ok(format!("(strlen({}) != 0)", code))
            }
            CType::Value => {
                let t = self.emit_value(expr, depth)?;
                Ok(format!("ran_truthy({})", t))
            }
        }
    }
}

impl<'a> Emit<'a> {
    fn emit_block(&mut self, body: &[Stmt], depth: usize) -> Result<(), Diagnostic> {
        for stmt in body {
            self.emit_stmt(stmt, depth)?;
        }
        Ok(())
    }

    fn emit_stmt(&mut self, stmt: &Stmt, depth: usize) -> Result<(), Diagnostic> {
        match &stmt.kind {
            Statement::VarDecl { name, value, .. } => {
                let start = self.tmp;
                let ty = self.infer(value)?;
                if ty == CType::Value {
                    let t = self.emit_value(value, depth)?;
                    match self.scopes.lookup(name) {
                        Some(CType::Value) => {
                            self.line(depth, &format!("ran_release({});", c_ident(name)));
                            self.line(depth, &format!("{} = ran_clone({});", c_ident(name), t));
                        }
                        _ => {
                            self.line(depth, &format!("RanValue {} = ran_clone({});", c_ident(name), t));
                            self.scopes.declare_local(name, CType::Value);
                        }
                    }
                } else {
                    let (code, sty) = self.emit_scalar(value, depth)?;
                    match self.scopes.lookup(name) {
                        Some(existing) if existing == sty => {
                            self.line(depth, &format!("{} = {};", c_ident(name), code));
                        }
                        _ => {
                            self.line(depth, &format!("{} {} = {};", sty.c_decl(), c_ident(name), code));
                            self.scopes.declare_local(name, sty);
                        }
                    }
                }
                self.release_range(depth, start, self.tmp);
                Ok(())
            }
            Statement::Echo { expr, escapes } => {
                let start = self.tmp;
                let built = if let Expression::StringLiteral(s) = expr {
                    build_interpolated(s, &self.scopes)
                } else {
                    let ty = self.infer(expr)?;
                    if ty == CType::Value {
                        let t = self.emit_value(expr, depth)?;
                        format!("ran_value_to_str({})", t)
                    } else {
                        let (code, sty) = self.emit_scalar(expr, depth)?;
                        to_cstr(&code, sty)
                    }
                };
                if *escapes {
                    self.line(depth, &format!("ran_echo(ran_apply_escapes({}));", built));
                } else {
                    self.line(depth, &format!("ran_echo({});", built));
                }
                self.release_range(depth, start, self.tmp);
                Ok(())
            }
            Statement::Return(Some(e)) => {
                let start = self.tmp;
                if self.ret_ty == Some(CType::Value) {
                    let t = self.emit_value(e, depth)?;
                    let temps_end = self.tmp;
                    let r = self.fresh_s();
                    self.line(depth, &format!("RanValue {} = ran_clone({});", r, t));
                    self.release_range(depth, start, temps_end);
                    self.emit_return_cleanup(depth);
                    self.line(depth, &format!("return {};", r));
                } else {
                    let (code, sty) = self.emit_scalar(e, depth)?;
                    let temps_end = self.tmp;
                    let r = self.fresh_s();
                    self.line(depth, &format!("{} {} = {};", sty.c_decl(), r, code));
                    self.release_range(depth, start, temps_end);
                    self.emit_return_cleanup(depth);
                    self.line(depth, &format!("return {};", r));
                }
                Ok(())
            }
            Statement::Return(None) => {
                self.emit_return_cleanup(depth);
                if self.is_main {
                    self.line(depth, "return 0;");
                } else {
                    self.line(depth, "return;");
                }
                Ok(())
            }
            Statement::Break => {
                self.emit_loop_cleanup(depth);
                self.line(depth, "break;");
                Ok(())
            }
            Statement::Continue => {
                self.emit_loop_cleanup(depth);
                self.line(depth, "continue;");
                Ok(())
            }
            Statement::Expr(Expression::Match { subject, arms }) => {
                self.emit_match_stmt(subject, arms, depth)
            }
            Statement::Expr(e) => {
                let start = self.tmp;
                let ty = self.infer(e)?;
                if ty == CType::Value {
                    let t = self.emit_value(e, depth)?;
                    self.line(depth, &format!("(void){};", t));
                } else {
                    let (code, _) = self.emit_scalar(e, depth)?;
                    self.line(depth, &format!("(void)({});", code));
                }
                self.release_range(depth, start, self.tmp);
                Ok(())
            }
            Statement::If { condition, then_body, else_body } => {
                let start = self.tmp;
                let cond = self.emit_condition(condition, depth)?;
                let cend = self.tmp;
                let cv = self.fresh_s();
                self.line(depth, &format!("bool {} = {};", cv, cond));
                self.release_range(depth, start, cend);
                self.line(depth, &format!("if ({}) {{", cv));
                self.scopes.push();
                self.emit_block(then_body, depth + 1)?;
                self.scope_pop(depth + 1);
                if let Some(eb) = else_body {
                    self.line(depth, "} else {");
                    self.scopes.push();
                    self.emit_block(eb, depth + 1)?;
                    self.scope_pop(depth + 1);
                }
                self.line(depth, "}");
                Ok(())
            }
            Statement::While { condition, body } => {
                self.line(depth, "while (1) {");
                let start = self.tmp;
                let cond = self.emit_condition(condition, depth + 1)?;
                let cend = self.tmp;
                let cv = self.fresh_s();
                self.line(depth + 1, &format!("bool {} = {};", cv, cond));
                self.release_range(depth + 1, start, cend);
                self.line(depth + 1, &format!("if (!{}) break;", cv));
                self.scopes.push();
                self.loop_marks.push(self.scopes.stack.len() - 1);
                self.emit_block(body, depth + 1)?;
                self.release_top_scope(depth + 1);
                self.loop_marks.pop();
                self.scopes.pop();
                self.line(depth, "}");
                Ok(())
            }
            Statement::For { variable, iterable, body } => {
                let (start_expr, end_expr): (Option<&Expression>, &Expression) = match iterable {
                    Expression::FnCall { args, .. } if args.len() == 1 => (None, &args[0]),
                    Expression::FnCall { args, .. } if args.len() == 2 => (Some(&args[0]), &args[1]),
                    _ => return Err(codegen_err(self.file, 0, 0, "for iterable is not range(...)")),
                };
                let bstart = self.tmp;
                let startcode = match start_expr {
                    Some(e) => self.emit_scalar(e, depth)?.0,
                    None => "INT64_C(0)".to_string(),
                };
                let endcode = self.emit_scalar(end_expr, depth)?.0;
                let bend = self.tmp;
                let sa = self.fresh_s();
                let sb = self.fresh_s();
                self.line(depth, &format!("int64_t {} = {};", sa, startcode));
                self.line(depth, &format!("int64_t {} = {};", sb, endcode));
                self.release_range(depth, bstart, bend);
                let cv = c_ident(variable);
                self.line(
                    depth,
                    &format!("for (int64_t {} = {}; {} < {}; {}++) {{", cv, sa, cv, sb, cv),
                );
                self.scopes.push();
                self.scopes.declare(variable, CType::Int);
                self.loop_marks.push(self.scopes.stack.len() - 1);
                self.emit_block(body, depth + 1)?;
                self.release_top_scope(depth + 1);
                self.loop_marks.pop();
                self.scopes.pop();
                self.line(depth, "}");
                Ok(())
            }
            other => Err(codegen_err(self.file, 0, 0, &format!("unsupported statement: {}", stmt_kind(other)))),
        }
    }

    fn emit_match_stmt(
        &mut self,
        subject: &Expression,
        arms: &[MatchArm],
        depth: usize,
    ) -> Result<(), Diagnostic> {
        let start = self.tmp;
        let sty = self.infer(subject)?;
        self.scopes.push();
        let subj = format!("_m{}", self.svar);
        self.svar += 1;
        if sty == CType::Value {
            let t = self.emit_value(subject, depth)?;
            self.line(depth, &format!("RanValue {} = ran_clone({});", subj, t));
            self.scopes.val_locals.last_mut().unwrap().push(subj.clone());
        } else {
            let (code, _) = self.emit_scalar(subject, depth)?;
            self.line(depth, &format!("{} {} = {};", sty.c_decl(), subj, code));
        }
        self.release_range(depth, start, self.tmp);
        self.emit_arms(arms, &subj, sty, depth)?;
        self.scope_pop(depth);
        Ok(())
    }

    fn emit_arms(
        &mut self,
        arms: &[MatchArm],
        subj: &str,
        sty: CType,
        depth: usize,
    ) -> Result<(), Diagnostic> {
        if arms.is_empty() {
            return Ok(());
        }
        let arm = &arms[0];
        match &arm.pattern {
            Pattern::Wildcard | Pattern::Variable(_) => {
                self.line(depth, "{");
                self.scopes.push();
                if let Pattern::Variable(bind) = &arm.pattern {
                    if sty == CType::Value {
                        self.line(depth + 1, &format!("RanValue {} = ran_clone({});", c_ident(bind), subj));
                        self.scopes.declare_local(bind, CType::Value);
                    } else {
                        self.line(depth + 1, &format!("{} {} = {};", sty.c_decl(), c_ident(bind), subj));
                        self.scopes.declare(bind, sty);
                    }
                }
                self.emit_block(&arm.body, depth + 1)?;
                self.scope_pop(depth + 1);
                self.line(depth, "}");
                Ok(())
            }
            Pattern::Literal(lit) => {
                let tstart = self.tmp;
                let test = if sty == CType::Value {
                    let lv = self.emit_value(lit, depth)?;
                    format!("ran_eq({}, {})", subj, lv)
                } else {
                    let (lc, _lty) = self.emit_scalar(lit, depth)?;
                    match sty {
                        CType::Str => format!("(strcmp({}, {}) == 0)", subj, lc),
                        CType::Float => format!("((double)({}) == (double)({}))", subj, lc),
                        _ => format!("({} == {})", subj, lc),
                    }
                };
                let tend = self.tmp;
                let cv = self.fresh_s();
                self.line(depth, &format!("bool {} = {};", cv, test));
                self.release_range(depth, tstart, tend);
                self.line(depth, &format!("if ({}) {{", cv));
                self.scopes.push();
                self.emit_block(&arm.body, depth + 1)?;
                self.scope_pop(depth + 1);
                if arms.len() > 1 {
                    self.line(depth, "} else {");
                    self.emit_arms(&arms[1..], subj, sty, depth + 1)?;
                    self.line(depth, "}");
                } else {
                    self.line(depth, "}");
                }
                Ok(())
            }
        }
    }

    fn emit_fn(&mut self, name: &str, params: &[Param], body: &[Stmt]) -> Result<(), Diagnostic> {
        self.scopes = Scopes::new();
        self.loop_marks.clear();
        self.tmp = 0;
        self.svar = 0;
        self.is_main = name == "main";
        self.ret_ty = self.sigs.get(name).and_then(|s| s.ret);
        for p in params {
            self.scopes.declare_param(&p.name, param_ctype(p, self.structs));
        }
        if self.is_main {
            self.line(0, "int main(void) {");
        } else {
            let proto = fn_prototype(name, params, self.sigs, self.structs);
            self.line(0, &format!("{} {{", proto));
        }
        self.emit_block(body, 1)?;
        self.release_top_scope(1);
        if self.is_main {
            self.line(1, "return 0;");
        }
        self.line(0, "}");
        Ok(())
    }
}

fn fn_prototype(name: &str, params: &[Param], sigs: &HashMap<String, FnSig>, structs: &StructDefs) -> String {
    let sig = sigs.get(name);
    let ret = sig.and_then(|s| s.ret);
    let ret_str = match ret {
        Some(t) => t.c_decl(),
        None => "void",
    };
    let mut s = format!("{} {}(", ret_str, c_ident(name));
    if params.is_empty() {
        s.push_str("void");
    } else {
        let parts: Vec<String> = params
            .iter()
            .map(|p| format!("{} {}", param_ctype(p, structs).c_decl(), c_ident(&p.name)))
            .collect();
        s.push_str(&parts.join(", "));
    }
    s.push(')');
    s
}

/// Decompose a `Decimal` into a signed integer-mantissa string + scale, derived
/// from its exact textual form (the only lossless public surface).
fn decimal_parts(d: &Decimal) -> (String, u32) {
    let scale = d.scale();
    let s = d.to_string();
    let neg = s.starts_with('-');
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = if digits.is_empty() { "0".to_string() } else { digits };
    let mantissa = if neg { format!("-{}", digits) } else { digits };
    (mantissa, scale)
}

/// Lower a checked program (already verified by [`supported`]) to C source.
pub fn lower(checked: &CheckedProgram, file: &str) -> Result<String, Diagnostic> {
    let structs = collect_structs(checked);
    let sigs = resolve_signatures(checked, &structs)?;

    let mut out = String::new();
    out.push_str("/* Generated by Ran native AOT codegen (Phase D, D2). Do not edit. */\n");
    out.push_str("#include \"ran_rt.h\"\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stdbool.h>\n");
    out.push_str("#include <string.h>\n\n");

    // Static field-name tables for struct/object display + field access.
    for stmt in &checked.program.statements {
        if let Statement::StructDecl { name, fields, .. } = &stmt.kind {
            let names: Vec<String> = fields.iter().map(|f| c_string_literal(&f.name)).collect();
            let body = if names.is_empty() { "NULL".to_string() } else { names.join(", ") };
            out.push_str(&format!(
                "static const char *const {}[] = {{ {} }};\n",
                struct_fields_sym(name),
                body
            ));
        }
    }
    out.push('\n');

    // Forward declarations.
    for stmt in &checked.program.statements {
        if let Statement::FnDecl { name, params, .. } = &stmt.kind {
            if name == "main" {
                continue;
            }
            out.push_str(&fn_prototype(name, params, &sigs, &structs));
            out.push_str(";\n");
        }
    }
    out.push('\n');

    // Definitions.
    let mut emitter = Emit::new(&sigs, &structs, file);
    for stmt in &checked.program.statements {
        if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
            emitter.emit_fn(name, params, body)?;
            emitter.out.push('\n');
        }
    }
    out.push_str(&emitter.out);

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::lexer;
    use crate::frontend::parser;
    use crate::semantics::analyzer::{self, OwnershipMode};

    fn check(src: &str) -> CheckedProgram {
        let tokens = lexer::tokenize(src);
        let (program, _diags) = parser::parse_checked(tokens);
        analyzer::analyze_with_file(&program, "test.ran", src, OwnershipMode::Warn)
    }

    // ---- D1 regression: the monomorphic core still lowers as before. -------

    #[test]
    fn supported_accepts_core_subset() {
        let src = r#"
fn add(a: int, b: int) -> int {
    return a + b
}
fn main() {
    let mut total = 0
    for i in range(5) {
        total = total + i
    }
    if total > 3 {
        echo "big: $total"
    } else {
        echo "small"
    }
    echo add(2, 3)
}
"#;
        let checked = check(src);
        assert!(supported(&checked, "test.ran").is_ok());
        let c = lower(&checked, "test.ran").unwrap();
        assert!(c.contains("int main(void)"));
        assert!(c.contains("ran_checked_add("));
    }

    // ---- D2 accept: new constructs are inside the subset. ------------------

    #[test]
    fn supported_accepts_structs_arrays_match_float_decimal() {
        let src = r#"
struct Point { x: int, y: int }
fn main() {
    let p = Point { x: 3, y: 4 }
    echo p.x
    let xs = [10, 20, 30]
    echo xs[1]
    echo len(xs)
    let f = 3.5 + 1.25
    echo f
    let price = dec("19.99")
    let total = price * 3
    echo total
    let n = 2
    match n {
        1 => { echo "one" }
        2 => { echo "two" }
        _ => { echo "many" }
    }
}
"#;
        let checked = check(src);
        assert!(supported(&checked, "test.ran").is_ok(), "D2 subset should be accepted");
        let c = lower(&checked, "test.ran").unwrap();
        assert!(c.contains("ran_object_new("), "struct construction:\n{c}");
        assert!(c.contains("ran_field("), "field access:\n{c}");
        assert!(c.contains("ran_array_new("), "array literal:\n{c}");
        assert!(c.contains("ran_index("), "indexing:\n{c}");
        assert!(c.contains("ran_len("), "len:\n{c}");
        assert!(c.contains("ran_dec_make("), "decimal literal:\n{c}");
        assert!(c.contains("ran_mul("), "decimal arithmetic:\n{c}");
    }

    #[test]
    fn lower_struct_field_table_in_declaration_order() {
        let src = r#"
struct Money { cents: int, currency: str }
fn main() {
    let m = Money { currency: "USD", cents: 100 }
    echo m.cents
}
"#;
        let checked = check(src);
        supported(&checked, "test.ran").unwrap();
        let c = lower(&checked, "test.ran").unwrap();
        assert!(c.contains("_ranf_Money[] = { \"cents\", \"currency\" }"), "field table:\n{c}");
    }

    #[test]
    fn lower_refcount_release_for_value_locals() {
        let src = r#"
fn main() {
    let xs = [1, 2, 3]
    echo xs[0]
}
"#;
        let checked = check(src);
        supported(&checked, "test.ran").unwrap();
        let c = lower(&checked, "test.ran").unwrap();
        // Array local must be released; per-statement temporaries too.
        assert!(c.contains("ran_release(r_xs)"), "value local release:\n{c}");
    }

    // ---- D2 reject: still-out-of-subset constructs are hard E0606. ---------

    #[test]
    fn supported_rejects_maps() {
        let src = r#"
fn main() {
    let m = map()
    echo "x"
}
"#;
        let checked = check(src);
        let err = supported(&checked, "test.ran").expect_err("map builtin must be rejected");
        assert_eq!(err.code.as_deref(), Some("E0606"));
    }

    #[test]
    fn supported_rejects_closures() {
        let src = r#"
fn main() {
    let f = fn(x) { return x + 1 }
    echo "x"
}
"#;
        let checked = check(src);
        let err = supported(&checked, "test.ran").expect_err("closure must be rejected");
        assert_eq!(err.code.as_deref(), Some("E0606"));
    }

    #[test]
    fn supported_rejects_imports() {
        let src = r#"
import "std::fs" as fs
fn main() {
    echo "hi"
}
"#;
        let checked = check(src);
        let err = supported(&checked, "test.ran").expect_err("import must be rejected");
        assert_eq!(err.code.as_deref(), Some("E0606"));
    }

    #[test]
    fn supported_rejects_method_calls() {
        let src = r#"
fn main() {
    let s = "hello"
    echo s.to_upper()
}
"#;
        let checked = check(src);
        let err = supported(&checked, "test.ran").expect_err("method call must be rejected");
        assert_eq!(err.code.as_deref(), Some("E0606"));
    }

    #[test]
    fn lower_emits_recursive_function() {
        let src = r#"
fn fib(n: int) -> int {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}
fn main() {
    echo fib(10)
}
"#;
        let checked = check(src);
        supported(&checked, "test.ran").unwrap();
        let c = lower(&checked, "test.ran").unwrap();
        assert!(c.contains("int64_t r_fib(int64_t r_n);"), "missing fib prototype:\n{c}");
        assert!(c.contains("ran_checked_add("), "recursion sum should use checked add:\n{c}");
    }

    #[test]
    fn lower_emits_float_arithmetic() {
        let src = r#"
fn main() {
    let a = 2.5
    let b = 4.0
    echo a * b
}
"#;
        let checked = check(src);
        supported(&checked, "test.ran").unwrap();
        let c = lower(&checked, "test.ran").unwrap();
        assert!(c.contains("double r_a"), "float decl:\n{c}");
        assert!(c.contains("ran_float_to_str("), "float echo:\n{c}");
    }
}
