//! Unused-binding and unused-import diagnostics (Rust-style, warnings).
//!
//! Strategy — soundness over precision. We collect the set of every name
//! *referenced* anywhere in the program (variable reads, method-call receivers,
//! field/index bases, and `$name` / `${name}` reads inside interpolated string
//! literals), then warn for:
//!   * `W0601` — an explicit `let` / `var` / `let mut` declaration whose name is
//!     never referenced (`is_decl = true`; bare `x = …` assignments are skipped).
//!   * `W0602` — an `import "…" as alias` whose `alias` is never referenced.
//!
//! Because the use-set is program-wide, a name used *anywhere* is never flagged,
//! so there are no false positives (at worst we miss an unused binding when two
//! scopes reuse the same name — a safe false negative). Names starting with `_`
//! are intentionally ignored, matching the common "I know it's unused" convention.

use std::collections::HashSet;

use crate::frontend::ast::{Expression, Span, Statement, Stmt};
use crate::support::diagnostics::{Diagnostic, DiagnosticEngine, SourceLoc};

/// Run the unused-binding / unused-import lints over a program's top-level
/// statements, reporting warnings into `diag`.
pub fn check_unused(statements: &[Stmt], file: &str, diag: &mut DiagnosticEngine) {
    // 1) Every name referenced anywhere in the program.
    let mut used: HashSet<String> = HashSet::new();
    for s in statements {
        collect_stmt_uses(&s.kind, &mut used);
    }

    // 2) Unused imports.
    for s in statements {
        if let Statement::Import { path: _, alias } = &s.kind {
            if let Some(name) = alias.as_ref().filter(|a| !a.is_empty()) {
                if !name.starts_with('_') && !used.contains(name) {
                    warn(
                        diag,
                        file,
                        s.span,
                        "W0602",
                        format!("unused import: `{}`", name),
                        "unused import",
                        format!(
                            "remove this import, or use it (e.g. `{}.something(...)`); prefix the alias with `_` to silence",
                            name
                        ),
                    );
                }
            }
        }
    }

    // 3) Unused explicit `let` / `var` declarations (anywhere in the program).
    let mut decls: Vec<(String, Span)> = Vec::new();
    collect_decls_block(statements, &mut decls);
    for (name, span) in decls {
        if !name.starts_with('_') && !used.contains(&name) {
            warn(
                diag,
                file,
                span,
                "W0601",
                format!("unused variable: `{}`", name),
                "declared here but never read",
                format!(
                    "remove `{name}`, or read it somewhere; prefix it with `_` (e.g. `_{name}`) to silence",
                    name = name
                ),
            );
        }
    }
}

fn warn(
    diag: &mut DiagnosticEngine,
    file: &str,
    span: Span,
    code: &str,
    message: String,
    label: &str,
    help: String,
) {
    let l = SourceLoc::new(file, span.line, span.col, span.col);
    diag.report(
        Diagnostic::warning(message)
            .with_code(code)
            .with_loc(l.clone())
            .with_label(l, label)
            .with_help(help),
    );
}

/// Walk a list of statements, collecting explicit `let`/`var`/`let mut`
/// declarations (`is_decl = true`) with their real spans, recursing into all
/// nested blocks (functions, loops, ifs, impl/trait methods, spawn).
fn collect_decls_block(stmts: &[Stmt], out: &mut Vec<(String, Span)>) {
    for s in stmts {
        if let Statement::VarDecl { name, is_decl, .. } = &s.kind {
            if *is_decl {
                out.push((name.clone(), s.span));
            }
        }
        match &s.kind {
            Statement::FnDecl { body, .. }
            | Statement::For { body, .. }
            | Statement::While { body, .. }
            | Statement::Spawn { body } => collect_decls_block(body, out),
            Statement::If { then_body, else_body, .. } => {
                collect_decls_block(then_body, out);
                if let Some(eb) = else_body {
                    collect_decls_block(eb, out);
                }
            }
            Statement::ImplBlock { methods, .. } | Statement::TraitDecl { methods, .. } => {
                collect_decls_block(methods, out)
            }
            _ => {}
        }
    }
}

/// Collect every referenced name in a statement (and its nested blocks).
fn collect_stmt_uses(stmt: &Statement, used: &mut HashSet<String>) {
    match stmt {
        Statement::VarDecl { value, .. } => collect_expr_uses(value, used),
        Statement::Expr(e) => collect_expr_uses(e, used),
        Statement::Echo { expr, .. } => collect_expr_uses(expr, used),
        Statement::Return(Some(e)) => collect_expr_uses(e, used),
        Statement::Return(None) | Statement::Break | Statement::Continue => {}
        Statement::If { condition, then_body, else_body } => {
            collect_expr_uses(condition, used);
            collect_block_uses(then_body, used);
            if let Some(eb) = else_body {
                collect_block_uses(eb, used);
            }
        }
        Statement::For { iterable, body, variable: _ } => {
            collect_expr_uses(iterable, used);
            collect_block_uses(body, used);
        }
        Statement::While { condition, body } => {
            collect_expr_uses(condition, used);
            collect_block_uses(body, used);
        }
        Statement::Spawn { body } => collect_block_uses(body, used),
        Statement::FnDecl { body, .. } => collect_block_uses(body, used),
        Statement::ImplBlock { methods, .. } | Statement::TraitDecl { methods, .. } => {
            collect_block_uses(methods, used)
        }
        Statement::StructDecl { .. } | Statement::EnumDecl { .. } | Statement::Import { .. } => {}
    }
}

fn collect_block_uses(stmts: &[Stmt], used: &mut HashSet<String>) {
    for s in stmts {
        collect_stmt_uses(&s.kind, used);
    }
}

/// Collect every referenced name in an expression, including names read inside
/// interpolated string literals (`"$x"`, `"${x.y}"`).
fn collect_expr_uses(expr: &Expression, used: &mut HashSet<String>) {
    match expr {
        Expression::Variable(name) => {
            used.insert(name.clone());
        }
        Expression::StringLiteral(s) => collect_interpolation_names(s, used),
        Expression::BinaryOp { left, right, .. } => {
            collect_expr_uses(left, used);
            collect_expr_uses(right, used);
        }
        Expression::UnaryOp { operand, .. } => collect_expr_uses(operand, used),
        Expression::FnCall { callee, args } => {
            collect_expr_uses(callee, used);
            for a in args {
                collect_expr_uses(a, used);
            }
        }
        Expression::MethodCall { object, args, .. } => {
            collect_expr_uses(object, used);
            for a in args {
                collect_expr_uses(a, used);
            }
        }
        Expression::FieldAccess { object, .. } => collect_expr_uses(object, used),
        Expression::Index { object, index } => {
            collect_expr_uses(object, used);
            collect_expr_uses(index, used);
        }
        Expression::Pipe { left, right } => {
            collect_expr_uses(left, used);
            collect_expr_uses(right, used);
        }
        Expression::ChanSend { channel, value } => {
            collect_expr_uses(channel, used);
            collect_expr_uses(value, used);
        }
        Expression::ChanRecv { channel } => collect_expr_uses(channel, used),
        Expression::Lambda { body, .. } => collect_block_uses(body, used),
        Expression::StructInit { fields, .. } => {
            for (_, e) in fields {
                collect_expr_uses(e, used);
            }
        }
        Expression::Array(elems) => {
            for e in elems {
                collect_expr_uses(e, used);
            }
        }
        Expression::Await(inner) => collect_expr_uses(inner, used),
        Expression::Match { subject, arms } => {
            collect_expr_uses(subject, used);
            for arm in arms {
                collect_block_uses(&arm.body, used);
            }
        }
        Expression::IntLiteral(_)
        | Expression::FloatLiteral(_)
        | Expression::BoolLiteral(_) => {}
    }
}

/// Extract the base identifiers read by a `$name` / `${name.path}` interpolation
/// inside a string literal. Only the base name matters for use-tracking
/// (`$user.name` is a read of `user`). Mirrors the interpreter's interpolation
/// grammar closely enough to never under-count a real read.
fn collect_interpolation_names(s: &str, used: &mut HashSet<String>) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' && i + 1 < chars.len() {
            i += 1;
            let braced = chars[i] == '{';
            if braced {
                i += 1;
            }
            let mut name = String::new();
            // base identifier: alphanumeric/underscore
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                name.push(chars[i]);
                i += 1;
            }
            if !name.is_empty() {
                used.insert(name);
            }
            // Skip the rest of a dotted path / closing brace; those are fields,
            // not separate bindings.
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.' || chars[i] == '}')
            {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}
