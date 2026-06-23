//! Compiler module - Semantic analysis, type checking, and validation.
//! Strict mode: undefined variables, undefined functions, type mismatches -> hard errors.
//! No program with errors should ever reach the runtime.

use crate::frontend::ast::{Expression, Param, Program, Span, Statement, Stmt, TypeExpr};
use crate::support::diagnostics::{Diagnostic, DiagnosticEngine, Severity, SourceLoc};
use crate::semantics::types::{OwnershipFinding, TypeChecker};

use std::collections::HashSet;
use std::process;

/// Ownership/borrow enforcement mode (migration rollout — see design "Migrasi").
///
/// * `Warn`   — compatibility mode (default for this release): ownership/borrow
///   diagnostics (`E0210`/`E0212`/`E0214`/`E0215`/`E0613`) are downgraded to
///   warnings and the program keeps running.
/// * `Strict` — opt-in: those diagnostics are hard errors that abort before
///   runtime/codegen.
///
/// NOTE (task 6.1): this enum is the plumbing that carries the selected mode
/// from the CLI/env down to the analyzer. The actual downgrade-to-warning and
/// abort behavior is implemented by later tasks (6.2 builds the model, 7.x
/// emits the diagnostics). For now the mode is parsed, threaded, and stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipMode {
    Warn,
    Strict,
}

impl OwnershipMode {
    /// Default mode for this release (compatibility-first rollout).
    pub const DEFAULT: OwnershipMode = OwnershipMode::Warn;

    /// Parse a mode string (`warn`/`strict`). Surrounding whitespace is ignored.
    pub fn parse(value: &str) -> Result<OwnershipMode, String> {
        match value.trim() {
            "warn" => Ok(OwnershipMode::Warn),
            "strict" => Ok(OwnershipMode::Strict),
            other => Err(format!(
                "invalid ownership mode '{}' (expected `warn` or `strict`)",
                other
            )),
        }
    }

    /// Canonical string form of the mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            OwnershipMode::Warn => "warn",
            OwnershipMode::Strict => "strict",
        }
    }
}

impl Default for OwnershipMode {
    fn default() -> Self {
        OwnershipMode::DEFAULT
    }
}

/// A checked program ready for code generation or interpretation
pub struct CheckedProgram {
    pub program: Program,
    pub has_main: bool,
    /// Ownership enforcement mode selected for this compilation/run.
    /// Carried here so the runtime and migration tooling (tasks 7.x/8.x) can
    /// observe the active mode without re-reading CLI/env state.
    pub ownership_mode: OwnershipMode,
}

/// Built-in functions that are always available
fn builtin_functions() -> HashSet<String> {
    [
        "echo", "print", "println", "len", "typeof", "str", "int", "float",
        "push", "map", "set", "get", "exit", "range", "keys", "values",
        "abs", "assert", "bool", "dec",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Built-in modules
fn builtin_modules() -> HashSet<String> {
    [
        "http", "time", "fs", "json", "os", "math", "html",
        "str", "rand", "log", "decimal", "env", "crypto",
        "concurrency", "web", "db",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Analyze a parsed program - type checking, ownership, undefined detection.
/// Strict: any error causes the compiler to abort.
pub fn analyze(program: &Program) -> CheckedProgram {
    analyze_with_file(program, "<stdin>", "", OwnershipMode::DEFAULT)
}

/// Analyze with file context for better error reporting.
///
/// `ownership_mode` selects the ownership/borrow rollout mode (see
/// [`OwnershipMode`]). It is stored on the returned [`CheckedProgram`] so later
/// phases can downgrade ownership diagnostics to warnings (`warn`) or abort
/// (`strict`). Enforcement itself lands in tasks 7.x.
pub fn analyze_with_file(
    program: &Program,
    filename: &str,
    source: &str,
    ownership_mode: OwnershipMode,
) -> CheckedProgram {
    let mut diag = DiagnosticEngine::new(filename, source);
    let filename_owned = filename.to_string();
    let mut checker = TypeChecker::new();

    // Thread the ownership rollout mode into the checker: in `warn` mode the
    // move/borrow findings are downgraded to warnings so the program keeps
    // running (R9.x migration); in `strict` mode they stay errors and abort
    // before runtime/codegen.
    checker.set_downgrade_to_warning(ownership_mode == OwnershipMode::Warn);

    // Ownership/type checking
    let _valid = checker.check(program);
    for err in &checker.errors {
        diag.report(Diagnostic::error(err.clone()));
    }
    for warn in &checker.warnings {
        diag.report(Diagnostic::warning(warn.clone()));
    }

    // Render coded ownership findings (use-after-move E0210, etc.) so they
    // carry `error[E0210]` and `file:line:col`. In `warn` mode the severity is
    // downgraded to a warning; in `strict` mode it stays an error and makes the
    // analysis abort before runtime/codegen (see "Pemetaan diagnostik ke alur
    // abort" — R9.3).
    let downgrade = checker.downgrade_ownership_to_warning;
    for finding in &checker.ownership_findings {
        // Use a label phrased for the specific ownership/borrow violation so
        // the rendered diagnostic reads correctly for moves *and* borrows.
        let label_text = match finding.code {
            "E0210" => "value used here after move",
            "E0212" => "conflicting borrow occurs here",
            "E0214" => "reference to local value returned here",
            "E0215" => "value moved here while still borrowed",
            "E0613" => "unsynchronized shared write occurs here",
            _ => "ownership violation occurs here",
        };
        let mut d = Diagnostic::from_code(finding.code, finding.message.clone())
            .with_loc(loc(&filename_owned, finding.span))
            .with_label(loc(&filename_owned, finding.span), label_text);
        if let Some(help) = &finding.help {
            d = d.with_help(help.clone());
        }
        if downgrade {
            d.severity = Severity::Warning;
        }
        diag.report(d);
    }

    // Collect all defined symbols (functions + variables)
    let mut defined_fns: HashSet<String> = builtin_functions();
    let mut defined_vars: HashSet<String> = HashSet::new();

    // Known stdlib module names (valid import targets)
    let known_stdlib = builtin_modules();
    // Usable module identifiers are derived from imports (alias mandatory for stdlib)
    let mut modules: HashSet<String> = HashSet::new();

    // Track function arities for argument count checking
    let mut fn_arity: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    // Track user-declared function names to detect duplicate definitions.
    let mut user_fns: HashSet<String> = HashSet::new();

    // First pass: collect all top-level declarations
    for stmt in &program.statements {
        match &stmt.kind {
            Statement::FnDecl { name, params, .. } => {
                // Duplicate definition: the same function name declared twice
                // (across the merged program) is an error — the second silently
                // shadowed the first at runtime before.
                if !user_fns.insert(name.clone()) {
                    diag.report(
                        Diagnostic::error(format!("duplicate function definition: `{}`", name))
                            .with_code("E0008")
                            .with_loc(loc(&filename_owned, stmt.span))
                            .with_label(loc(&filename_owned, stmt.span), "redefined here")
                            .with_help("each function name must be unique; rename or remove the duplicate definition"),
                    );
                }
                defined_fns.insert(name.clone());
                fn_arity.insert(name.clone(), params.len());
            }
            Statement::VarDecl { name, .. } => {
                defined_vars.insert(name.clone());
            }
            // Register user types so `Type.method(...)` (associated functions /
            // constructors) and struct literals are recognized as valid.
            Statement::StructDecl { name, .. } => {
                modules.insert(name.clone());
            }
            Statement::EnumDecl { name, .. } => {
                modules.insert(name.clone());
            }
            Statement::ImplBlock { type_name, .. } => {
                modules.insert(type_name.clone());
            }
            // Register trait names so trait references and method dispatch are
            // recognized as valid (R8.6); `impl Trait for Type` only registers
            // the concrete `type_name` above, never flagging the trait.
            Statement::TraitDecl { name, .. } => {
                modules.insert(name.clone());
            }
            Statement::Import { path, alias } => {
                if let Some(name) = path.strip_prefix("std::") {
                    // Canonical stdlib import: `import "std::http" as http`.
                    if !known_stdlib.contains(name) {
                        diag.report(
                            Diagnostic::error(format!("unknown standard library module `{}`", name))
                                .with_code("E0007")
                                .with_loc(loc(&filename_owned, stmt.span))
                                .with_label(loc(&filename_owned, stmt.span), "no such std module")
                                .with_help("available: std::http, std::time, std::fs, std::json, std::os, std::math, std::html, std::str, std::rand, std::log, std::decimal, std::env"),
                        );
                    } else {
                        match alias {
                            Some(a) => { modules.insert(a.clone()); }
                            None => {
                                diag.report(
                                    Diagnostic::error(format!("stdlib import `std::{}` requires an alias", name))
                                        .with_code("E0005")
                                        .with_loc(loc(&filename_owned, stmt.span))
                                        .with_label(loc(&filename_owned, stmt.span), "missing alias")
                                        .with_help(format!("write: import \"std::{}\" as {}", name, name)),
                                );
                            }
                        }
                    }
                } else if known_stdlib.contains(path) {
                    // Bare stdlib name without the required `std::` prefix.
                    diag.report(
                        Diagnostic::error(format!("standard library imports must use the `std::` prefix"))
                            .with_code("E0006")
                            .with_loc(loc(&filename_owned, stmt.span))
                            .with_label(loc(&filename_owned, stmt.span), "missing `std::` prefix")
                            .with_help(format!("write: import \"std::{}\" as {}", path, path)),
                    );
                }
                // Otherwise it's a local/relative import — handled by the loader.
            }
            _ => {}
        }
    }

    // Second pass: check for undefined references
    // Check top-level statements
    for stmt in &program.statements {
        let sp = stmt.span;
        match &stmt.kind {
            Statement::FnDecl { name: _, params, body, .. } => {
                let mut local_vars: HashSet<String> = defined_vars.clone();
                for p in params {
                    local_vars.insert(p.name.clone());
                }
                check_body(body, &defined_fns, &local_vars, &modules, &fn_arity, &filename_owned, &mut diag);
            }
            Statement::Expr(expr) => {
                check_expr(expr, &defined_fns, &defined_vars, &modules, &fn_arity, sp, &filename_owned, &mut diag);
            }
            Statement::Echo { expr, .. } => {
                check_expr(expr, &defined_fns, &defined_vars, &modules, &fn_arity, sp, &filename_owned, &mut diag);
            }
            Statement::VarDecl { value, type_annotation, name: _, .. } => {
                check_expr(value, &defined_fns, &defined_vars, &modules, &fn_arity, sp, &filename_owned, &mut diag);
                if let Some(ann) = type_annotation {
                    check_type_match(ann, value, sp, &filename_owned, &mut diag);
                }
            }
            // Top-level `impl`/`trait` blocks: analyze each method body like a
            // normal function body so genuine undefined references inside them
            // are caught without false-positively flagging valid methods
            // (R8.6/R8.7).
            Statement::ImplBlock { methods, .. } | Statement::TraitDecl { methods, .. } => {
                check_methods(methods, &defined_fns, &defined_vars, &modules, &fn_arity, &filename_owned, &mut diag);
            }
            _ => {}
        }
    }

    // Emit all diagnostics
    if diag.has_errors {
        diag.emit_all();
        process::exit(1);
    }

    // Warn-mode migration readiness summary (task 7.4). Only in `warn` mode and
    // only on a clean (no hard error) build — in `strict` mode the findings are
    // errors and we already aborted above, so strict behavior is untouched.
    //
    // The per-finding ownership warnings live in `diag` but the success path
    // never flushes them; emit them here so the summary that follows is backed
    // by visible warnings. Suppressed entirely when there is nothing to report
    // (no findings and no `&mut` sites) to avoid noise on clean programs.
    if ownership_mode == OwnershipMode::Warn {
        let counts = aggregate_ownership_counts(&checker.ownership_findings);
        let mut_sites = count_mut_ref_param_sites(program);
        let total_findings: usize = counts.iter().map(|(_, n)| *n).sum();
        if total_findings > 0 || mut_sites > 0 {
            diag.emit_all();
            eprintln!("{}", format_ownership_summary(&counts, mut_sites));
            if total_findings == 0 {
                eprintln!(
                    "  = note: no ownership findings — run with `--ownership=strict` to enforce."
                );
            } else {
                eprintln!(
                    "  = note: resolve the findings above, then run with `--ownership=strict` once every count is zero to enforce."
                );
            }
        }
    }

    // Check for main
    let has_main = program.statements.iter().any(|stmt| {
        matches!(&stmt.kind, Statement::FnDecl { name, .. } if name == "main")
    });

    CheckedProgram {
        program: program.clone(),
        has_main,
        ownership_mode,
    }
}

/// Diagnostic codes summarized in the warn-mode migration readiness output, in
/// their fixed reporting order (use-after-move, borrow conflict, dangling,
/// move-while-borrowed, unsynchronized shared write).
const OWNERSHIP_SUMMARY_CODES: [&str; 5] = ["E0210", "E0212", "E0214", "E0215", "E0613"];

/// Aggregate ownership findings by diagnostic code.
///
/// Pure function (no I/O) so it is unit-testable: given the checker's findings
/// it returns a `(code, count)` pair per summarized code, in the fixed order of
/// [`OWNERSHIP_SUMMARY_CODES`]. Codes outside that set are ignored (there are
/// none today, but this keeps the summary stable if new findings are added).
fn aggregate_ownership_counts(findings: &[OwnershipFinding]) -> [(&'static str, usize); 5] {
    let mut counts: [(&'static str, usize); 5] = [
        (OWNERSHIP_SUMMARY_CODES[0], 0),
        (OWNERSHIP_SUMMARY_CODES[1], 0),
        (OWNERSHIP_SUMMARY_CODES[2], 0),
        (OWNERSHIP_SUMMARY_CODES[3], 0),
        (OWNERSHIP_SUMMARY_CODES[4], 0),
    ];
    for finding in findings {
        for slot in counts.iter_mut() {
            if slot.0 == finding.code {
                slot.1 += 1;
                break;
            }
        }
    }
    counts
}

/// Format the one-line warn-mode ownership migration summary.
///
/// Pure function. Example output:
/// `ownership summary (warn mode): E0210=2 E0212=0 E0214=1 E0215=0 E0613=0; &mut sites=3`
fn format_ownership_summary(counts: &[(&'static str, usize)], mut_sites: usize) -> String {
    let mut line = String::from("ownership summary (warn mode):");
    for (code, n) in counts {
        line.push_str(&format!(" {}={}", code, n));
    }
    line.push_str(&format!("; &mut sites={}", mut_sites));
    line
}

/// Count `&mut` parameter sites across the whole program.
///
/// Migration-readiness metric: a function parameter whose declared type is a
/// mutable reference (`&mut T`) is a site whose mutation becomes observable to
/// the caller once `&mut` write-back lands (task 8.2). This is the tractable
/// metric chosen here (a static count of declared `&mut` parameters) over
/// scanning every call site. Counts top-level functions and methods declared
/// inside `impl` blocks (recursively, to include nested functions).
fn count_mut_ref_param_sites(program: &Program) -> usize {
    fn is_mut_ref_param(p: &Param) -> bool {
        matches!(
            &p.type_annotation,
            Some(TypeExpr::Ref { mutable: true, .. })
        )
    }
    fn count_in_stmts(stmts: &[Stmt]) -> usize {
        let mut n = 0;
        for s in stmts {
            match &s.kind {
                Statement::FnDecl { params, body, .. } => {
                    n += params.iter().filter(|p| is_mut_ref_param(p)).count();
                    n += count_in_stmts(body);
                }
                Statement::ImplBlock { methods, .. } => {
                    n += count_in_stmts(methods);
                }
                _ => {}
            }
        }
        n
    }
    count_in_stmts(&program.statements)
}

/// Recursively check a block of statements for undefined references
fn check_body(
    stmts: &[Stmt],
    fns: &HashSet<String>,
    parent_vars: &HashSet<String>,
    modules: &HashSet<String>,
    fn_arity: &std::collections::HashMap<String, usize>,
    file: &str,
    diag: &mut DiagnosticEngine,
) {
    let mut vars = parent_vars.clone();

    for stmt in stmts {
        let sp = stmt.span;
        match &stmt.kind {
            Statement::VarDecl { name, value, type_annotation, .. } => {
                check_expr(value, fns, &vars, modules, fn_arity, sp, file, diag);
                if let Some(ann) = type_annotation {
                    check_type_match(ann, value, sp, file, diag);
                }
                vars.insert(name.clone());
            }
            Statement::Echo { expr, .. } => {
                check_expr(expr, fns, &vars, modules, fn_arity, sp, file, diag);
            }
            Statement::Expr(expr) => {
                check_expr(expr, fns, &vars, modules, fn_arity, sp, file, diag);
            }
            Statement::If { condition, then_body, else_body } => {
                check_expr(condition, fns, &vars, modules, fn_arity, sp, file, diag);
                check_body(then_body, fns, &vars, modules, fn_arity, file, diag);
                if let Some(else_stmts) = else_body {
                    check_body(else_stmts, fns, &vars, modules, fn_arity, file, diag);
                }
            }
            Statement::For { variable, iterable, body } => {
                check_expr(iterable, fns, &vars, modules, fn_arity, sp, file, diag);
                let mut loop_vars = vars.clone();
                loop_vars.insert(variable.clone());
                check_body(body, fns, &loop_vars, modules, fn_arity, file, diag);
            }
            Statement::While { condition, body } => {
                check_expr(condition, fns, &vars, modules, fn_arity, sp, file, diag);
                check_body(body, fns, &vars, modules, fn_arity, file, diag);
            }
            Statement::Spawn { body } => {
                check_body(body, fns, &vars, modules, fn_arity, file, diag);
            }
            Statement::Return(Some(expr)) => {
                check_expr(expr, fns, &vars, modules, fn_arity, sp, file, diag);
            }
            Statement::FnDecl { name, params, body, .. } => {
                vars.insert(name.clone());
                let mut fn_vars = vars.clone();
                for p in params {
                    fn_vars.insert(p.name.clone());
                }
                check_body(body, fns, &fn_vars, modules, fn_arity, file, diag);
            }
            // Trait default-method and `impl` method bodies are analyzed like
            // normal function bodies: each method's params (including `self`)
            // are bound, so valid method bodies pass while genuine undefined
            // references inside them are still caught (R8.6/R8.7).
            Statement::ImplBlock { methods, .. } | Statement::TraitDecl { methods, .. } => {
                check_methods(methods, fns, &vars, modules, fn_arity, file, diag);
            }
            // Loop-control statements carry no references to resolve.
            Statement::Break | Statement::Continue => {}
            _ => {}
        }
    }
}

/// Analyze the method bodies declared inside an `impl`/`trait` block. Each
/// method is a `FnDecl`; its parameters (including `self`) are bound as locals
/// so a well-formed method body is accepted while genuine undefined references
/// are still reported (R8.6/R8.7).
fn check_methods(
    methods: &[Stmt],
    fns: &HashSet<String>,
    parent_vars: &HashSet<String>,
    modules: &HashSet<String>,
    fn_arity: &std::collections::HashMap<String, usize>,
    file: &str,
    diag: &mut DiagnosticEngine,
) {
    for m in methods {
        if let Statement::FnDecl { params, body, .. } = &m.kind {
            let mut method_vars = parent_vars.clone();
            for p in params {
                method_vars.insert(p.name.clone());
            }
            check_body(body, fns, &method_vars, modules, fn_arity, file, diag);
        }
    }
}

/// Returns true if a variable name is a runtime-injected web request variable.
/// The HTTP server injects these into route handlers, so the static checker
/// must treat them as always-defined.
fn is_request_var(name: &str) -> bool {
    name == "req_method"
        || name == "req_path"
        || name == "req_body"
        || name.starts_with("param_")
        || name.starts_with("query_")
        || name.starts_with("cookie_")
}

/// Build a SourceLoc from a span and filename.
fn loc(file: &str, sp: Span) -> SourceLoc {
    SourceLoc::new(file, sp.line, sp.col, sp.col)
}

/// Check an expression for undefined function calls and variables
fn check_expr(
    expr: &Expression,
    fns: &HashSet<String>,
    vars: &HashSet<String>,
    modules: &HashSet<String>,
    fn_arity: &std::collections::HashMap<String, usize>,
    sp: Span,
    file: &str,
    diag: &mut DiagnosticEngine,
) {
    match expr {
        Expression::Variable(name) => {
            if !vars.contains(name)
                && !fns.contains(name)
                && !modules.contains(name)
                && !is_request_var(name)
            {
                diag.report(
                    Diagnostic::error(format!("undefined variable: `{}`", name))
                        .with_code("E0001")
                        .with_loc(loc(file, sp))
                        .with_label(loc(file, sp), "not found in this scope")
                        .with_help(format!("declare it first: `let {} = ...` or `{}=\"...\"`", name, name)),
                );
            }
        }
        Expression::FnCall { callee, args } => {
            if let Expression::Variable(name) = callee.as_ref() {
                if !fns.contains(name) && !vars.contains(name) {
                    diag.report(
                        Diagnostic::error(format!("undefined function: `{}`", name))
                            .with_code("E0002")
                            .with_loc(loc(file, sp))
                            .with_label(loc(file, sp), "not defined")
                            .with_help("did you forget to define this function with `fn`?"),
                    );
                } else if let Some(&expected) = fn_arity.get(name) {
                    if args.len() != expected {
                        diag.report(
                            Diagnostic::error(format!(
                                "function `{}` expects {} argument{}, but {} {} provided",
                                name,
                                expected,
                                if expected == 1 { "" } else { "s" },
                                args.len(),
                                if args.len() == 1 { "was" } else { "were" },
                            ))
                            .with_code("E0003")
                            .with_loc(loc(file, sp))
                            .with_label(loc(file, sp), "wrong number of arguments")
                            .with_help(format!("call `{}` with exactly {} argument{}", name, expected, if expected == 1 { "" } else { "s" })),
                        );
                    }
                }
            } else {
                check_expr(callee, fns, vars, modules, fn_arity, sp, file, diag);
            }
            for arg in args {
                check_expr(arg, fns, vars, modules, fn_arity, sp, file, diag);
            }
        }
        Expression::MethodCall { object, method: _, args } => {
            if let Expression::Variable(name) = object.as_ref() {
                if !modules.contains(name) && !vars.contains(name) {
                    diag.report(
                        Diagnostic::error(format!("undefined variable or module: `{}`", name))
                            .with_code("E0001")
                            .with_loc(loc(file, sp))
                            .with_label(loc(file, sp), "not found in this scope"),
                    );
                }
            } else {
                check_expr(object, fns, vars, modules, fn_arity, sp, file, diag);
            }
            for arg in args {
                check_expr(arg, fns, vars, modules, fn_arity, sp, file, diag);
            }
        }
        Expression::BinaryOp { left, right, .. } => {
            check_expr(left, fns, vars, modules, fn_arity, sp, file, diag);
            check_expr(right, fns, vars, modules, fn_arity, sp, file, diag);
        }
        Expression::UnaryOp { operand, .. } => {
            check_expr(operand, fns, vars, modules, fn_arity, sp, file, diag);
        }
        Expression::Array(elements) => {
            for e in elements {
                check_expr(e, fns, vars, modules, fn_arity, sp, file, diag);
            }
        }
        Expression::FieldAccess { object, .. } => {
            check_expr(object, fns, vars, modules, fn_arity, sp, file, diag);
        }
        Expression::Index { object, index } => {
            check_expr(object, fns, vars, modules, fn_arity, sp, file, diag);
            check_expr(index, fns, vars, modules, fn_arity, sp, file, diag);
        }
        Expression::Pipe { left, right } => {
            check_expr(left, fns, vars, modules, fn_arity, sp, file, diag);
            check_expr(right, fns, vars, modules, fn_arity, sp, file, diag);
        }
        // Closure / lambda: check the body in a scope that sees the captured
        // outer variables (`vars`) plus the lambda's own parameters, so a
        // well-formed closure is accepted while genuine undefined references
        // inside the body are still caught (R8.7).
        Expression::Lambda { params, body } => {
            let mut closure_vars = vars.clone();
            for p in params {
                closure_vars.insert(p.name.clone());
            }
            check_body(body, fns, &closure_vars, modules, fn_arity, file, diag);
        }
        // Match: check the subject, then each arm body. A `Pattern::Variable`
        // binds a fresh name visible inside that arm.
        Expression::Match { subject, arms } => {
            check_expr(subject, fns, vars, modules, fn_arity, sp, file, diag);
            for arm in arms {
                let mut arm_vars = vars.clone();
                if let crate::frontend::ast::Pattern::Variable(n) = &arm.pattern {
                    arm_vars.insert(n.clone());
                }
                check_body(&arm.body, fns, &arm_vars, modules, fn_arity, file, diag);
            }
        }
        Expression::StructInit { fields, .. } => {
            for (_, e) in fields {
                check_expr(e, fns, vars, modules, fn_arity, sp, file, diag);
            }
        }
        Expression::Await(inner) => {
            check_expr(inner, fns, vars, modules, fn_arity, sp, file, diag);
        }
        Expression::ChanSend { channel, value } => {
            check_expr(channel, fns, vars, modules, fn_arity, sp, file, diag);
            check_expr(value, fns, vars, modules, fn_arity, sp, file, diag);
        }
        Expression::ChanRecv { channel } => {
            check_expr(channel, fns, vars, modules, fn_arity, sp, file, diag);
        }
        _ => {}
    }
}

/// Check that a value expression matches its type annotation.
fn check_type_match(annotation: &TypeExpr, value: &Expression, sp: Span, file: &str, diag: &mut DiagnosticEngine) {
    let ann_name = match annotation {
        TypeExpr::Named(n) => n.as_str(),
        _ => return,
    };

    let value_type = match value {
        Expression::IntLiteral(_) => Some("int"),
        Expression::FloatLiteral(_) => Some("float"),
        Expression::StringLiteral(_) => Some("str"),
        Expression::BoolLiteral(_) => Some("bool"),
        Expression::Array(_) => Some("array"),
        _ => None,
    };

    if let Some(vt) = value_type {
        let matches = match ann_name {
            "int" | "i64" | "i32" => vt == "int",
            "float" | "f64" | "f32" => vt == "float" || vt == "int",
            "str" | "string" | "String" => vt == "str",
            "bool" => vt == "bool",
            _ => true,
        };

        if !matches {
            diag.report(
                Diagnostic::error(format!(
                    "type mismatch: expected `{}`, found `{}`",
                    ann_name, vt
                ))
                .with_code("E0004")
                .with_loc(loc(file, sp))
                .with_label(loc(file, sp), format!("this is a `{}`", vt))
                .with_help(format!("change the type annotation to `{}` or fix the value", vt)),
            );
        }
    }
}

#[cfg(test)]
mod ownership_mode_tests {
    use super::*;
    use crate::frontend::ast::{Expression, Program, Span, Statement, Stmt};

    fn stmt(kind: Statement) -> Stmt {
        Stmt::new(kind, Span::new(1, 1))
    }

    fn var_decl(name: &str, value: Expression) -> Stmt {
        stmt(Statement::VarDecl {
            name: name.into(),
            mutable: false,
            is_decl: true,
            type_annotation: None,
            value,
        })
    }

    /// In `warn` mode a use-after-move is downgraded to a warning, so the
    /// analysis must NOT abort: `analyze_with_file` returns a `CheckedProgram`
    /// (no `process::exit`). This is the migration-compatibility guarantee
    /// (R9.2/R9.3 rollout). Strict-mode abort is exercised by the integration
    /// smoke test (running the binary with `--ownership=strict`), since the
    /// abort path calls `process::exit` and cannot return into a unit test.
    #[test]
    fn warn_mode_does_not_abort_on_use_after_move() {
        // let s = "hi"; let t = s; echo s   (s is str -> non-Copy, moved into t)
        let program = Program {
            statements: vec![
                var_decl("s", Expression::StringLiteral("hi".into())),
                var_decl("t", Expression::Variable("s".into())),
                stmt(Statement::Echo {
                    expr: Expression::Variable("s".into()),
                    escapes: false,
                }),
            ],
        };

        let checked = analyze_with_file(&program, "test.ran", "", OwnershipMode::Warn);
        assert_eq!(checked.ownership_mode, OwnershipMode::Warn);
        assert!(!checked.has_main);
    }

    fn finding(code: &'static str) -> OwnershipFinding {
        OwnershipFinding {
            code,
            span: Span::new(1, 1),
            message: format!("synthetic {} finding", code),
            help: None,
        }
    }

    /// The aggregation helper counts each summarized code and keeps the fixed
    /// reporting order regardless of the order findings arrive in.
    #[test]
    fn aggregate_counts_by_code_in_fixed_order() {
        let findings = vec![
            finding("E0210"),
            finding("E0214"),
            finding("E0210"),
            finding("E0613"),
        ];
        let counts = aggregate_ownership_counts(&findings);
        assert_eq!(
            counts,
            [
                ("E0210", 2),
                ("E0212", 0),
                ("E0214", 1),
                ("E0215", 0),
                ("E0613", 1),
            ]
        );
    }

    /// No findings → every count is zero, still in the canonical order.
    #[test]
    fn aggregate_counts_empty_is_all_zero() {
        let counts = aggregate_ownership_counts(&[]);
        assert_eq!(
            counts,
            [
                ("E0210", 0),
                ("E0212", 0),
                ("E0214", 0),
                ("E0215", 0),
                ("E0613", 0),
            ]
        );
    }

    /// The formatted line matches the documented shape exactly.
    #[test]
    fn format_summary_matches_documented_shape() {
        let counts = [
            ("E0210", 2),
            ("E0212", 0),
            ("E0214", 1),
            ("E0215", 0),
            ("E0613", 0),
        ];
        let line = format_ownership_summary(&counts, 3);
        assert_eq!(
            line,
            "ownership summary (warn mode): E0210=2 E0212=0 E0214=1 E0215=0 E0613=0; &mut sites=3"
        );
    }

    /// `&mut` parameter sites are counted across top-level functions and
    /// methods inside impl blocks; plain and `&` (shared) params are ignored.
    #[test]
    fn counts_mut_ref_param_sites() {
        use crate::frontend::ast::{Param, TypeExpr};

        let mut_ref_param = |name: &str| Param {
            name: name.into(),
            type_annotation: Some(TypeExpr::Ref {
                mutable: true,
                inner: Box::new(TypeExpr::Named("int".into())),
            }),
            is_mut: false,
        };
        let shared_ref_param = |name: &str| Param {
            name: name.into(),
            type_annotation: Some(TypeExpr::Ref {
                mutable: false,
                inner: Box::new(TypeExpr::Named("int".into())),
            }),
            is_mut: false,
        };
        let plain_param = |name: &str| Param {
            name: name.into(),
            type_annotation: Some(TypeExpr::Named("int".into())),
            is_mut: false,
        };

        let top_fn = stmt(Statement::FnDecl {
            name: "f".into(),
            params: vec![mut_ref_param("a"), shared_ref_param("b"), plain_param("c")],
            return_type: None,
            body: vec![],
            is_pub: false,
            is_async: false,
        });
        let method = stmt(Statement::FnDecl {
            name: "m".into(),
            params: vec![mut_ref_param("self"), mut_ref_param("other")],
            return_type: None,
            body: vec![],
            is_pub: false,
            is_async: false,
        });
        let impl_block = stmt(Statement::ImplBlock {
            type_name: "T".into(),
            trait_name: None,
            methods: vec![method],
        });

        let program = Program {
            statements: vec![top_fn, impl_block],
        };
        // 1 (top fn) + 2 (method) = 3 mutable-ref parameter sites.
        assert_eq!(count_mut_ref_param_sites(&program), 3);
    }
}
