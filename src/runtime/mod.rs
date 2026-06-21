//! Runtime module - Interprets checked AST for `ran run`.
//! Full standard library: I/O, HTTP server with routes, fs, json, templates.

use std::collections::HashMap;

use crate::frontend::ast::*;
use crate::semantics::analyzer::CheckedProgram;
use crate::support::decimal::{Decimal, Rounding};

pub use crate::frontend::ast::Program;

// Submodules carrying parts of the `Environment` inherent impl, split out of
// this file for maintainability (no behavior change). Child modules can access
// this module's private items via `use super::*`.
mod json;
mod module_dispatch;
mod server;
mod builtins;
mod frame;
mod helpers;

/// A lexical scope frame: variable name -> value, hashed with the fast,
/// std-only FNV-1a hasher. Variable names are short, trusted, in-process keys
/// (never attacker-controlled), so the default DoS-resistant SipHash is
/// unnecessary overhead on the interpreter's hottest path. Using FNV here cuts
/// the per-access hashing cost in tight loops.
type Scope = HashMap<String, Value, crate::support::fasthash::FnvBuildHasher>;

/// A local scope frame: a small, linear-scanned association list. Function and
/// block scopes hold only a handful of bindings, so a `Vec` with a linear find
/// beats a `HashMap` (no hashing, no per-binding allocation) on the hot path.
/// Globals stay in a `Scope` (`HashMap`) since there can be many of them.
type Frame = Vec<(String, Value)>;

/// Control-flow signal propagated out of statement execution.
///
/// This is how `return` (and, in the future, `break`/`continue`) escapes
/// nested blocks such as loop and `if` bodies. Without it, a `return` inside
/// a `for`/`while` loop would not actually return from the enclosing function.
#[derive(Debug, Clone)]
pub enum Flow {
    /// Continue executing subsequent statements normally.
    Normal,
    /// Unwind to the enclosing function, yielding this value.
    Return(Value),
    /// Stop the innermost loop.
    Break,
    /// Skip to the next iteration of the innermost loop.
    Continue,
}

/// A recoverable runtime fault (overflow, divide-by-zero, bad decimal, missing
/// required env var, ...). Raised via `runtime_error` and caught either at the
/// top level (`execute`) to print a clean diagnostic and exit, or by the HTTP
/// server per request to return a 500 without killing the process.
#[derive(Debug, Clone)]
pub struct RuntimeFault {
    pub code: String,
    pub message: String,
    pub help: String,
}

impl RuntimeFault {
    /// Print the fault in the standard Rust-grade diagnostic format.
    pub fn report(&self) {
        eprintln!("\x1b[31;1merror\x1b[0m[{}]: {}", self.code, self.message);
        if !self.help.is_empty() {
            eprintln!("  \x1b[36m= help\x1b[0m: {}", self.help);
        }
    }
}

/// Raise a recoverable runtime fault. Unwinds to the nearest catch boundary
/// (top-level run, or per-request server dispatch) instead of exiting the
/// process, so one bad request can no longer take down a server.
pub fn runtime_error(code: &str, msg: &str, help: &str) -> ! {
    std::panic::panic_any(RuntimeFault {
        code: code.to_string(),
        message: msg.to_string(),
        help: help.to_string(),
    });
}

/// Run a closure, catching a `RuntimeFault` unwind. Returns `Err(fault)` if one
/// was raised; re-panics on any other (non-fault) panic. Used by both the
/// top-level runner and the HTTP request dispatcher.
pub fn catch_fault<F, R>(f: F) -> Result<R, RuntimeFault>
where
    F: FnOnce() -> R,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => {
            if let Some(fault) = payload.downcast_ref::<RuntimeFault>() {
                Err(fault.clone())
            } else {
                // Not our fault type: propagate (e.g. a genuine bug/panic).
                std::panic::resume_unwind(payload);
            }
        }
    }
}

/// Convert a `RuntimeFault` into a Ran `Value::Map` so `try`/recover code can
/// inspect the failure as ordinary data. The shape mirrors the error maps
/// produced elsewhere in the runtime (db/concurrency helpers):
/// `{ "error": true, "code": <code>, "message": <message> }` (R4.1, R4.5, R4.6).
#[allow(dead_code)]
pub(crate) fn fault_to_value(f: &RuntimeFault) -> Value {
    let mut m: HashMap<String, Value> = HashMap::new();
    m.insert("error".to_string(), Value::Bool(true));
    m.insert("code".to_string(), Value::Str(f.code.clone()));
    m.insert("message".to_string(), Value::Str(f.message.clone()));
    Value::Map(m)
}

/// Install a panic hook that stays silent for `RuntimeFault` (we print those at
/// the catch boundary) but keeps default reporting for genuine panics. Call
/// once at program start.
pub fn install_fault_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if info.payload().downcast_ref::<RuntimeFault>().is_some() {
            return; // handled at the catch boundary
        }
        default(info);
    }));
}

/// Global memory-safety watchdog. Memory safety is a first-class goal of Ran:
/// rather than let a program exhaust RAM — leaking, thrashing, or triggering the
/// OS OOM-killer (which can take down a laptop or server) — the process stops
/// *itself* cleanly the moment free memory crosses a safety floor.
///
/// This runs for every execution path (interpreter, compiled binary, the HTTP
/// server, and the `--vm` path) because it is a background daemon thread polling
/// the system's available memory. It complements the per-loop guard (`E1006`):
/// the loop guard catches tight allocation loops between checks, while this
/// watchdog catches *any* growth (recursion, data structures, server load) on a
/// fixed ~200 ms cadence. Probing is cheap (one `/proc/meminfo` read) and the
/// thread does not keep the process alive after `main` returns.
///
/// Coverage (R2.7): this is installed for *every* execution path. `main.rs`
/// calls it once at process start, and each runtime entry point (`execute`,
/// `execute_statements`) calls it again so the watchdog is present even when the
/// runtime is entered without going through `main` (e.g. the `--vm` path that
/// falls back via `run_checked_on_big_stack -> execute`, the HTTP server, and
/// the standalone build binary). The `Once` below makes those repeated calls
/// idempotent so multiple entry paths never spawn duplicate watchdog threads.
pub fn install_memory_watchdog() {
    use std::time::Duration;
    static WATCHDOG_ONCE: std::sync::Once = std::sync::Once::new();
    WATCHDOG_ONCE.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("ran-memwatch".to_string())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_millis(200));
                let avail = crate::support::sysinfo::mem_available();
                if avail == 0 {
                    continue; // probing unsupported/failed: never interfere (R2.5)
                }
                let total = crate::support::sysinfo::mem_total();
                // Stop before the OS does: keep at least max(total/32, 128 MiB) free.
                let floor = (total / 32).max(128 * 1024 * 1024);
                if avail < floor {
                    // E1006 diagnostic (R2.6): free memory, the Safety_Floor, and a help line.
                    eprintln!(
                        "\x1b[31;1merror\x1b[0m[E1006]: out of memory: only {} free, below the {} safety floor",
                        crate::support::sysinfo::human_bytes(avail),
                        crate::support::sysinfo::human_bytes(floor),
                    );
                    eprintln!(
                        "  \x1b[36m= help\x1b[0m: Ran stopped the process itself to protect the system from an out-of-memory crash"
                    );
                    std::process::exit(70);
                }
            });
    });
}

use std::cell::{Cell, RefCell};
thread_local! {
    static TEST_MODE: Cell<bool> = const { Cell::new(false) };
    static TEST_FAILURE: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Call sites (`callee::param`) that already emitted a `&mut` write-back
    /// note in `warn` mode, so the note is printed at most once per site and
    /// stays low-noise (see Migrasi: write-back active in all phases).
    static MUT_NOTE_SEEN: RefCell<std::collections::HashSet<String>> =
        RefCell::new(std::collections::HashSet::new());
    /// Response headers a handler asked to set (via `http.set_header`/`set_cookie`).
    static RESP_HEADERS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// Response status a handler asked for (via `http.set_status`/`redirect`).
    static RESP_STATUS: Cell<Option<u16>> = const { Cell::new(None) };
    /// Id of the most recently `spawn`ed thread on this thread, so the program
    /// can retrieve a handle to it via `concurrency.last_thread()` (spawn is a
    /// statement and cannot yield a value directly).
    static LAST_THREAD_ID: Cell<u64> = const { Cell::new(0) };
    /// Desired SPA fallback flag for the built-in web server, set by
    /// `web.spa(bool)` and applied when the server is started by
    /// `web.serve`/`http.server` (R2.1/R2.2). Default: disabled.
    static WEB_SPA: Cell<bool> = const { Cell::new(false) };
    /// Frontend build command recorded by `web.build(cmd)`. When set, it is run
    /// to completion before the web server begins serving (R5.2). `None` means
    /// no build step is configured.
    static WEB_BUILD_CMD: RefCell<Option<String>> = const { RefCell::new(None) };
}

use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    /// Active Ran function-call depth on *this* execution thread (Recursion_Guard,
    /// R1.1). It is `thread_local` so each `spawn`ed thread tracks its own depth
    /// independently — a deep stack on one thread never affects another. The
    /// counter is incremented/decremented at the single call boundary
    /// (`run_function_frame*` in `frame.rs`, wired in task 1.2); enforcement
    /// compares it against `current_max_depth()`.
    static CALL_DEPTH: Cell<usize> = const { Cell::new(0) };

    /// Pending control-flow signal carried *out of a `match` arm* (R8.5).
    ///
    /// A `match` is evaluated in expression position: `eval_expression` yields a
    /// `Value`, not a `Flow`, so a `return`/`break`/`continue` executed inside an
    /// arm body cannot return a `Flow` to the statement executor directly. The
    /// arm evaluator (`eval_match_body`) stashes the signal here, and the
    /// statement sequencer (`run_stmts`) consumes it after the enclosing
    /// statement runs — unwinding a `return` value to the enclosing function and
    /// letting `break`/`continue` reach an enclosing loop. It is `thread_local`
    /// so each `spawn`ed thread has its own channel (never crosses threads).
    static PENDING_FLOW: RefCell<Option<Flow>> = const { RefCell::new(None) };
}

/// Stash a control-flow signal raised inside a `match` arm so the statement
/// sequencer can honor it (R8.5). Only ever called with a non-`Normal` flow.
fn set_pending_flow(flow: Flow) {
    PENDING_FLOW.with(|p| *p.borrow_mut() = Some(flow));
}

/// True if a `match` arm raised a control-flow signal that has not yet been
/// consumed. Used to stop evaluating the rest of an arm body once one fires.
fn pending_flow_present() -> bool {
    PENDING_FLOW.with(|p| p.borrow().is_some())
}

/// Take (and clear) any pending control-flow signal raised inside a `match`
/// arm. Returns `None` when no signal is pending.
fn take_pending_flow() -> Option<Flow> {
    PENDING_FLOW.with(|p| p.borrow_mut().take())
}

/// Effective call-depth limit for the whole process (Recursion_Guard, R1.3).
/// Defaults to 10000 frames — chosen conservatively to stay well below the OS
/// thread stack (each Ran frame consumes Rust stack) so `E1007` is raised before
/// a real stack overflow (SIGSEGV) can occur. Overridden via `--max-depth=<N>`
/// (wired in `main.rs`, task 1.3) through `set_max_call_depth`.
static MAX_CALL_DEPTH: AtomicUsize = AtomicUsize::new(10_000);

/// Set the effective call-depth limit (R1.4). Called from `main` when parsing
/// the `--max-depth=<N>` flag. Values are stored as-is; flag validation (R1.5)
/// lives at the parse site so an invalid value can fall back to the default.
pub fn set_max_call_depth(n: usize) {
    MAX_CALL_DEPTH.store(n, Ordering::Relaxed);
}

/// Read the effective call-depth limit. Used by the Recursion_Guard at the call
/// boundary to decide when to raise `E1007` (task 1.2).
pub(crate) fn current_max_depth() -> usize {
    MAX_CALL_DEPTH.load(Ordering::Relaxed)
}

fn resp_reset() {
    RESP_HEADERS.with(|h| h.borrow_mut().clear());
    RESP_STATUS.with(|s| s.set(None));
}
fn resp_add_header(k: &str, v: &str) {
    RESP_HEADERS.with(|h| h.borrow_mut().push((k.to_string(), v.to_string())));
}
fn resp_set_status(code: u16) {
    RESP_STATUS.with(|s| s.set(Some(code)));
}
fn resp_take() -> (Option<u16>, Vec<(String, String)>) {
    let status = RESP_STATUS.with(|s| s.get());
    let headers = RESP_HEADERS.with(|h| h.borrow_mut().drain(..).collect());
    (status, headers)
}

/// Run the user-configured frontend build command to completion before the web
/// server starts serving (R5.2).
///
/// The command is an opaque, user-provided string. It is executed through the
/// system command interpreter so a full command line (pipes, arguments, etc.)
/// works as written. Child stdin/stdout/stderr are inherited so build progress
/// is visible on the user's terminal.
///
/// Returns `Ok(())` only when the command spawns AND exits successfully
/// (status code 0). A spawn failure or any non-zero exit yields `Err(())`, so
/// the caller can emit `E0404` and refuse to serve stale/unbuilt assets. This
/// uses only the standard library and never panics on a failed build.
fn run_frontend_build(cmd: &str) -> Result<(), ()> {
    use std::process::{Command, Stdio};

    // Empty command: nothing to build, treat as success.
    if cmd.trim().is_empty() {
        return Ok(());
    }

    let mut command = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        // Run through the system shell so a full command line works.
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };

    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match command.status() {
        Ok(status) if status.success() => Ok(()),
        // Spawn succeeded but the command exited non-zero, or the process
        // could not be spawned at all: both mean the build did not succeed.
        _ => Err(()),
    }
}

fn test_mode_active() -> bool {
    TEST_MODE.with(|m| m.get())
}

/// Map an import path to the runtime module name: `std::http` -> `http`.
fn real_module_name(path: &str) -> String {
    // Treat both `std::` and bare module names uniformly.
    path.strip_prefix("std::").unwrap_or(path).to_string()
}

/// Process-global ownership mode flag, set by `execute`/`run_tests` from the
/// `CheckedProgram`. Drives whether the `&mut` write-back note is surfaced
/// (note only in `warn`); write-back itself is active in all phases. A global
/// (rather than thread-local) so spawned worker threads observe the same mode.
static OWNERSHIP_WARN_MODE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Record the effective ownership mode for this run (see `OWNERSHIP_WARN_MODE`).
fn set_ownership_warn(warn: bool) {
    OWNERSHIP_WARN_MODE.store(warn, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a parameter should receive `&mut` write-back to the caller (R11.6).
///
/// A parameter is treated as a mutable borrow when its type annotation is
/// `&mut T` (parsed as `TypeExpr::Ref { mutable: true, .. }`), which is the
/// canonical way to declare an observable mutation. The `mut` keyword form
/// (`fn f(mut x)`) is also honored so either spelling flows back.
fn param_is_mut(p: &Param) -> bool {
    if p.is_mut {
        return true;
    }
    matches!(&p.type_annotation, Some(TypeExpr::Ref { mutable: true, .. }))
}

/// Are we running in ownership `warn` mode? Write-back of `&mut` is active in
/// all phases (see design "Migrasi"); the informational note below is only
/// surfaced in `warn` mode. The mode is taken from the active `CheckedProgram`
/// (recorded via `set_ownership_warn`); the compatibility-first default for
/// this release is `warn`.
fn ownership_is_warn() -> bool {
    OWNERSHIP_WARN_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// In `warn` mode, emit a single low-noise note the first time a given `&mut`
/// call site (`callee::param`) has its mutation written back to the caller, so
/// users migrating toward `strict` can see where behavior now differs (R11.6).
fn maybe_emit_mut_note(callee: &str, param: &str) {
    if !ownership_is_warn() {
        return;
    }
    let key = format!("{}::{}", callee, param);
    let fresh = MUT_NOTE_SEEN.with(|s| s.borrow_mut().insert(key));
    if fresh {
        eprintln!(
            "\x1b[36mnote\x1b[0m: `&mut {}` in `{}` now writes back to the caller \
             (ownership=warn); run with `--ownership=strict` to enforce borrow rules",
            param, callee
        );
    }
}

fn record_test_failure(msg: &str) {
    TEST_FAILURE.with(|f| {
        let mut slot = f.borrow_mut();
        if slot.is_none() {
            *slot = Some(msg.to_string());
        }
    });
}

/// Escape a string for embedding in JSON (RFC 8259): quotes, backslash, the
/// control characters, and the named short escapes.
fn json_escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Run all `test_*` functions in the program. Returns the process exit code
/// (0 if every test passed). Powers `ran test`.
pub fn run_tests(checked: &CheckedProgram) -> i32 {
    let mut env = Environment::new();

    // Record the effective ownership mode so the `&mut` write-back note is
    // gated to `warn` (write-back itself runs in all phases — see Migrasi).
    set_ownership_warn(checked.ownership_mode == crate::semantics::analyzer::OwnershipMode::Warn);

    // Register declarations (functions, impls, top-level vars, imports).
    // Traits are registered first so `impl Trait for Type` blocks can inherit
    // default method bodies regardless of source order (R8.6).
    for stmt in &checked.program.statements {
        if let Statement::TraitDecl { name, methods, .. } = &stmt.kind {
            env.register_trait(name, methods);
        }
    }
    for stmt in &checked.program.statements {
        match &stmt.kind {
            Statement::FnDecl { name, params, body, .. } => {
                env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params.insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                env.fn_mut.insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
            Statement::VarDecl { name, value, .. } => {
                let v = env.eval_expression(value);
                env.var_set(name, v);
            }
            Statement::Import { path, alias } => {
                let key = alias.clone().unwrap_or_else(|| real_module_name(path));
                env.module_aliases.insert(key, real_module_name(path));
            }
            Statement::ImplBlock { type_name, trait_name, methods } => {
                env.register_impl(type_name, trait_name.as_deref(), methods);
            }
            Statement::EnumDecl { name, variants, .. } => {
                env.register_enum(name, variants);
            }
            _ => {}
        }
    }

    let mut tests: Vec<String> = env
        .functions
        .keys()
        .filter(|n| n.starts_with("test_"))
        .cloned()
        .collect();
    tests.sort();

    if tests.is_empty() {
        println!("\x1b[33mno tests found\x1b[0m (define functions named `test_*`)");
        return 0;
    }

    println!("\x1b[32;1mrunning\x1b[0m {} test{}", tests.len(), if tests.len() == 1 { "" } else { "s" });
    TEST_MODE.with(|m| m.set(true));

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    for name in &tests {
        TEST_FAILURE.with(|f| *f.borrow_mut() = None);
        if let Some(body) = env.functions.get(name).cloned() {
            // A runtime fault in a test is a failure, not a crash. Each test
            // runs in its own function frame (globals + a fresh local scope).
            let faulted = catch_fault(|| {
                let _ = env.run_function_frame(&body[..], Vec::new());
            });
            if let Err(fault) = faulted {
                record_test_failure(&format!("error[{}]: {}", fault.code, fault.message));
            }
        }
        let failure = TEST_FAILURE.with(|f| f.borrow_mut().take());
        match failure {
            None => {
                println!("  \x1b[32mok\x1b[0m   {}", name);
                passed += 1;
            }
            Some(msg) => {
                println!("  \x1b[31mFAIL\x1b[0m {} — {}", name, msg);
                failures.push((name.clone(), msg));
                failed += 1;
            }
        }
    }

    TEST_MODE.with(|m| m.set(false));
    println!();
    if failed == 0 {
        println!("\x1b[32;1mtest result: ok\x1b[0m. {} passed; 0 failed", passed);
        0
    } else {
        println!("\x1b[31;1mtest result: FAILED\x1b[0m. {} passed; {} failed", passed, failed);
        1
    }
}

/// Execute a checked program via interpretation
pub fn execute(checked: &CheckedProgram) {
    // R2.7: guarantee the Memory_Watchdog is running for this execution path
    // (interpreter `ran run`, standalone binary, HTTP server, and the `--vm`
    // fallback that reaches here via `run_checked_on_big_stack`). Idempotent —
    // the `Once` inside ensures no duplicate watchdog thread is spawned even
    // though `main` already installed it.
    install_memory_watchdog();

    let mut env = Environment::new();

    // Record the effective ownership mode so the `&mut` write-back note is
    // gated to `warn` (write-back itself runs in all phases — see Migrasi).
    set_ownership_warn(checked.ownership_mode == crate::semantics::analyzer::OwnershipMode::Warn);

    // First pass: register all top-level declarations
    // Traits first so `impl Trait for Type` blocks inherit default method
    // bodies regardless of source order (R8.6).
    for stmt in &checked.program.statements {
        if let Statement::TraitDecl { name, methods, .. } = &stmt.kind {
            env.register_trait(name, methods);
        }
    }
    for stmt in &checked.program.statements {
        match &stmt.kind {
            Statement::FnDecl { name, params, body, .. } => {
                env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                env.fn_params.insert(name.clone(), param_names);
                env.fn_mut.insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
            Statement::VarDecl { name, value, .. } => {
                let val = env.eval_expression(value);
                env.var_set(name, val);
            }
            Statement::Import { path, alias } => {
                // Register stdlib alias -> real module name
                let key = alias.clone().unwrap_or_else(|| real_module_name(path));
                env.module_aliases.insert(key, real_module_name(path));
            }
            Statement::ImplBlock { type_name, trait_name, methods } => {
                env.register_impl(type_name, trait_name.as_deref(), methods);
            }
            Statement::EnumDecl { name, variants, .. } => {
                env.register_enum(name, variants);
            }
            _ => {}
        }
    }

    // Entry point selection:
    // - if --port N was given (RAN_PORT set), call `fn port(N)` instead of main
    // - otherwise call main()
    if let Ok(port_str) = std::env::var("RAN_PORT") {
        let port: i64 = port_str.parse().unwrap_or(0);
        if env.functions.contains_key("port") {
            // Validate arity: fn port must take exactly one parameter
            let arity = env.fn_params.get("port").map(|p| p.len()).unwrap_or(0);
            if arity != 1 {
                eprintln!("\x1b[31;1merror\x1b[0m: `fn port` must take exactly one int parameter");
                eprintln!("  \x1b[36m= help\x1b[0m: define `fn port(p: int) {{ ... }}`");
                std::process::exit(1);
            }
            env.call_function("port", &[crate::frontend::ast::Expression::IntLiteral(port)]);
            return;
        } else {
            eprintln!("\x1b[31;1merror\x1b[0m: --port requires a `fn port(p: int)` function");
            eprintln!("  \x1b[36m= help\x1b[0m: define `fn port(p: int) {{ http.server(p) }}`");
            std::process::exit(1);
        }
    }

    // Call main if it exists, in its own function frame.
    if checked.has_main {
        if let Some(main_body) = env.functions.get("main").cloned() {
            let result = catch_fault(|| {
                env.run_function_frame(&main_body[..], Vec::new());
            });
            if let Err(fault) = result {
                fault.report();
                env.join_spawned();
                crate::stdlib::concurrency::join_all_remaining_threads();
                std::process::exit(70);
            }
        }
    }

    // Wait for any tasks started with `spawn` to finish before exiting.
    env.join_spawned();
    crate::stdlib::concurrency::join_all_remaining_threads();
}

/// Execute all statements directly (for REPL and scripting mode)
pub fn execute_statements(program: &Program) {
    // R2.7: ensure the Memory_Watchdog covers the REPL/scripting entry path too
    // (idempotent via the `Once` in `install_memory_watchdog`).
    install_memory_watchdog();

    let mut env = Environment::new();

    // Register import aliases first
    for stmt in &program.statements {
        if let Statement::Import { path, alias } = &stmt.kind {
            let key = alias.clone().unwrap_or_else(|| real_module_name(path));
            env.module_aliases.insert(key, real_module_name(path));
        }
    }

    for stmt in &program.statements {
        let flow = env.exec_statement(stmt);
        // A `return` raised inside a top-level `match` arm surfaces via the
        // pending-flow channel; consume it so it ends the top-level run and
        // never leaks into a later statement (R8.5).
        if let Some(pending) = take_pending_flow() {
            if let Flow::Return(_) = pending {
                break;
            }
        }
        if let Flow::Return(_) = flow {
            break;
        }
    }
    env.join_spawned();
    crate::stdlib::concurrency::join_all_remaining_threads();
}

// ============================================================================
// Value types
// ============================================================================

/// Runtime value
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    /// Exact base-10 fixed-point decimal — for money and business math.
    Decimal(crate::support::decimal::Decimal),
    Str(String),
    Bool(bool),
    Array(Vec<Value>),
    Map(HashMap<String, Value>),
    /// A struct instance: type name + named fields. Value semantics.
    Object(String, HashMap<String, Value>),
    /// A first-class closure: an anonymous function value that captures the
    /// scope visible where it was created (R8.1, R8.2). The body is shared via
    /// `Arc` so cloning a closure value is cheap, and `captured` is a snapshot
    /// of the enclosing bindings (by value) taken at creation time.
    Closure {
        params: Vec<Param>,
        body: std::sync::Arc<Vec<Stmt>>,
        captured: Scope,
    },
    Void,
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Float(n) => write!(f, "{}", n),
            Value::Decimal(d) => write!(f, "{}", d),
            Value::Str(s) => write!(f, "{}", s),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Array(arr) => {
                let items: Vec<String> = arr.iter().map(|v| format!("{}", v)).collect();
                write!(f, "[{}]", items.join(", "))
            }
            Value::Map(map) => {
                let items: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("\"{}\": {}", k, v))
                    .collect();
                write!(f, "{{{}}}", items.join(", "))
            }
            Value::Object(name, fields) => {
                let items: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                write!(f, "{} {{{}}}", name, items.join(", "))
            }
            Value::Closure { .. } => write!(f, "<closure>"),
            Value::Void => write!(f, "()"),
        }
    }
}

impl Value {
    fn as_f64(&self) -> f64 {
        match self {
            Value::Int(n) => *n as f64,
            Value::Float(f) => *f,
            Value::Decimal(d) => d.to_f64(),
            _ => 0.0,
        }
    }

    fn as_i64(&self) -> i64 {
        match self {
            Value::Int(n) => *n,
            Value::Float(f) => *f as i64,
            Value::Decimal(d) => d.to_i64_trunc(),
            _ => 0,
        }
    }

    fn is_truthy_val(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Decimal(d) => !d.is_zero(),
            Value::Str(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Void => false,
            _ => true,
        }
    }

    fn to_json(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(n) => n.to_string(),
            // Emit decimals as an unquoted JSON number to preserve exact value.
            Value::Decimal(d) => d.to_string(),
            Value::Str(s) => format!("\"{}\"", json_escape_str(s)),
            Value::Bool(b) => b.to_string(),
            Value::Array(arr) => {
                let items: Vec<String> = arr.iter().map(|v| v.to_json()).collect();
                format!("[{}]", items.join(","))
            }
            Value::Map(map) => {
                let items: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("\"{}\":{}", k, v.to_json()))
                    .collect();
                format!("{{{}}}", items.join(","))
            }
            // Objects serialize like JSON objects (the type name is dropped).
            Value::Object(_, fields) => {
                let items: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("\"{}\":{}", k, v.to_json()))
                    .collect();
                format!("{{{}}}", items.join(","))
            }
            // Closures are not data; render as a JSON string placeholder so
            // serialization of a structure that happens to hold one never fails.
            Value::Closure { .. } => "\"<closure>\"".to_string(),
            Value::Void => "null".to_string(),
        }
    }
}

// ============================================================================
// HTTP Route & Server types
// ============================================================================

#[derive(Clone)]
struct HttpRoute {
    method: String,
    path: String,
    handler_name: String,
}

// ============================================================================
// Environment
// ============================================================================

struct Environment {
    /// Top-level (global) bindings: program-level `let`s and config vars.
    /// Shared across all function calls (not cloned per call) and visible to
    /// every function in addition to its own frames.
    globals: Scope,
    /// Local scope stack for the *current* function: parameter frame at index
    /// `frame_base`, then one frame per nested block/loop/match arm. A function
    /// call records the previous `frame_base`, sets `frame_base` to the current
    /// length, and only sees `frames[frame_base..]` — so it never sees the
    /// caller's locals, with no whole-environment clone per call.
    frames: Vec<Frame>,
    /// Start index, into `frames`, of the current function's own frames.
    frame_base: usize,
    /// Saved `frame_base` values for enclosing (caller) functions.
    base_stack: Vec<usize>,
    functions: HashMap<String, std::sync::Arc<Vec<Stmt>>>,
    fn_params: HashMap<String, Vec<String>>,
    /// Per-parameter `&mut` flags for each free function, parallel to
    /// `fn_params`. Drives `&mut` write-back to the caller's lvalue (R11.6).
    fn_mut: HashMap<String, Vec<bool>>,
    routes: Vec<HttpRoute>,
    /// Maps an import alias to its real stdlib module name (e.g. "web" -> "http")
    module_aliases: HashMap<String, String>,
    /// User-defined methods: type_name -> (method_name -> (param_names,
    /// per-param `&mut` flags, body)). The `&mut` flags drive write-back (R11.6).
    methods: HashMap<String, HashMap<String, (Vec<String>, Vec<bool>, Vec<Stmt>)>>,
    /// Trait default methods: trait_name -> (method_name -> (param_names,
    /// per-param `&mut` flags, default body)). Only methods declared with a
    /// default body are stored; an `impl Trait for Type` block inherits these
    /// for any method it does not override (R8.6).
    traits: HashMap<String, HashMap<String, (Vec<String>, Vec<bool>, Vec<Stmt>)>>,
    /// Enum declarations: enum_name -> variant names.
    enums: HashMap<String, Vec<String>>,
    /// Join handles for tasks started via `spawn`, joined before program exit.
    spawned: Vec<std::thread::JoinHandle<()>>,
}

impl Environment {
    fn new() -> Self {
        Self {
            globals: Scope::default(),
            frames: Vec::new(),
            frame_base: 0,
            base_stack: Vec::new(),
            functions: HashMap::new(),
            fn_params: HashMap::new(),
            fn_mut: HashMap::new(),
            routes: Vec::new(),
            module_aliases: HashMap::new(),
            methods: HashMap::new(),
            traits: HashMap::new(),
            enums: HashMap::new(),
            spawned: Vec::new(),
        }
    }

    // The scope/frame engine (scope_push/pop, var_get/set, run_function_frame,
    // and the exactly-once release memory model) lives in `frame.rs`.

    /// If `arg` denotes a writable lvalue for `&mut` write-back — a bare
    /// variable, an index (`arr[i]` / `map[k]`), or a field access
    /// (`obj.field`), optionally wrapped in an explicit `&mut` operator — return
    /// the underlying lvalue expression. Plain `&` (immutable borrow) and
    /// non-lvalue expressions (literals, calls, arithmetic) yield `None`.
    fn writeback_target(arg: &Expression) -> Option<&Expression> {
        let inner = match arg {
            Expression::UnaryOp { op: UnaryOperator::MutRef, operand } => operand.as_ref(),
            other => other,
        };
        match inner {
            Expression::Variable(_)
            | Expression::Index { .. }
            | Expression::FieldAccess { .. } => Some(inner),
            _ => None,
        }
    }

    /// Build the list of `(param_name, target_lvalue)` pairs for a call: for
    /// every parameter declared `&mut` whose argument is a writable lvalue,
    /// record the binding to write back after the call returns (R11.6). In
    /// `warn` mode this also surfaces a one-line informational note per call
    /// site so users can see where `&mut` mutations now flow back to them.
    fn collect_writebacks(
        &self,
        callee: &str,
        param_names: &[String],
        mut_flags: &[bool],
        args: &[Expression],
    ) -> Vec<(String, Expression)> {
        let mut out = Vec::new();
        for (i, pname) in param_names.iter().enumerate() {
            if !mut_flags.get(i).copied().unwrap_or(false) {
                continue;
            }
            if let Some(target) = args.get(i).and_then(Self::writeback_target) {
                maybe_emit_mut_note(callee, pname);
                out.push((pname.clone(), target.clone()));
            }
        }
        out
    }

    /// After a call returns, write each captured final parameter value back into
    /// the caller's recorded lvalue path (R11.6).
    fn apply_writebacks(
        &mut self,
        writebacks: &[(String, Expression)],
        finals: &[(String, Value)],
    ) {
        for (pname, target) in writebacks {
            if let Some((_, val)) = finals.iter().find(|(n, _)| n == pname) {
                self.assign_lvalue(target, val.clone());
            }
        }
    }

    /// Write `value` into the lvalue denoted by `target`, walking up nested
    /// paths with read-modify-write (value semantics). Supports:
    ///   * variable        -> update the caller's binding
    ///   * `arr[i]`         -> replace element `i` of the caller's array
    ///   * `map[k]`         -> set key `k` of the caller's map
    ///   * `obj.field`      -> set the field of the caller's object/struct/map
    /// Nested forms (`obj.items[i]`, `a.b.c`, ...) compose recursively. Returns
    /// `true` if the write landed somewhere, `false` otherwise.
    fn assign_lvalue(&mut self, target: &Expression, value: Value) -> bool {
        match target {
            Expression::Variable(name) => {
                if self.var_exists(name) {
                    self.var_set(name, value);
                    true
                } else {
                    false
                }
            }
            Expression::Index { object, index } => {
                let idx = self.eval_expression(index);
                let mut container = self.eval_expression(object);
                match (&mut container, &idx) {
                    (Value::Array(arr), Value::Int(i)) => {
                        // R7.3: bounds-checked write. Reject negative/at-or-past-end
                        // indices with `E1012` (carrying index + length) instead of
                        // silently dropping the write or wrapping the `as usize` cast.
                        match Self::checked_index(*i, arr.len()) {
                            Some(u) => arr[u] = value,
                            None => Self::index_out_of_bounds(*i, arr.len()),
                        }
                    }
                    (Value::Map(map), Value::Str(key)) => {
                        map.insert(key.clone(), value);
                    }
                    _ => return false,
                }
                self.assign_lvalue(object, container)
            }
            Expression::FieldAccess { object, field } => {
                let mut container = self.eval_expression(object);
                match &mut container {
                    Value::Object(_, fields) => {
                        fields.insert(field.clone(), value);
                    }
                    Value::Map(map) => {
                        map.insert(field.clone(), value);
                    }
                    _ => return false,
                }
                self.assign_lvalue(object, container)
            }
            _ => false,
        }
    }

    /// Join all spawned tasks, blocking until they finish.
    fn join_spawned(&mut self) {
        for handle in self.spawned.drain(..) {
            let _ = handle.join();
        }
    }

    /// Register the methods of an `impl` block under their type name. When the
    /// block is an `impl Trait for Type`, any method declared with a default
    /// body in the trait that this block does not override is inherited under
    /// the type so the existing dispatch picks it up (R8.6).
    fn register_impl(&mut self, type_name: &str, trait_name: Option<&str>, methods: &[Stmt]) {
        let mut provided: Vec<String> = Vec::new();
        {
            let entry = self.methods.entry(type_name.to_string()).or_default();
            for m in methods {
                if let Statement::FnDecl { name, params, body, .. } = &m.kind {
                    let pnames: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                    let pmut: Vec<bool> = params.iter().map(param_is_mut).collect();
                    entry.insert(name.clone(), (pnames, pmut, body.clone()));
                    provided.push(name.clone());
                }
            }
        }

        // Inherit trait default methods not overridden by this impl block.
        if let Some(tn) = trait_name {
            if let Some(defaults) = self.traits.get(tn).cloned() {
                let entry = self.methods.entry(type_name.to_string()).or_default();
                for (mname, sig) in defaults {
                    if !provided.contains(&mname) {
                        entry.entry(mname).or_insert(sig);
                    }
                }
            }
        }
    }

    /// Register a trait declaration's default methods (those carrying a body).
    /// Pure signatures (empty body) are recorded as the trait's surface but
    /// contribute no callable default.
    fn register_trait(&mut self, name: &str, methods: &[Stmt]) {
        let entry = self.traits.entry(name.to_string()).or_default();
        for m in methods {
            if let Statement::FnDecl { name: mname, params, body, .. } = &m.kind {
                if body.is_empty() {
                    continue; // signature only — no default body to inherit
                }
                let pnames: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let pmut: Vec<bool> = params.iter().map(param_is_mut).collect();
                entry.insert(mname.clone(), (pnames, pmut, body.clone()));
            }
        }
    }

    /// Register an enum declaration's variant names.
    fn register_enum(&mut self, name: &str, variants: &[crate::frontend::ast::EnumVariant]) {
        let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
        self.enums.insert(name.to_string(), names);
    }

    /// Dispatch a user-defined method on an object value.
    /// The receiver is bound to `self`; remaining params bind to `args`.
    fn call_user_method(&mut self, obj: Value, type_name: &str, method: &str, args: &[Expression]) -> Option<Value> {
        let (pnames, pmut, body) = self
            .methods
            .get(type_name)
            .and_then(|m| m.get(method))
            .cloned()?;

        let arg_values: Vec<Value> = args.iter().map(|a| self.eval_expression(a)).collect();

        // Bind the receiver as `self`. If the first param is literally named
        // `self`, the remaining params bind to args; otherwise all params bind.
        let mut params: Vec<(String, Value)> = vec![("self".to_string(), obj)];
        let has_self = pnames.first().map(|s| s.as_str()) == Some("self");
        let bind_params: &[String] = if has_self { &pnames[1..] } else { &pnames[..] };
        let bind_mut: &[bool] = if has_self {
            pmut.get(1..).unwrap_or(&[])
        } else {
            &pmut[..]
        };
        for (i, pname) in bind_params.iter().enumerate() {
            if let Some(v) = arg_values.get(i) {
                params.push((pname.clone(), v.clone()));
            }
        }

        // `&mut` write-back for the (non-self) parameters (R11.6).
        let writebacks = self.collect_writebacks(method, bind_params, bind_mut, args);
        if writebacks.is_empty() {
            return Some(self.run_function_frame(&body, params));
        }
        let capture: Vec<String> = writebacks.iter().map(|(n, _)| n.clone()).collect();
        let (ret, finals) = self.run_function_frame_capture(&body, params, &capture);
        self.apply_writebacks(&writebacks, &finals);
        Some(ret)
    }

    /// Dispatch an associated function (constructor / static method):
    /// `Type.method(args)`. No `self` is injected; params bind to args.
    fn call_assoc_method(&mut self, type_name: &str, method: &str, args: &[Expression]) -> Option<Value> {
        let (pnames, pmut, body) = self
            .methods
            .get(type_name)
            .and_then(|m| m.get(method))
            .cloned()?;
        // If the method takes `self`, it is not an associated function.
        if pnames.first().map(|s| s.as_str()) == Some("self") {
            return None;
        }
        let arg_values: Vec<Value> = args.iter().map(|a| self.eval_expression(a)).collect();
        let params: Vec<(String, Value)> = pnames
            .iter()
            .enumerate()
            .filter_map(|(i, n)| arg_values.get(i).map(|v| (n.clone(), v.clone())))
            .collect();

        // `&mut` write-back (R11.6).
        let writebacks = self.collect_writebacks(method, &pnames, &pmut, args);
        if writebacks.is_empty() {
            return Some(self.run_function_frame(&body, params));
        }
        let capture: Vec<String> = writebacks.iter().map(|(n, _)| n.clone()).collect();
        let (ret, finals) = self.run_function_frame_capture(&body, params, &capture);
        self.apply_writebacks(&writebacks, &finals);
        Some(ret)
    }

    // ========================================================================
    // Statement execution
    // ========================================================================

    fn exec_statement(&mut self, stmt: &Stmt) -> Flow {
        match &stmt.kind {
            Statement::VarDecl { name, value, .. } => {
                let val = self.eval_expression(value);
                self.var_set(name, val);
                Flow::Normal
            }

            Statement::FnDecl { name, params, body, .. } => {
                self.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                let pnames: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                self.fn_params.insert(name.clone(), pnames);
                self.fn_mut.insert(name.clone(), params.iter().map(param_is_mut).collect());
                Flow::Normal
            }

            Statement::Echo { expr, escapes } => {
                let val = self.eval_expression(expr);
                let mut output = self.interpolate_string(&format!("{}", val));
                if *escapes {
                    output = output
                        .replace("\\n", "\n")
                        .replace("\\t", "\t")
                        .replace("\\r", "\r");
                }
                println!("{}", output);
                Flow::Normal
            }

            Statement::Expr(expr) => {
                self.eval_expression(expr);
                Flow::Normal
            }

            Statement::If { condition, then_body, else_body } => {
                let cond = self.eval_expression(condition);
                if self.is_truthy(&cond) {
                    self.exec_block(then_body)
                } else if let Some(else_stmts) = else_body {
                    self.exec_block(else_stmts)
                } else {
                    Flow::Normal
                }
            }

            Statement::For { variable, iterable, body } => {
                // Fast path: `for x in range(...)` iterates numerically without
                // materializing an array. Building `range(100_000_000)` as a
                // `Vec<Value>` would allocate gigabytes and OOM; the numeric
                // loop uses O(1) memory. Skipped if the user shadowed `range`.
                if !self.functions.contains_key("range") {
                    if let Some((start, end)) = self.as_range_bounds(iterable) {
                        // Loop scope holds the loop variable (updated in place);
                        // each body runs in its own child scope, which only
                        // allocates if the body declares locals.
                        self.scope_push();
                        let mut out = Flow::Normal;
                        let mut i = start;
                        let mut tick: u64 = 0;
                        while i < end {
                            self.memory_guard_tick(tick);
                            tick = tick.wrapping_add(1);
                            self.var_set_local(variable, Value::Int(i)); // in place after iter 0
                            self.scope_push();
                            let flow = self.run_stmts(body);
                            self.scope_pop();
                            match flow {
                                Flow::Normal | Flow::Continue => {}
                                Flow::Break => break,
                                ret @ Flow::Return(_) => { out = ret; break; }
                            }
                            i += 1;
                        }
                        self.scope_pop();
                        return out;
                    }
                }
                // General path: iterate an evaluated array. Same structure —
                // the loop variable lives in the loop scope, bodies in a child.
                if let Value::Array(items) = self.eval_expression(iterable) {
                    self.scope_push();
                    let mut out = Flow::Normal;
                    let mut tick: u64 = 0;
                    for item in items {
                        self.memory_guard_tick(tick);
                        tick = tick.wrapping_add(1);
                        self.var_set_local(variable, item);
                        self.scope_push();
                        let flow = self.run_stmts(body);
                        self.scope_pop();
                        match flow {
                            Flow::Normal | Flow::Continue => {}
                            Flow::Break => break,
                            ret @ Flow::Return(_) => { out = ret; break; }
                        }
                    }
                    self.scope_pop();
                    return out;
                }
                Flow::Normal
            }

            Statement::While { condition, body } => {
                let mut tick: u64 = 0;
                loop {
                    self.memory_guard_tick(tick);
                    tick = tick.wrapping_add(1);
                    let cond = self.eval_expression(condition);
                    if !self.is_truthy(&cond) {
                        break;
                    }
                    match self.exec_block(body) {
                        Flow::Normal | Flow::Continue => {}
                        Flow::Break => break,
                        ret @ Flow::Return(_) => return ret,
                    }
                }
                Flow::Normal
            }

            Statement::Spawn { body } => {
                let body_clone = body.clone();
                let vars = self.flatten_scopes();
                let funcs = self.functions.clone();
                let fn_params = self.fn_params.clone();
                let fn_mut = self.fn_mut.clone();
                let aliases = self.module_aliases.clone();
                let methods_clone = self.methods.clone();
                let traits_clone = self.traits.clone();
                let enums_clone = self.enums.clone();
                // The spawned closure returns the thread body's result `Value`.
                // A `RuntimeFault` raised inside the body is caught here (via the
                // existing `catch_fault`) and rendered as a catchable Ran error
                // value, so a faulting thread is delivered to its joiner as an
                // error value instead of crashing the process (R12.6).
                let handle = std::thread::spawn(move || -> Value {
                    let outcome = catch_fault(move || {
                        let mut child = Environment {
                            globals: vars,
                            frames: Vec::new(),
                            frame_base: 0,
                            base_stack: Vec::new(),
                            functions: funcs,
                            fn_params,
                            fn_mut,
                            routes: Vec::new(),
                            module_aliases: aliases,
                            methods: methods_clone,
                            traits: traits_clone,
                            enums: enums_clone,
                            spawned: Vec::new(),
                        };
                        let mut result = Value::Void;
                        for s in &body_clone {
                            let flow = child.exec_statement(s);
                            // Honor a `return` raised inside a `match` arm in
                            // the spawn body, surfaced via the pending-flow
                            // channel (R8.5).
                            if let Some(Flow::Return(v)) = take_pending_flow() {
                                result = v;
                                break;
                            }
                            if let Flow::Return(v) = flow {
                                result = v;
                                break;
                            }
                        }
                        child.join_spawned();
                        result
                    });
                    match outcome {
                        Ok(v) => v,
                        Err(fault) => Environment::thread_fault_value(&fault),
                    }
                });
                // Register the handle and expose its unique id (R12.1). Because
                // `spawn` is a statement, the id is recorded thread-locally so
                // the program can capture it via `concurrency.last_thread()`.
                let id = crate::stdlib::concurrency::register_thread(handle);
                LAST_THREAD_ID.with(|c| c.set(id));
                Flow::Normal
            }

            Statement::Return(Some(expr)) => {
                let val = self.eval_expression(expr);
                Flow::Return(val)
            }
            Statement::Return(None) => Flow::Return(Value::Void),

            Statement::Break => Flow::Break,

            Statement::Continue => Flow::Continue,
            Statement::ImplBlock { type_name, trait_name, methods } => {
                self.register_impl(type_name, trait_name.as_deref(), methods);
                Flow::Normal
            }
            Statement::TraitDecl { name, methods, .. } => {
                self.register_trait(name, methods);
                Flow::Normal
            }
            Statement::EnumDecl { name, variants, .. } => {
                self.register_enum(name, variants);
                Flow::Normal
            }
            _ => Flow::Normal,
        }
    }

    /// Execute statements in the current scope (no new frame). Stops early on a
    /// non-Normal flow signal.
    fn run_stmts(&mut self, stmts: &[Stmt]) -> Flow {
        for s in stmts {
            let flow = self.exec_statement(s);
            // A `return`/`break`/`continue` raised inside a `match` arm is
            // carried out-of-band via `PENDING_FLOW` because `match` is
            // evaluated in expression position (it cannot return a `Flow`).
            // Honor it here so the signal unwinds to the enclosing function or
            // loop exactly as a normal statement-level signal would (R8.5).
            if let Some(pending) = take_pending_flow() {
                return pending;
            }
            match flow {
                Flow::Normal => {}
                other => return other,
            }
        }
        Flow::Normal
    }

    /// Execute a block in its own lexical scope: locals declared inside do not
    /// leak out. Propagates return/break/continue to the caller.
    fn exec_block(&mut self, stmts: &[Stmt]) -> Flow {
        self.scope_push();
        let flow = self.run_stmts(stmts);
        self.scope_pop();
        flow
    }

    // ========================================================================
    // Expression evaluation
    // ========================================================================

    fn eval_expression(&mut self, expr: &Expression) -> Value {
        match expr {
            Expression::IntLiteral(n) => Value::Int(*n),
            Expression::FloatLiteral(n) => Value::Float(*n),
            Expression::StringLiteral(s) => Value::Str(s.clone()),
            Expression::BoolLiteral(b) => Value::Bool(*b),

            Expression::Variable(name) => self.var_get(name).unwrap_or(Value::Void),

            Expression::BinaryOp { left, op, right } => {
                let l = self.eval_expression(left);
                let r = self.eval_expression(right);
                self.eval_binary_op(&l, op, &r)
            }

            Expression::UnaryOp { op, operand } => {
                let val = self.eval_expression(operand);
                match op {
                    UnaryOperator::Neg => match val {
                        Value::Int(n) => Value::Int(-n),
                        Value::Float(n) => Value::Float(-n),
                        _ => Value::Void,
                    },
                    UnaryOperator::Not => Value::Bool(!self.is_truthy(&val)),
                    _ => val,
                }
            }

            Expression::FnCall { callee, args } => {
                if let Expression::Variable(name) = callee.as_ref() {
                    self.call_function(name, args)
                } else {
                    Value::Void
                }
            }

            Expression::MethodCall { object, method, args } => {
                if let Expression::Variable(ident) = object.as_ref() {
                    // Module call (http.get, decimal.add, ...) takes priority.
                    if self.is_module(ident) {
                        let real = self.resolve_module_alias(ident).to_string();
                        return self.call_module_method(&real, method, args);
                    }
                    // Associated function / constructor: `Type.method(args)` when
                    // `ident` is a known type and not a bound variable.
                    if !self.var_exists(ident) && self.methods.contains_key(ident) {
                        if let Some(v) = self.call_assoc_method(ident, method, args) {
                            return v;
                        }
                    }
                }
                let obj = self.eval_expression(object);
                // User-defined instance method on a struct instance?
                if let Value::Object(type_name, _) = &obj {
                    let tn = type_name.clone();
                    if let Some(v) = self.call_user_method(obj.clone(), &tn, method, args) {
                        return v;
                    }
                }
                // Fall back to built-in value methods (string/array/map/decimal).
                self.call_method(&obj, method, args)
            }

            Expression::FieldAccess { object, field } => {
                // Enum variant access: `Status.Active` when `Status` is an enum.
                if let Expression::Variable(name) = object.as_ref() {
                    if let Some(variants) = self.enums.get(name) {
                        if variants.contains(field) {
                            let mut m = HashMap::new();
                            m.insert("variant".to_string(), Value::Str(field.clone()));
                            return Value::Object(name.clone(), m);
                        }
                    }
                }
                let obj = self.eval_expression(object);
                match obj {
                    Value::Object(_, fields) => fields.get(field).cloned().unwrap_or(Value::Void),
                    Value::Map(map) => map.get(field).cloned().unwrap_or(Value::Void),
                    _ => Value::Void,
                }
            }

            Expression::Match { subject, arms } => {
                let subj = self.eval_expression(subject);
                for arm in arms {
                    if self.pattern_matches(&arm.pattern, &subj) {
                        // Run the matched arm in its own scope so a binding
                        // pattern does not leak into the surrounding scope.
                        self.scope_push();
                        if let Pattern::Variable(name) = &arm.pattern {
                            self.var_set_local(name, subj.clone());
                        }
                        let v = self.eval_match_body(&arm.body);
                        self.scope_pop();
                        return v;
                    }
                }
                Value::Void
            }

            Expression::StructInit { name, fields } => {
                let mut obj_fields = HashMap::new();
                for (fname, fexpr) in fields {
                    let v = self.eval_expression(fexpr);
                    obj_fields.insert(fname.clone(), v);
                }
                Value::Object(name.clone(), obj_fields)
            }

            Expression::Index { object, index } => {
                let obj = self.eval_expression(object);
                let idx = self.eval_expression(index);
                match (&obj, &idx) {
                    (Value::Array(arr), Value::Int(i)) => {
                        // R7.3: bounds-checked array indexing. A raw `arr[*i as
                        // usize]` would panic the host on `i >= len`, and a
                        // negative `i` would wrap to a huge `usize` on the `as`
                        // cast. Reject both with a recoverable `E1012` fault
                        // whose message carries the offending index and length.
                        Self::checked_index(*i, arr.len())
                            .map(|u| arr[u].clone())
                            .unwrap_or_else(|| Self::index_out_of_bounds(*i, arr.len()))
                    }
                    (Value::Str(s), Value::Int(i)) => {
                        // R7.3: char-boundary-aware string indexing. Index by
                        // Unicode scalar (char), never raw bytes, so we never
                        // split a multi-byte character or panic on a byte
                        // boundary. Out-of-range (incl. negative) → `E1012`.
                        let len = s.chars().count();
                        Self::checked_index(*i, len)
                            .and_then(|u| s.chars().nth(u))
                            .map(|c| Value::Str(c.to_string()))
                            .unwrap_or_else(|| Self::index_out_of_bounds(*i, len))
                    }
                    (Value::Map(map), Value::Str(key)) => {
                        map.get(key).cloned().unwrap_or(Value::Void)
                    }
                    _ => Value::Void,
                }
            }

            Expression::Array(elements) => {
                let values: Vec<Value> = elements.iter().map(|e| self.eval_expression(e)).collect();
                Value::Array(values)
            }

            Expression::Pipe { left, right } => {
                let _l = self.eval_expression(left);
                self.eval_expression(right)
            }

            // Closure literal: capture the currently visible scope by value
            // (globals + the active function's frames) so the closure can read
            // those bindings later, even after the defining scope has exited
            // (R8.1, R8.2). The body is shared via `Arc` for cheap cloning.
            Expression::Lambda { params, body } => Value::Closure {
                params: params.clone(),
                body: std::sync::Arc::new(body.clone()),
                captured: self.flatten_scopes(),
            },

            _ => Value::Void,
        }
    }

    /// Invoke a closure value: run its body in a fresh function frame seeded
    /// with the captured bindings first, then the call arguments bound to the
    /// closure's parameters (so parameters shadow same-named captures). This
    /// reuses the same call boundary as named functions (`run_function_frame`),
    /// so the Recursion_Guard and frame model apply uniformly (R8.1, R8.2).
    pub(crate) fn call_closure(
        &mut self,
        params: &[Param],
        body: &std::sync::Arc<Vec<Stmt>>,
        captured: &Scope,
        arg_values: Vec<Value>,
    ) -> Value {
        let mut frame: Vec<(String, Value)> = captured
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (i, p) in params.iter().enumerate() {
            if let Some(v) = arg_values.get(i) {
                frame.push((p.name.clone(), v.clone()));
            }
        }
        self.run_function_frame(&body[..], frame)
    }

    /// R7.3: validate an index against a container length without ever
    /// panicking. Returns `Some(i as usize)` only when `0 <= i < len`; a
    /// negative index or one at/past the end yields `None` (never a `usize`
    /// wraparound from the `as` cast).
    fn checked_index(i: i64, len: usize) -> Option<usize> {
        if i < 0 {
            return None;
        }
        let u = i as usize;
        if u < len { Some(u) } else { None }
    }

    /// R7.3: raise the recoverable `E1012` index-out-of-bounds fault. The
    /// message carries BOTH the offending index and the container length so
    /// the developer can see exactly what went wrong.
    fn index_out_of_bounds(i: i64, len: usize) -> ! {
        runtime_error(
            "E1012",
            &format!("index out of bounds: index {} but length {}", i, len),
            "Indeks di luar batas. Pastikan 0 <= indeks < panjang sebelum mengakses elemen.",
        )
    }

    /// Decide whether a match pattern matches the subject value.
    fn pattern_matches(&mut self, pat: &Pattern, subject: &Value) -> bool {
        match pat {
            Pattern::Wildcard => true,
            Pattern::Variable(_) => true, // binds; matches anything
            Pattern::Literal(expr) => {
                let pv = self.eval_expression(expr);
                Self::values_equal(&pv, subject)
            }
        }
    }

    /// Execute a match arm body and yield its value (last expression statement).
    ///
    /// A `match` is evaluated in expression position, so a `return` (or
    /// `break`/`continue`) inside an arm cannot return a `Flow` directly. When
    /// an arm statement raises a control-flow signal, we stash it in
    /// `PENDING_FLOW` and stop running the arm; the statement sequencer
    /// (`run_stmts`) then unwinds it to the enclosing function or loop (R8.5).
    fn eval_match_body(&mut self, body: &[Stmt]) -> Value {
        let mut result = Value::Void;
        for stmt in body {
            if let Statement::Expr(e) = &stmt.kind {
                result = self.eval_expression(e);
                // A nested `match` (or other expression) inside this arm may
                // itself have raised a signal; stop and let it propagate.
                if pending_flow_present() {
                    break;
                }
            } else {
                match self.exec_statement(stmt) {
                    Flow::Normal => {}
                    other => {
                        // Carry the signal out via the side channel (R8.5).
                        set_pending_flow(other);
                        break;
                    }
                }
            }
        }
        result
    }

    /// Structural value equality for match (scalars precisely; objects/enums via
    /// their display form).
    fn values_equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::Decimal(x), Value::Decimal(y)) => x.cmp(y) == std::cmp::Ordering::Equal,
            _ => format!("{}", a) == format!("{}", b),
        }
    }

    /// Coerce a value to Decimal for mixed arithmetic. Int is lossless;
    /// Float/Str go through their decimal text form; anything else is None.
    fn to_decimal(v: &Value) -> Option<Decimal> {
        match v {
            Value::Decimal(d) => Some(*d),
            Value::Int(n) => Some(Decimal::from_int(*n)),
            Value::Float(f) => Decimal::parse(&format!("{}", f)).ok(),
            Value::Str(s) => Decimal::parse(s).ok(),
            _ => None,
        }
    }

    /// Apply a binary operator to two decimals, aborting on overflow (E1003).
    fn decimal_binop(l: &Decimal, op: &BinaryOperator, r: &Decimal) -> Value {
        use BinaryOperator::*;
        let unwrap = |res: Result<Decimal, String>| -> Decimal {
            match res {
                Ok(d) => d,
                Err(e) => runtime_error("E1003", &e, "decimal values exceed 128-bit precision"),
            }
        };
        match op {
            Add => Value::Decimal(unwrap(l.add(r))),
            Sub => Value::Decimal(unwrap(l.sub(r))),
            Mul => Value::Decimal(unwrap(l.mul(r))),
            Div => {
                if r.is_zero() {
                    runtime_error("E1002", "decimal division by zero",
                        "guard the divisor before dividing");
                }
                // Default division precision: keep the larger operand scale,
                // at least 2 places (cents), rounded half-up. Use
                // `decimal.div(a, b, scale, mode)` for explicit control.
                let scale = l.scale().max(r.scale()).max(2);
                Value::Decimal(unwrap(l.div(r, scale, Rounding::HalfUp)))
            }
            Mod => {
                // a mod b = a - floor(a/b)*b, computed exactly at combined scale.
                if r.is_zero() {
                    runtime_error("E1002", "decimal modulo by zero", "guard the divisor");
                }
                let scale = l.scale().max(r.scale());
                let q = unwrap(l.div(r, 0, Rounding::Down));
                let prod = unwrap(q.mul(r));
                Value::Decimal(unwrap(l.sub(&prod).and_then(|d| d.rescale(scale, Rounding::HalfUp))))
            }
            Eq => Value::Bool(l.cmp(r) == std::cmp::Ordering::Equal),
            Neq => Value::Bool(l.cmp(r) != std::cmp::Ordering::Equal),
            Lt => Value::Bool(l.cmp(r) == std::cmp::Ordering::Less),
            Lte => Value::Bool(l.cmp(r) != std::cmp::Ordering::Greater),
            Gt => Value::Bool(l.cmp(r) == std::cmp::Ordering::Greater),
            Gte => Value::Bool(l.cmp(r) != std::cmp::Ordering::Less),
            _ => Value::Void,
        }
    }

    fn eval_binary_op(&self, left: &Value, op: &BinaryOperator, right: &Value) -> Value {
        use BinaryOperator::*;
        match op {
            And => return Value::Bool(left.is_truthy_val() && right.is_truthy_val()),
            Or => return Value::Bool(left.is_truthy_val() || right.is_truthy_val()),
            _ => {}
        }

        // Exact decimal arithmetic. If either operand is a Decimal, both are
        // treated as Decimal (Int/Decimal mixes promote the Int losslessly;
        // Float/Decimal mixes promote the Float via its shortest decimal form).
        if matches!(left, Value::Decimal(_)) || matches!(right, Value::Decimal(_)) {
            if let (Some(l), Some(r)) = (Self::to_decimal(left), Self::to_decimal(right)) {
                return Self::decimal_binop(&l, op, &r);
            }
        }

        match (left, right) {
            // Int op Int
            (Value::Int(l), Value::Int(r)) => match op {
                // R7.1: checked integer arithmetic. The release profile sets
                // `overflow-checks = false`, so a raw `+ - *` would silently wrap
                // on overflow. Using the `checked_*` variants turns an overflow
                // into a recoverable `E1010` fault instead of an undefined value.
                Add => Value::Int(l.checked_add(*r).unwrap_or_else(|| {
                    runtime_error("E1010", &format!("integer overflow: {} + {}", l, r),
                        "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.")
                })),
                Sub => Value::Int(l.checked_sub(*r).unwrap_or_else(|| {
                    runtime_error("E1010", &format!("integer overflow: {} - {}", l, r),
                        "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.")
                })),
                Mul => Value::Int(l.checked_mul(*r).unwrap_or_else(|| {
                    runtime_error("E1010", &format!("integer overflow: {} * {}", l, r),
                        "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.")
                })),
                // R7.2: division/modulo by zero is an `E1011` fault. `checked_div`/
                // `checked_rem` also return `None` for the `i64::MIN / -1` (and
                // `i64::MIN % -1`) overflow case, which is reported as `E1010`.
                Div => {
                    if *r == 0 {
                        runtime_error("E1011", &format!("division by zero: {} / 0", l),
                            "Pembagian/modulo dengan nol. Pastikan pembagi bukan nol sebelum operasi.");
                    }
                    Value::Int(l.checked_div(*r).unwrap_or_else(|| {
                        runtime_error("E1010", &format!("integer overflow: {} / {}", l, r),
                            "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.")
                    }))
                }
                Mod => {
                    if *r == 0 {
                        runtime_error("E1011", &format!("modulo by zero: {} % 0", l),
                            "Pembagian/modulo dengan nol. Pastikan pembagi bukan nol sebelum operasi.");
                    }
                    Value::Int(l.checked_rem(*r).unwrap_or_else(|| {
                        runtime_error("E1010", &format!("integer overflow: {} % {}", l, r),
                            "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.")
                    }))
                }
                Eq => Value::Bool(l == r),
                Neq => Value::Bool(l != r),
                Lt => Value::Bool(l < r),
                Lte => Value::Bool(l <= r),
                Gt => Value::Bool(l > r),
                Gte => Value::Bool(l >= r),
                _ => Value::Void,
            },
            // Float op Float (and mixed, via coercion below)
            (Value::Float(_), Value::Float(_))
            | (Value::Int(_), Value::Float(_))
            | (Value::Float(_), Value::Int(_)) => {
                let l = left.as_f64();
                let r = right.as_f64();
                match op {
                    Add => Value::Float(l + r),
                    Sub => Value::Float(l - r),
                    Mul => Value::Float(l * r),
                    Div => Value::Float(l / r),
                    Mod => Value::Float(l % r),
                    Eq => Value::Bool(l == r),
                    Neq => Value::Bool(l != r),
                    Lt => Value::Bool(l < r),
                    Lte => Value::Bool(l <= r),
                    Gt => Value::Bool(l > r),
                    Gte => Value::Bool(l >= r),
                    _ => Value::Void,
                }
            }
            // String op String
            (Value::Str(l), Value::Str(r)) => match op {
                Add => Value::Str(format!("{}{}", l, r)),
                Eq => Value::Bool(l == r),
                Neq => Value::Bool(l != r),
                Lt => Value::Bool(l < r),
                Lte => Value::Bool(l <= r),
                Gt => Value::Bool(l > r),
                Gte => Value::Bool(l >= r),
                _ => Value::Void,
            },
            // Bool op Bool
            (Value::Bool(l), Value::Bool(r)) => match op {
                Eq => Value::Bool(l == r),
                Neq => Value::Bool(l != r),
                _ => Value::Void,
            },
            // String + anything -> concatenation
            (Value::Str(l), other) => match op {
                Add => Value::Str(format!("{}{}", l, other)),
                _ => Value::Void,
            },
            (other, Value::Str(r)) => match op {
                Add => Value::Str(format!("{}{}", other, r)),
                _ => Value::Void,
            },
            _ => Value::Void,
        }
    }

    fn is_module(&self, name: &str) -> bool {
        self.module_aliases.contains_key(name)
    }

    /// Resolve an import alias to its real stdlib module name.
    fn resolve_module_alias<'a>(&'a self, alias: &'a str) -> &'a str {
        self.module_aliases.get(alias).map(|s| s.as_str()).unwrap_or(alias)
    }


    /// The distinguishable "channel closed" indicator returned by `recv` when
    /// all senders are closed and the buffer is drained (R11.4). Modeled as a
    /// map carrying a reserved marker key so it cannot be confused with an
    /// ordinary received value.

    /// Simple time-seeded xorshift64 PRNG (good enough for scripting; not crypto).
    fn rand_u64() -> u64 {
        use std::cell::Cell;
        use std::time::{SystemTime, UNIX_EPOCH};
        thread_local! {
            static STATE: Cell<u64> = Cell::new(0);
        }
        STATE.with(|s| {
            let mut x = s.get();
            if x == 0 {
                x = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0x9E3779B97F4A7C15)
                    | 1;
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            s.set(x);
            x
        })
    }

    /// Parse a boolean-ish env value: true/1/yes/on/y → true; false/0/no/off/n → false.
    fn parse_env_bool(s: &str) -> Option<bool> {
        match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" | "y" => Some(true),
            "false" | "0" | "no" | "off" | "n" | "" => Some(false),
            _ => None,
        }
    }

    /// Load a dotenv-style file. Supports `KEY=value`, `export KEY=value`,
    /// `#` comments, blank lines, and single/double-quoted values. Returns the
    /// number of variables set. If `override_existing` is false, variables
    /// already present in the environment are left untouched.
    fn load_dotenv(path: &str, override_existing: bool) -> i64 {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return 0, // missing file is not an error
        };
        let mut count = 0i64;
        for raw_line in text.lines() {
            let mut line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("export ") {
                line = rest.trim();
            }
            let (key, val) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };
            if key.is_empty() {
                continue;
            }
            // Strip surrounding matching quotes.
            let val = if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
                || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
            {
                &val[1..val.len() - 1]
            } else {
                // Trim trailing inline comment for unquoted values.
                match val.find(" #") {
                    Some(i) => val[..i].trim(),
                    None => val,
                }
            };
            if !override_existing && std::env::var(key).is_ok() {
                continue;
            }
            std::env::set_var(key, val);
            count += 1;
        }
        count
    }

    /// Build a Decimal value from a string/int/float/decimal value.
    fn make_decimal(&self, v: &Value) -> Value {
        match v {
            Value::Decimal(d) => Value::Decimal(*d),
            Value::Int(n) => Value::Decimal(Decimal::from_int(*n)),
            Value::Str(s) => match Decimal::parse(s) {
                Ok(d) => Value::Decimal(d),
                Err(e) => runtime_error("E1004", &e, "use a numeric string like \"19.99\""),
            },
            Value::Float(f) => match Decimal::parse(&format!("{}", f)) {
                Ok(d) => Value::Decimal(d),
                Err(e) => runtime_error("E1004", &e, "float could not be represented as decimal"),
            },
            other => runtime_error(
                "E1004",
                &format!("cannot make a decimal from {}", other),
                "pass a string, int, or float",
            ),
        }
    }

    /// Evaluate argument `idx` and coerce to Decimal (errors via E1004).
    fn decimal_arg(&mut self, args: &[Expression], idx: usize) -> Decimal {
        let v = self.eval_arg_val(args, idx);
        match Self::to_decimal(&v) {
            Some(d) => d,
            None => runtime_error(
                "E1004",
                &format!("argument {} is not decimal-compatible", idx + 1),
                "pass a decimal, int, float, or numeric string",
            ),
        }
    }

    /// Apply a 2-arg decimal operation from the `decimal` module.
    fn decimal_op2(&mut self, args: &[Expression], op: BinaryOperator) -> Value {
        let a = self.decimal_arg(args, 0);
        let b = self.decimal_arg(args, 1);
        Self::decimal_binop(&a, &op, &b)
    }

    /// Perform an HTTP client request and wrap the result in a Ran map value.
    fn http_client_call(&self, method: &str, url: &str, body: &str) -> Value {
        let resp = crate::stdlib::net::http_request(method, url, body);
        let mut m = HashMap::new();
        m.insert("status".to_string(), Value::Int(resp.status as i64));
        m.insert("body".to_string(), Value::Str(resp.body));
        m.insert(
            "ok".to_string(),
            Value::Bool(resp.status >= 200 && resp.status < 300),
        );
        m.insert(
            "error".to_string(),
            Value::Str(resp.error.unwrap_or_default()),
        );
        Value::Map(m)
    }

    /// Emit a leveled log line to stderr: `LEVEL [ISO-8601] message`.
    fn log_at(&mut self, level: &str, color: &str, args: &[Expression]) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let ts = Self::unix_to_iso(secs as i64);
        let parts: Vec<String> = args
            .iter()
            .map(|a| {
                let v = self.eval_expression(a);
                self.interpolate_string(&format!("{}", v))
            })
            .collect();
        eprintln!("{}{:<5}\x1b[0m [{}] {}", color, level, ts, parts.join(" "));
    }

    /// Convert a Unix timestamp (seconds) to a UTC ISO-8601 string.
    /// Pure arithmetic (no external crates); accurate for dates after 1970.
    fn unix_to_iso(secs: i64) -> String {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);

        // Civil-from-days algorithm (Howard Hinnant).
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if m <= 2 { y + 1 } else { y };

        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, m, d, hh, mm, ss
        )
    }

    /// Pretty-print a value as indented JSON.
    fn to_json_pretty(val: &Value, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        let pad_inner = "  ".repeat(indent + 1);
        match val {
            Value::Array(arr) => {
                if arr.is_empty() {
                    return "[]".to_string();
                }
                let items: Vec<String> = arr
                    .iter()
                    .map(|v| format!("{}{}", pad_inner, Self::to_json_pretty(v, indent + 1)))
                    .collect();
                format!("[\n{}\n{}]", items.join(",\n"), pad)
            }
            Value::Map(map) => {
                if map.is_empty() {
                    return "{}".to_string();
                }
                let items: Vec<String> = map
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}\"{}\": {}",
                            pad_inner,
                            k,
                            Self::to_json_pretty(v, indent + 1)
                        )
                    })
                    .collect();
                format!("{{\n{}\n{}}}", items.join(",\n"), pad)
            }
            other => other.to_json(),
        }
    }

    // ========================================================================
    // HTTP Server with route handling (uses FastServer from network module)
    // ========================================================================



    // ========================================================================
    // Return propagation & helpers
    // ========================================================================

    /// Execute a function body and extract its return value. Returns `Void`
    /// if the body finishes without an explicit `return`.
    fn exec_block_with_return(&mut self, stmts: &[Stmt]) -> Value {
        match self.exec_block(stmts) {
            Flow::Return(v) => v,
            _ => Value::Void,
        }
    }

    fn is_truthy(&self, val: &Value) -> bool {
        match val {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Str(s) => !s.is_empty(),
            Value::Void => false,
            _ => true,
        }
    }

    // --- Arg helpers ---

    fn eval_arg_int(&mut self, args: &[Expression], idx: usize, default: i64) -> i64 {
        if let Some(arg) = args.get(idx) {
            match self.eval_expression(arg) {
                Value::Int(n) => n,
                Value::Float(f) => f as i64,
                Value::Str(s) => s.parse().unwrap_or(default),
                _ => default,
            }
        } else {
            default
        }
    }

    fn eval_arg_str(&mut self, args: &[Expression], idx: usize, default: &str) -> String {
        if let Some(arg) = args.get(idx) {
            match self.eval_expression(arg) {
                Value::Str(s) => s,
                other => format!("{}", other),
            }
        } else {
            default.to_string()
        }
    }

    fn eval_arg_val(&mut self, args: &[Expression], idx: usize) -> Value {
        if let Some(arg) = args.get(idx) {
            self.eval_expression(arg)
        } else {
            Value::Void
        }
    }

    fn eval_arg_f64(&mut self, args: &[Expression], idx: usize) -> f64 {
        self.eval_arg_val(args, idx).as_f64()
    }

    // --- Template rendering ---

    fn render_template(&self, template: &str) -> String {
        self.interpolate_string(template)
    }


    // --- String interpolation ---

    fn interpolate_string(&self, s: &str) -> String {
        let mut result = String::new();
        let chars: Vec<char> = s.chars().collect();
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
                    if i < chars.len() { i += 1; }
                } else {
                    // Allow dotted field paths: $user.name, $order.total
                    while i < chars.len()
                        && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                    {
                        path.push(chars[i]);
                        i += 1;
                    }
                    // A trailing dot is punctuation, not part of the path.
                    while path.ends_with('.') {
                        path.pop();
                        i -= 1;
                    }
                }
                match self.lookup_path(&path) {
                    Some(val) => result.push_str(&format!("{}", val)),
                    None => {
                        result.push('$');
                        result.push_str(&path);
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }

        result
    }

    /// Resolve a (possibly dotted) variable path like `user.address.city`
    /// against the current scope, traversing Object/Map fields.
    fn lookup_path(&self, path: &str) -> Option<Value> {
        let mut parts = path.split('.');
        let base = parts.next()?;
        let mut current = self.var_get(base)?;
        for field in parts {
            current = match current {
                Value::Object(_, fields) => fields.get(field).cloned()?,
                Value::Map(map) => map.get(field).cloned()?,
                _ => return None,
            };
        }
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment {
        Environment::new()
    }

    #[test]
    fn test_json_decode_object() {
        let e = env();
        let v = e.parse_json("{\"name\": \"Ran\", \"n\": 4}");
        if let Value::Map(m) = v {
            assert_eq!(format!("{}", m.get("name").unwrap()), "Ran");
            assert_eq!(format!("{}", m.get("n").unwrap()), "4");
        } else {
            panic!("expected map, got {:?}", v);
        }
    }

    #[test]
    fn test_json_decode_array() {
        let e = env();
        let v = e.parse_json("[1, 2, 3]");
        if let Value::Array(a) = v {
            assert_eq!(a.len(), 3);
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn test_json_nested() {
        let e = env();
        let v = e.parse_json("{\"items\": [1, 2], \"ok\": true}");
        if let Value::Map(m) = v {
            assert!(matches!(m.get("items"), Some(Value::Array(_))));
            assert!(matches!(m.get("ok"), Some(Value::Bool(true))));
        } else {
            panic!("expected map");
        }
    }

    #[test]
    fn test_binary_float_compare() {
        let e = env();
        let r = e.eval_binary_op(&Value::Float(3.5), &BinaryOperator::Gt, &Value::Int(2));
        assert!(matches!(r, Value::Bool(true)));
    }

    #[test]
    fn test_binary_mixed_arithmetic() {
        let e = env();
        let r = e.eval_binary_op(&Value::Float(3.5), &BinaryOperator::Add, &Value::Int(2));
        assert!(matches!(r, Value::Float(f) if (f - 5.5).abs() < 1e-9));
    }

    #[test]
    fn test_logical_ops() {
        let e = env();
        let and = e.eval_binary_op(&Value::Bool(true), &BinaryOperator::And, &Value::Bool(false));
        assert!(matches!(and, Value::Bool(false)));
        let or = e.eval_binary_op(&Value::Bool(false), &BinaryOperator::Or, &Value::Bool(true));
        assert!(matches!(or, Value::Bool(true)));
    }

    // --- &mut write-back to the caller (R11.6, task 8.2) ------------------
    //
    // These tests exercise the write-back path directly: a function declared
    // with a `&mut` parameter that reassigns the parameter must have that
    // final value flow back into the caller's lvalue (variable, array element,
    // map key, struct field), while a by-value parameter must not.

    /// Build an `Environment` with the source's free functions registered,
    /// mirroring the registration `execute`/`run_tests` perform.
    fn prep_funcs(src: &str) -> Environment {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                env.fn_mut
                    .insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
        }
        env
    }

    fn var(name: &str) -> Expression {
        Expression::Variable(name.to_string())
    }

    fn mut_ref(inner: Expression) -> Expression {
        Expression::UnaryOp {
            op: UnaryOperator::MutRef,
            operand: Box::new(inner),
        }
    }

    #[test]
    fn writeback_target_detection() {
        // A bare variable and an explicit `&mut <lvalue>` are write-back targets.
        assert!(Environment::writeback_target(&var("x")).is_some());
        assert!(Environment::writeback_target(&mut_ref(var("x"))).is_some());
        // A plain immutable borrow `&x` is NOT a write-back target.
        let shared = Expression::UnaryOp {
            op: UnaryOperator::Ref,
            operand: Box::new(var("x")),
        };
        assert!(Environment::writeback_target(&shared).is_none());
        // Non-lvalue expressions are never targets.
        assert!(Environment::writeback_target(&Expression::IntLiteral(1)).is_none());
    }

    #[test]
    fn writeback_variable_observed_by_caller() {
        let mut e = prep_funcs("fn bump(x: &mut int) { x = x + 41 }");
        e.var_set("n", Value::Int(1));
        e.call_function("bump", &[mut_ref(var("n"))]);
        assert!(matches!(e.var_get("n"), Some(Value::Int(42))));
    }

    #[test]
    fn byvalue_param_does_not_write_back() {
        let mut e = prep_funcs("fn bump(x: int) { x = x + 999 }");
        e.var_set("k", Value::Int(5));
        e.call_function("bump", &[var("k")]);
        // By-value semantics: the caller's binding is unchanged.
        assert!(matches!(e.var_get("k"), Some(Value::Int(5))));
    }

    #[test]
    fn writeback_array_element() {
        let mut e = prep_funcs("fn set_to(a: &mut int) { a = 7 }");
        e.var_set("arr", Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)]));
        let target = Expression::Index {
            object: Box::new(var("arr")),
            index: Box::new(Expression::IntLiteral(1)),
        };
        e.call_function("set_to", &[mut_ref(target)]);
        if let Some(Value::Array(a)) = e.var_get("arr") {
            assert!(matches!(a[1], Value::Int(7)));
            assert!(matches!(a[0], Value::Int(10))); // others untouched
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn writeback_map_key() {
        let mut e = prep_funcs("fn set_to(a: &mut int) { a = 7 }");
        let mut m = HashMap::new();
        m.insert("score".to_string(), Value::Int(0));
        e.var_set("m", Value::Map(m));
        let target = Expression::Index {
            object: Box::new(var("m")),
            index: Box::new(Expression::StringLiteral("score".to_string())),
        };
        e.call_function("set_to", &[mut_ref(target)]);
        if let Some(Value::Map(m)) = e.var_get("m") {
            assert!(matches!(m.get("score"), Some(Value::Int(7))));
        } else {
            panic!("expected map");
        }
    }

    #[test]
    fn fault_to_value_shape() {
        let f = RuntimeFault {
            code: "E0001".to_string(),
            message: "boom".to_string(),
            help: "fix it".to_string(),
        };
        match fault_to_value(&f) {
            Value::Map(m) => {
                assert!(matches!(m.get("error"), Some(Value::Bool(true))));
                assert!(matches!(m.get("code"), Some(Value::Str(s)) if s == "E0001"));
                assert!(matches!(m.get("message"), Some(Value::Str(s)) if s == "boom"));
                // `help` is intentionally not surfaced in the recover map.
                assert!(m.get("help").is_none());
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn writeback_struct_field() {
        let mut e = prep_funcs("fn set_to(a: &mut int) { a = 100 }");
        let mut fields = HashMap::new();
        fields.insert("x".to_string(), Value::Int(0));
        fields.insert("y".to_string(), Value::Int(0));
        e.var_set("p", Value::Object("Point".to_string(), fields));
        let target = Expression::FieldAccess {
            object: Box::new(var("p")),
            field: "x".to_string(),
        };
        e.call_function("set_to", &[mut_ref(target)]);
        if let Some(Value::Object(_, fields)) = e.var_get("p") {
            assert!(matches!(fields.get("x"), Some(Value::Int(100))));
            assert!(matches!(fields.get("y"), Some(Value::Int(0)))); // untouched
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn writeback_via_mut_keyword_param() {
        // The `mut x` keyword spelling is also honored for write-back.
        let mut e = prep_funcs("fn bump(mut x: int) { x = x + 1 }");
        e.var_set("n", Value::Int(41));
        e.call_function("bump", &[mut_ref(var("n"))]);
        assert!(matches!(e.var_get("n"), Some(Value::Int(42))));
    }

    // --- Frontend build hook (R5.2) ---------------------------------------
    // The decision logic that gates serving: a build command that exits
    // non-zero must fail (caller emits E0404 and aborts serving), while a
    // successful command must let serving proceed. Tests use the shell
    // builtins `true`/`false` so they stay std-only and have no side effects.

    #[test]
    #[cfg(unix)]
    fn test_frontend_build_success_proceeds() {
        // A command that exits 0 → Ok(()) → serving may proceed.
        assert!(run_frontend_build("true").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_frontend_build_nonzero_aborts() {
        // A command that exits non-zero → Err(()) → E0404 path, no serving.
        assert!(run_frontend_build("false").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_frontend_build_failing_pipeline_aborts() {
        // A full command line whose final status is non-zero must fail, so
        // stale assets are never served.
        assert!(run_frontend_build("exit 3").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn test_frontend_build_spawn_failure_aborts() {
        // A command that cannot resolve to a runnable program exits non-zero
        // through the shell, which must be treated as a failed build.
        assert!(run_frontend_build("this_command_should_not_exist_42").is_err());
    }

    #[test]
    fn test_frontend_build_empty_is_noop() {
        // An empty/whitespace command is a no-op success: nothing to build.
        assert!(run_frontend_build("").is_ok());
        assert!(run_frontend_build("   ").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn test_frontend_build_observable_side_effect() {
        // End-to-end check that the command actually runs to completion: it
        // writes a marker file under .tmp_tests/, which we then observe.
        let dir = std::path::Path::new(".tmp_tests");
        let _ = std::fs::create_dir_all(dir);
        let marker = dir.join("frontend_build_marker.txt");
        let _ = std::fs::remove_file(&marker);
        let cmd = format!("printf built > {}", marker.display());
        assert!(run_frontend_build(&cmd).is_ok());
        let contents = std::fs::read_to_string(&marker).unwrap_or_default();
        assert_eq!(contents, "built");
        let _ = std::fs::remove_file(&marker);
    }

    // --- Runtime memory model: exactly-once release, no leaks (R10.4/R10.5) --
    //
    // This is a tree-walking interpreter: every `Value` is owned by the scope
    // `HashMap` that holds its binding, and value memory is released by the
    // implementation runtime's automatic destruction when the owning frame goes
    // away — never by explicit deallocation. The guarantees we can verify
    // directly are: (a) the scope stack is leak-free (every pushed frame is
    // popped, on normal *and* early-exit paths), so no value is stranded in a
    // dangling frame; and (b) the frame-drop mechanism the runtime relies on
    // releases each owned value exactly once (no double-free, no leak).

    /// Parse a source snippet into statements for driving the interpreter.
    fn parse_stmts(src: &str) -> Vec<Stmt> {
        let tokens = crate::frontend::lexer::tokenize(src);
        crate::frontend::parser::parse(tokens).statements
    }

    fn depth(e: &Environment) -> usize {
        e.frames.len()
    }

    #[test]
    fn test_block_scope_returns_to_baseline() {
        // A block runs in its own frame; after it ends the stack is back to its
        // prior depth and block-locals do not leak out (their values were
        // released with the frame).
        let mut e = env();
        assert_eq!(depth(&e), 0);
        let body = parse_stmts("let x = 1\nlet y = [1, 2, 3]\nlet s = \"hello\"");
        let flow = e.exec_block(&body);
        assert!(matches!(flow, Flow::Normal));
        assert_eq!(depth(&e), 0, "block frame must be popped");
        assert!(e.var_get("x").is_none(), "block-local must not leak out");
        assert!(e.var_get("y").is_none());
    }

    #[test]
    fn test_block_scope_popped_on_early_return() {
        // An early `return` inside a block must still pop the frame before the
        // signal propagates — otherwise a frame (and its values) would leak.
        let mut e = env();
        let body = parse_stmts("let big = [1, 2, 3, 4, 5]\nreturn big");
        let flow = e.exec_block(&body);
        assert!(matches!(flow, Flow::Return(_)));
        assert_eq!(depth(&e), 0, "frame must be popped even on early return");
    }

    #[test]
    fn test_for_loop_scopes_return_to_baseline() {
        // Each iteration gets a fresh frame holding the loop var + block-locals;
        // all of them are popped, so depth is unchanged after the loop and
        // nothing accumulates across iterations.
        let mut e = env();
        let stmts = parse_stmts("for i in [1, 2, 3] { let acc = [i, i] }");
        for s in &stmts {
            e.exec_statement(s);
        }
        assert_eq!(depth(&e), 0, "no per-iteration frame may leak");
        assert!(e.var_get("acc").is_none());
        assert!(e.var_get("i").is_none());
    }

    #[test]
    fn test_for_loop_scope_popped_on_break() {
        // A `break` mid-iteration must release that iteration's frame.
        let mut e = env();
        let stmts = parse_stmts("for i in [1, 2, 3] { let tmp = i\nbreak }");
        for s in &stmts {
            e.exec_statement(s);
        }
        assert_eq!(depth(&e), 0, "iteration frame must be popped on break");
    }

    // --- break/continue semantics (R8.3, R8.4) ------------------------------

    /// Run a top-level snippet, then return the integer value bound to `name`.
    fn run_and_get_int(src: &str, name: &str) -> i64 {
        let mut e = env();
        for s in &parse_stmts(src) {
            e.exec_statement(s);
        }
        match e.var_get(name) {
            Some(Value::Int(n)) => n,
            other => panic!("expected Int binding for `{name}`, got {other:?}"),
        }
    }

    #[test]
    fn test_break_stops_loop_early() {
        // `break` halts the innermost loop: only 1 + 2 accumulate before i == 3.
        let n = run_and_get_int(
            "let mut sum = 0\n\
             for i in [1, 2, 3, 4, 5] { if i == 3 { break }\nsum = sum + i }",
            "sum",
        );
        assert_eq!(n, 3, "break must stop the loop at i == 3 (sum of 1 + 2)");
    }

    #[test]
    fn test_break_stops_while_loop_early() {
        // `break` also halts a `while` loop mid-condition.
        let n = run_and_get_int(
            "let mut n = 0\n\
             while n < 100 { n = n + 1\nif n == 4 { break } }",
            "n",
        );
        assert_eq!(n, 4, "break must stop the while loop when n reaches 4");
    }

    #[test]
    fn test_continue_skips_iterations() {
        // `continue` skips the rest of the body but advances the loop: sum the
        // odd values in range(6) -> 1 + 3 + 5 = 9.
        let n = run_and_get_int(
            "let mut odds = 0\n\
             for i in range(6) { if i % 2 == 0 { continue }\nodds = odds + i }",
            "odds",
        );
        assert_eq!(n, 9, "continue must skip evens, summing 1 + 3 + 5");
    }

    #[test]
    fn test_continue_in_while_still_advances() {
        // A `continue` in a `while` must re-check the condition (the body itself
        // advances `n` before the continue), so the loop terminates: skip n == 2,
        // summing 1 + 3 + 4 + 5 = 13 across n in 1..=5.
        let n = run_and_get_int(
            "let mut n = 0\nlet mut wsum = 0\n\
             while n < 5 { n = n + 1\nif n == 2 { continue }\nwsum = wsum + n }",
            "wsum",
        );
        assert_eq!(n, 13, "continue in while must still advance and terminate");
    }

    #[test]
    fn test_nested_loop_break_only_breaks_inner() {
        // `break` in the inner loop must not escape the outer loop: for each of
        // the 3 outer iterations the inner loop runs once (b == 1) then breaks,
        // so pairs == 3.
        let n = run_and_get_int(
            "let mut pairs = 0\n\
             for a in [1, 2, 3] { for b in [1, 2, 3] { if b == 2 { break }\npairs = pairs + 1 } }",
            "pairs",
        );
        assert_eq!(n, 3, "inner break must not terminate the outer loop");
    }

    #[test]
    fn test_continue_inside_if_inside_loop_bubbles_up() {
        // `continue` nested inside an `if` block inside the loop must bubble the
        // Flow signal up through the block scope: skip multiples of 3 in
        // range(10) -> sum of {1,2,4,5,7,8} = 27.
        let n = run_and_get_int(
            "let mut s = 0\n\
             for i in range(10) { if i % 3 == 0 { continue }\ns = s + i }",
            "s",
        );
        assert_eq!(n, 27, "continue inside a nested if must skip the iteration");
    }

    #[test]
    fn test_while_loop_scopes_return_to_baseline() {
        let mut e = env();
        let stmts = parse_stmts(
            "let mut n = 3\nwhile n > 0 { let chunk = [n, n, n]\nn = n - 1 }",
        );
        for s in &stmts {
            e.exec_statement(s);
        }
        assert_eq!(depth(&e), 0, "while-body frames must all be popped");
        assert!(e.var_get("chunk").is_none());
    }

    #[test]
    fn test_function_frame_restores_scope_stack() {
        // A function frame swaps in globals + a parameter scope and swaps the
        // caller's stack back on return: depth and caller locals are unchanged.
        let mut e = env();
        e.scope_push();
        e.var_set_local("caller_local", Value::Int(7));
        let before = depth(&e);
        let body = parse_stmts("let local = [1, 2, 3]\nreturn x");
        let ret = e.run_function_frame(&body, vec![("x".to_string(), Value::Int(42))]);
        assert!(matches!(ret, Value::Int(42)));
        assert_eq!(depth(&e), before, "caller stack must be restored exactly");
        assert!(matches!(e.var_get("caller_local"), Some(Value::Int(7))));
        // The callee's locals/params never leak into the caller.
        assert!(e.var_get("local").is_none());
        assert!(e.var_get("x").is_none());
    }

    #[test]
    fn test_nested_blocks_balance_scope_stack() {
        // Deeply nested blocks must each pop their frame.
        let mut e = env();
        let body = parse_stmts(
            "let a = 1\nif a > 0 { let b = 2\nif b > 0 { let c = [1, 2, 3] } }",
        );
        let _ = e.exec_block(&body);
        assert_eq!(depth(&e), 0, "all nested frames must be popped");
    }

    #[test]
    fn test_scope_frame_drop_releases_each_value_exactly_once() {
        // Validates the mechanism the runtime delegates release to: dropping a
        // scope frame map runs the destructor of every value it owns exactly
        // once — no double-free (count would be > N) and no leak (count < N).
        use std::cell::Cell;
        use std::rc::Rc;

        struct DropProbe(Rc<Cell<usize>>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let releases = Rc::new(Cell::new(0usize));
        {
            // A frame owning several bindings, mirroring a scope `HashMap`.
            let mut frame: HashMap<String, DropProbe> = HashMap::new();
            frame.insert("a".to_string(), DropProbe(releases.clone()));
            frame.insert("b".to_string(), DropProbe(releases.clone()));
            frame.insert("c".to_string(), DropProbe(releases.clone()));
            assert_eq!(releases.get(), 0, "nothing released while frame is live");
            drop(frame); // mirrors scope_pop removing a frame
        }
        assert_eq!(
            releases.get(),
            3,
            "each owned value released exactly once (no double-free, no leak)"
        );
    }
}

// ============================================================================
// Closures / lambdas — first-class function values (R8.1, R8.2, task 10.1).
// ============================================================================
#[cfg(test)]
mod closure_tests {
    use super::*;

    /// Parse `src`, register its free functions (mirroring `execute`), and run
    /// `main`'s body, returning the value `main` produces. Used to drive the
    /// end-to-end closure behaviors through the real parser + interpreter.
    fn run_main(src: &str) -> Value {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions
                    .insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                env.fn_mut
                    .insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
        }
        let main_body = env.functions.get("main").cloned().expect("main present");
        env.run_function_frame(&main_body[..], Vec::new())
    }

    /// R8.1/R8.2: a closure reads a variable from the scope where it was
    /// created, even though the call happens through a separate binding.
    #[test]
    fn closure_captures_enclosing_variable() {
        let v = run_main(
            "fn main() { let base = 10\n let add = fn(n) { return n + base }\n return add(5) }",
        );
        assert!(matches!(v, Value::Int(15)), "expected 15, got {:?}", v);
    }

    /// R8.1/R8.2: a closure can be passed as an argument and invoked by the
    /// callee through its parameter binding.
    #[test]
    fn closure_passed_as_argument_and_invoked() {
        let v = run_main(
            "fn apply(g, n) { return g(n) }\n\
             fn main() { return apply(fn(v) { return v * 3 }, 7) }",
        );
        assert!(matches!(v, Value::Int(21)), "expected 21, got {:?}", v);
    }

    /// R8.1/R8.2: a closure returned from a function keeps its captured
    /// environment alive after the defining call has returned.
    #[test]
    fn closure_returned_from_function_keeps_capture() {
        let v = run_main(
            "fn make_adder(x) { return fn(n) { return n + x } }\n\
             fn main() { let add5 = make_adder(5)\n return add5(100) }",
        );
        assert!(matches!(v, Value::Int(105)), "expected 105, got {:?}", v);
    }

    /// A parameter shadows a same-named captured binding inside the closure.
    #[test]
    fn closure_param_shadows_capture() {
        let v = run_main(
            "fn main() { let n = 99\n let f = fn(n) { return n }\n return f(1) }",
        );
        assert!(matches!(v, Value::Int(1)), "expected 1, got {:?}", v);
    }
}

// ============================================================================
// Property 7 — Tanpa kebocoran & pelepasan tepat-sekali (R10.4, R10.5).
// ============================================================================
#[cfg(test)]
mod memory_release_property {
    // Feature: enterprise-runtime-capabilities, Property 7: Tanpa kebocoran & pelepasan tepat-sekali
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};
    use std::cell::Cell;
    use std::rc::Rc;

    // ---- A well-typed, leak-free-by-construction program shape. -------------
    //
    // Each node models one scope frame: a `Block` (`if true { .. }`), a `Loop`
    // (`for _ in [..] { .. }`, instantiated once per iteration), or a `Function`
    // frame (a nested `fn` declared then immediately called). Every frame binds
    // `bindings` non-`Copy` values and may nest `children`. The grammar only
    // produces programs that bind, read, and drop — never move-after-use — so
    // every generated program is well-typed and leak-free by construction.

    #[derive(Clone, Debug)]
    enum FrameKind {
        Block,
        Loop { iters: usize },
        Function,
    }

    #[derive(Clone, Debug)]
    struct Shape {
        kind: FrameKind,
        bindings: usize,
        children: Vec<Shape>,
    }

    fn gen_shape(rng: &mut Rng, depth_left: usize) -> Shape {
        let kind = match rng.below(3) {
            0 => FrameKind::Block,
            1 => FrameKind::Loop { iters: rng.upto(2) }, // 0..=2 iterations
            _ => FrameKind::Function,
        };
        let bindings = rng.upto(3); // 0..=3 bindings per frame
        let mut children = Vec::new();
        if depth_left > 0 {
            let n = rng.upto(2); // up to 2 nested children per frame
            for _ in 0..n {
                children.push(gen_shape(rng, depth_left - 1));
            }
        }
        Shape { kind, bindings, children }
    }

    fn shape_gen() -> Gen<Shape> {
        Gen::new(
            |rng, size| {
                // Depth grows with the size hint: 1..=4 nested levels.
                let max_depth = 1 + (size % 4);
                gen_shape(rng, max_depth)
            },
            |_| Vec::new(),
        )
    }

    // ---- Oracle A: drive the REAL interpreter; assert the scope stack returns
    //      to baseline depth for every shape (no scope frame leaked). ----------

    fn at() -> Span {
        Span::new(1, 1)
    }
    fn stmt(kind: Statement) -> Stmt {
        Stmt::new(kind, at())
    }

    /// A non-`Copy` initializer (str or array) — value-semantics, owned by its
    /// frame and released when the frame is dropped.
    fn binding_value(i: usize) -> Expression {
        if i % 2 == 0 {
            Expression::StringLiteral(format!("v{}", i))
        } else {
            Expression::Array(vec![
                Expression::IntLiteral(i as i64),
                Expression::IntLiteral(i as i64 + 1),
            ])
        }
    }

    /// Statements for a frame's *contents*: its bindings followed by its nested
    /// child frames (each lowered to a scope-introducing statement).
    fn frame_body(shape: &Shape, ctr: &mut usize) -> Vec<Stmt> {
        let mut out = Vec::new();
        for i in 0..shape.bindings {
            *ctr += 1;
            out.push(stmt(Statement::VarDecl {
                name: format!("b{}", *ctr),
                mutable: false,
                type_annotation: None,
                value: binding_value(i),
            }));
        }
        for child in &shape.children {
            out.push(child_stmt(child, ctr));
        }
        out
    }

    /// Lower a child frame into a statement that introduces its own scope:
    /// `Block`/`Function` → `if true { .. }` (the function additionally declares
    /// and calls a nested `fn`), `Loop` → `for _ in [0..iters] { .. }`.
    fn child_stmt(shape: &Shape, ctr: &mut usize) -> Stmt {
        match &shape.kind {
            FrameKind::Block => stmt(Statement::If {
                condition: Expression::BoolLiteral(true),
                then_body: frame_body(shape, ctr),
                else_body: None,
            }),
            FrameKind::Loop { iters } => {
                *ctr += 1;
                let var = format!("i{}", *ctr);
                let body = frame_body(shape, ctr);
                let arr = Expression::Array(
                    (0..*iters).map(|n| Expression::IntLiteral(n as i64)).collect(),
                );
                stmt(Statement::For { variable: var, iterable: arr, body })
            }
            FrameKind::Function => {
                *ctr += 1;
                let fname = format!("f{}", *ctr);
                let body = frame_body(shape, ctr);
                // Declare a nested fn and immediately call it, so a real
                // function frame is pushed and restored within the parent scope.
                stmt(Statement::If {
                    condition: Expression::BoolLiteral(true),
                    then_body: vec![
                        stmt(Statement::FnDecl {
                            name: fname.clone(),
                            params: vec![],
                            return_type: None,
                            body,
                            is_pub: false,
                            is_async: false,
                        }),
                        stmt(Statement::Expr(Expression::FnCall {
                            callee: Box::new(Expression::Variable(fname)),
                            args: vec![],
                        })),
                    ],
                    else_body: None,
                })
            }
        }
    }

    /// Run the shape through the real interpreter and report whether the scope
    /// stack returned exactly to its baseline depth (i.e. every `scope_push`
    /// was paired with a `scope_pop` — no scope frame, and thus no owned value,
    /// was leaked).
    fn interpreter_returns_to_baseline(shape: &Shape) -> bool {
        let mut env = Environment::new();
        let baseline = env.frames.len();
        let mut ctr = 0usize;
        let body = frame_body(shape, &mut ctr);
        for s in &body {
            env.exec_statement(s);
        }
        env.frames.len() == baseline
    }

    // ---- Oracle B: a counting "allocator" via `Drop`-probes mirroring the same
    //      scope shape; assert release_count == alloc_count and leaked == 0. ----

    struct DropProbe(Rc<Cell<usize>>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    /// Mirror the generated scope discipline with real frame `HashMap`s whose
    /// entries are `Drop`-counting probes. Pushing a frame allocates one probe
    /// per binding; dropping the frame (mirroring `scope_pop`) releases each
    /// owned probe exactly once. A `Loop` re-instantiates its frame once per
    /// iteration, exactly as the interpreter pushes a fresh frame per iteration.
    fn model_alloc_release(shape: &Shape, alloc: &Rc<Cell<usize>>, release: &Rc<Cell<usize>>) {
        let reps = match &shape.kind {
            FrameKind::Loop { iters } => *iters,
            _ => 1,
        };
        for _ in 0..reps {
            let mut frame: HashMap<String, DropProbe> = HashMap::new();
            for i in 0..shape.bindings {
                alloc.set(alloc.get() + 1);
                frame.insert(format!("b{}", i), DropProbe(release.clone()));
            }
            for child in &shape.children {
                model_alloc_release(child, alloc, release);
            }
            drop(frame); // mirrors scope_pop: releases every owned probe exactly once
        }
    }

    /// Property 7: for every well-typed program shape, releasing scope frames
    /// leaves the scope stack at baseline (no leaked frame) AND every allocated
    /// value is released exactly once with nothing left alive (no leak,
    /// `free_count == alloc_count`).
    ///
    /// Validates: Requirements 10.4, 10.5
    #[test]
    fn prop_no_leak_exactly_once_release() {
        pbt::for_all(
            "P7 no-leak & exactly-once release",
            &shape_gen(),
            |shape: &Shape| {
                // Oracle A — real interpreter scope stack returns to baseline.
                if !interpreter_returns_to_baseline(shape) {
                    return false;
                }
                // Oracle B — counting allocator: exactly-once release, zero leak.
                let alloc = Rc::new(Cell::new(0usize));
                let release = Rc::new(Cell::new(0usize));
                model_alloc_release(shape, &alloc, &release);
                let alloc_count = alloc.get();
                let free_count = release.get();
                let leaked = alloc_count - free_count;
                leaked == 0 && free_count == alloc_count
            },
        );
    }
}

// ============================================================================
// Property 9 — Write-back `&mut` teramati pemanggil (R11.6).
// ============================================================================
#[cfg(test)]
mod writeback_property {
    // Feature: enterprise-runtime-capabilities, Property 9: Write-back &mut teramati pemanggil
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    // A generated case: a caller value living at one of four lvalue kinds, a
    // `&mut` mutation applied to it through a real `fn mutate(x: &mut int)`
    // call, and the sibling slots that must remain untouched. The oracle is
    // model-based: we independently apply the same mutation to a copy of the
    // target lvalue and assert the runtime's caller binding matches.

    #[derive(Clone, Debug)]
    enum LvalueKind {
        /// plain variable: `v`
        Var,
        /// array element: `arr[i]`
        ArrayElem,
        /// map key: `m["target"]`
        MapKey,
        /// struct field: `p.target`
        StructField,
    }

    #[derive(Clone, Debug)]
    enum Mutation {
        /// `x = <c>` — overwrite with a constant.
        SetConst(i64),
        /// `x = x + <d>` (rendered as `x - |d|` when negative).
        Add(i64),
    }

    #[derive(Clone, Debug)]
    struct Case {
        kind: LvalueKind,
        /// value at the target lvalue before the call
        initial: i64,
        mutation: Mutation,
        /// values occupying the *other* slots (array elements / map keys /
        /// struct fields); must be observed unchanged after write-back
        siblings: Vec<i64>,
        /// for `ArrayElem`: which index (0..=siblings.len()) holds the target
        target_index: usize,
    }

    /// Apply the mutation the way the model expects: a pure function of the
    /// initial value, independent of the interpreter.
    fn model_apply(initial: i64, m: &Mutation) -> i64 {
        match m {
            Mutation::SetConst(c) => *c,
            Mutation::Add(d) => initial + d,
        }
    }

    /// Render an i64 as Ran source without relying on negative literals:
    /// non-negative → `N`, negative → `0 - N`.
    fn render_int(v: i64) -> String {
        if v >= 0 {
            v.to_string()
        } else {
            format!("0 - {}", -v)
        }
    }

    /// Source for `fn mutate(x: &mut int) { x = <expr> }` realizing `mutation`.
    fn mutate_src(m: &Mutation) -> String {
        let rhs = match m {
            Mutation::SetConst(c) => render_int(*c),
            Mutation::Add(d) => {
                if *d >= 0 {
                    format!("x + {}", d)
                } else {
                    format!("x - {}", -d)
                }
            }
        };
        format!("fn mutate(x: &mut int) {{ x = {} }}", rhs)
    }

    /// Register the source's free functions into a fresh `Environment`,
    /// mirroring what `execute` does (and the existing write-back unit tests).
    fn prep(src: &str) -> Environment {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                env.fn_mut
                    .insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
        }
        env
    }

    fn var_expr(name: &str) -> Expression {
        Expression::Variable(name.to_string())
    }

    fn mut_ref(inner: Expression) -> Expression {
        Expression::UnaryOp {
            op: UnaryOperator::MutRef,
            operand: Box::new(inner),
        }
    }

    // ---- Generator ---------------------------------------------------------

    fn case_gen() -> Gen<Case> {
        Gen::new(
            |rng: &mut Rng, _size| {
                // Bounded magnitudes keep arithmetic comfortably in range and
                // make negative-source rendering (`0 - N`, `x - N`) total.
                let bound = 1_000_000_000i64;
                let initial = rng.range_i64(-bound, bound);
                let kind = match rng.below(4) {
                    0 => LvalueKind::Var,
                    1 => LvalueKind::ArrayElem,
                    2 => LvalueKind::MapKey,
                    _ => LvalueKind::StructField,
                };
                let mutation = if rng.boolean() {
                    Mutation::SetConst(rng.range_i64(-bound, bound))
                } else {
                    Mutation::Add(rng.range_i64(-bound, bound))
                };
                let n_siblings = rng.upto(3); // 0..=3 other slots
                let siblings: Vec<i64> =
                    (0..n_siblings).map(|_| rng.range_i64(-bound, bound)).collect();
                // Target index for ArrayElem may be any slot in [0, n_siblings].
                let target_index = rng.upto(n_siblings);
                Case { kind, initial, mutation, siblings, target_index }
            },
            // Structural shrinking: fewer siblings, then simpler scalars.
            |c: &Case| {
                let mut out = Vec::new();
                if !c.siblings.is_empty() {
                    let mut fewer = c.clone();
                    fewer.siblings.pop();
                    fewer.target_index = fewer.target_index.min(fewer.siblings.len());
                    out.push(fewer);
                }
                if c.initial != 0 {
                    let mut z = c.clone();
                    z.initial = 0;
                    out.push(z);
                }
                match &c.mutation {
                    Mutation::SetConst(v) if *v != 0 => {
                        let mut z = c.clone();
                        z.mutation = Mutation::SetConst(0);
                        out.push(z);
                    }
                    Mutation::Add(v) if *v != 0 => {
                        let mut z = c.clone();
                        z.mutation = Mutation::Add(0);
                        out.push(z);
                    }
                    _ => {}
                }
                out
            },
        )
    }

    // ---- Drive the real runtime + model-based oracle -----------------------

    /// Run one case: build the lvalue in the caller, call `mutate(&mut lvalue)`
    /// on the real interpreter, then check the caller's binding read-back
    /// against an independently computed model. Returns `true` iff the runtime
    /// state equals the model (target mutated, siblings untouched).
    fn writeback_matches_model(c: &Case) -> bool {
        let mut e = prep(&mutate_src(&c.mutation));
        let expected = model_apply(c.initial, &c.mutation);

        match &c.kind {
            LvalueKind::Var => {
                e.var_set("v", Value::Int(c.initial));
                e.call_function("mutate", &[mut_ref(var_expr("v"))]);
                matches!(e.var_get("v"), Some(Value::Int(n)) if n == expected)
            }
            LvalueKind::ArrayElem => {
                // Build an array with the target at `target_index` and siblings
                // filling the remaining slots, preserving order.
                let mut arr: Vec<Value> = Vec::with_capacity(c.siblings.len() + 1);
                let mut sib = c.siblings.iter();
                for i in 0..=c.siblings.len() {
                    if i == c.target_index {
                        arr.push(Value::Int(c.initial));
                    } else {
                        arr.push(Value::Int(*sib.next().unwrap()));
                    }
                }
                let model = {
                    let mut m = arr.clone();
                    m[c.target_index] = Value::Int(expected);
                    m
                };
                e.var_set("arr", Value::Array(arr));
                let target = Expression::Index {
                    object: Box::new(var_expr("arr")),
                    index: Box::new(Expression::IntLiteral(c.target_index as i64)),
                };
                e.call_function("mutate", &[mut_ref(target)]);
                match e.var_get("arr") {
                    Some(Value::Array(got)) => values_eq_ints(&got, &model),
                    _ => false,
                }
            }
            LvalueKind::MapKey => {
                let mut map = HashMap::new();
                map.insert("target".to_string(), Value::Int(c.initial));
                for (i, s) in c.siblings.iter().enumerate() {
                    map.insert(format!("s{}", i), Value::Int(*s));
                }
                // Model: same map with "target" mutated.
                let mut model = map.clone();
                model.insert("target".to_string(), Value::Int(expected));
                e.var_set("m", Value::Map(map));
                let target = Expression::Index {
                    object: Box::new(var_expr("m")),
                    index: Box::new(Expression::StringLiteral("target".to_string())),
                };
                e.call_function("mutate", &[mut_ref(target)]);
                match e.var_get("m") {
                    Some(Value::Map(got)) => map_eq_ints(&got, &model),
                    _ => false,
                }
            }
            LvalueKind::StructField => {
                let mut fields = HashMap::new();
                fields.insert("target".to_string(), Value::Int(c.initial));
                for (i, s) in c.siblings.iter().enumerate() {
                    fields.insert(format!("f{}", i), Value::Int(*s));
                }
                let mut model = fields.clone();
                model.insert("target".to_string(), Value::Int(expected));
                e.var_set("p", Value::Object("S".to_string(), fields));
                let target = Expression::FieldAccess {
                    object: Box::new(var_expr("p")),
                    field: "target".to_string(),
                };
                e.call_function("mutate", &[mut_ref(target)]);
                match e.var_get("p") {
                    Some(Value::Object(_, got)) => map_eq_ints(&got, &model),
                    _ => false,
                }
            }
        }
    }

    /// Compare two int-valued arrays elementwise.
    fn values_eq_ints(a: &[Value], b: &[Value]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b.iter()).all(|(x, y)| match (x, y) {
                (Value::Int(p), Value::Int(q)) => p == q,
                _ => false,
            })
    }

    /// Compare two int-valued maps key-for-key.
    fn map_eq_ints(a: &HashMap<String, Value>, b: &HashMap<String, Value>) -> bool {
        a.len() == b.len()
            && b.iter().all(|(k, v)| match (a.get(k), v) {
                (Some(Value::Int(p)), Value::Int(q)) => p == q,
                _ => false,
            })
    }

    /// Property 9: for every `&mut` mutation applied through a real function
    /// call against any of the four lvalue kinds (variable, `arr[i]`, `map[k]`,
    /// `obj.field`), the caller's binding after the call equals the result of
    /// applying that same mutation directly to a model copy of the lvalue —
    /// and sibling slots are left untouched.
    ///
    /// Validates: Requirement 11.6
    #[test]
    fn prop_writeback_mut_observed_by_caller() {
        pbt::for_all(
            "P9 write-back &mut teramati pemanggil",
            &case_gen(),
            writeback_matches_model,
        );
    }
}

// ============================================================================
// Property 2 — Call-depth restored after every return (incl. fault) (R1.1, R1.7).
// ============================================================================
#[cfg(test)]
mod call_depth_restoration_property {
    // Feature: memory-safe-self-hosting, Property 2: Call-depth restored after every return (incl. fault)
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// One call to perform against a fresh environment. Every variant drives a
    /// real Ran function through `run_function_frame` (the single call boundary
    /// where the `DepthGuard` lives), so the tracked depth must come back to its
    /// pre-call value whether the call returns normally or unwinds via a fault.
    #[derive(Clone, Debug)]
    enum Call {
        /// Bounded recursion that returns normally (`depth` < limit).
        Normal(i64),
        /// Recurse `depth` deep, then divide by zero at the bottom → `E1011`.
        FaultDiv(i64),
        /// Recurse `depth` deep, then index out of bounds at the bottom → `E1012`.
        FaultIdx(i64),
    }

    /// Source registering the three helper functions. `fdiv`/`fidx` take the
    /// zero divisor / bad index as a *runtime argument* so nothing can be
    /// constant-folded away before evaluation.
    const SRC: &str = "\
fn fnorm(n: int) -> int {
    if n <= 0 { return 0 }
    return fnorm(n - 1)
}
fn fdiv(n: int, z: int) -> int {
    if n <= 0 { return 1 / z }
    return fdiv(n - 1, z)
}
fn fidx(n: int, i: int) -> int {
    if n <= 0 {
        let a = [10, 20, 30]
        return a[i]
    }
    return fidx(n - 1, i)
}
";

    /// Parse `SRC` and register its free functions into a fresh `Environment`,
    /// mirroring what `execute` does (and the existing write-back/recursion
    /// tests).
    fn prep() -> Environment {
        let tokens = crate::frontend::lexer::tokenize(SRC);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions
                    .insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
            }
        }
        env
    }

    fn call_gen() -> Gen<Vec<Call>> {
        Gen::new(
            |rng: &mut Rng, _size| {
                // 1..=5 calls per case; small recursion depths keep the run fast
                // and well below the default 10_000 limit (no SIGSEGV risk, and
                // no need to touch the process-global `MAX_CALL_DEPTH`).
                let n = 1 + rng.upto(4);
                (0..n)
                    .map(|_| {
                        let depth = rng.upto(40) as i64; // 0..=40
                        match rng.below(3) {
                            0 => Call::Normal(depth),
                            1 => Call::FaultDiv(depth),
                            _ => Call::FaultIdx(depth),
                        }
                    })
                    .collect()
            },
            // Structural shrink: fewer calls.
            |calls: &Vec<Call>| {
                if calls.len() <= 1 {
                    Vec::new()
                } else {
                    vec![calls[..calls.len() - 1].to_vec(), calls[1..].to_vec()]
                }
            },
        )
    }

    /// Read the current thread's tracked Ran call depth.
    fn depth() -> usize {
        CALL_DEPTH.with(|d| d.get())
    }

    /// Run one call on a fresh environment and report whether the tracked depth
    /// returned to its pre-call value (and whether the call's outcome matched
    /// the expected normal-return / fault classification).
    fn one_call_restores_depth(call: &Call) -> bool {
        let mut env = prep();
        let before = depth();
        let outcome = match call {
            Call::Normal(d) => {
                let args = [Expression::IntLiteral(*d)];
                catch_fault(move || {
                    env.call_function("fnorm", &args);
                })
                .is_ok() // normal return → Ok
            }
            Call::FaultDiv(d) => {
                let args = [Expression::IntLiteral(*d), Expression::IntLiteral(0)];
                match catch_fault(move || {
                    env.call_function("fdiv", &args);
                }) {
                    Err(f) => f.code == "E1011", // divide-by-zero fault
                    Ok(()) => false,
                }
            }
            Call::FaultIdx(d) => {
                let args = [Expression::IntLiteral(*d), Expression::IntLiteral(99)];
                match catch_fault(move || {
                    env.call_function("fidx", &args);
                }) {
                    Err(f) => f.code == "E1012", // index-out-of-bounds fault
                    Ok(()) => false,
                }
            }
        };
        let after = depth();
        // R1.7: depth restored to baseline on BOTH the normal-return and the
        // fault-unwind paths; R1.1: depth is tracked per execution thread.
        outcome && after == before
    }

    /// Property 2: for any sequence of Ran function calls — normal returns and
    /// faulting calls alike — the tracked call depth after each call equals the
    /// depth before it (the `DepthGuard` restores the counter on normal return
    /// and on fault unwind).
    ///
    /// Validates: Requirements 1.1, 1.7
    #[test]
    fn prop_call_depth_restored_after_every_return() {
        // Serialize against the other tests that mutate the process-global
        // `MAX_CALL_DEPTH` (the recursion-guard tests/property in
        // `runtime/frame.rs`). Without this, a concurrent test that lowers the
        // limit could make a generated call sequence here fault spuriously,
        // making this test flaky under parallel `cargo test`. Poison is
        // recovered so one failing test does not cascade into the others.
        let _serialize = super::frame::recursion_guard_tests::DEPTH_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Pin a known, generous limit for the duration so depth restoration is
        // measured independently of whatever limit another test may have set,
        // then restore the previous value when the test ends.
        let saved_max = current_max_depth();
        set_max_call_depth(10_000);

        // Baseline must be clean on this thread before we start measuring.
        CALL_DEPTH.with(|d| d.set(0));
        pbt::for_all(
            "P2 call-depth restored after every return (incl. fault)",
            &call_gen(),
            |calls: &Vec<Call>| {
                for call in calls {
                    if !one_call_restores_depth(call) {
                        return false;
                    }
                }
                // After the whole sequence the counter is back to baseline.
                depth() == 0
            },
        );

        // Restore the limit the rest of the suite expects.
        set_max_call_depth(saved_max);
    }
}

// ============================================================================
// Property 3 — Faults unwind without crashing; library code never exits
// outside whitelisted boundaries (R3.1, R3.4, R3.6, R4.3, R11.4).
// ============================================================================
#[cfg(test)]
mod fault_unwind_audit_property {
    // Feature: memory-safe-self-hosting, Property 3: Faults unwind without crashing; library code never exits outside boundaries
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};
    use std::panic::{self, AssertUnwindSafe};
    use std::path::{Path, PathBuf};

    /// A randomly generated recoverable fault (code + message + help).
    #[derive(Clone, Debug)]
    struct FaultSpec {
        code: String,
        message: String,
        help: String,
    }

    fn fault_spec_gen() -> Gen<FaultSpec> {
        let msg_gen = pbt::string(24);
        let help_gen = pbt::string(24);
        Gen::new(
            move |rng: &mut Rng, size| {
                // Mix the real Phase-A fault codes with arbitrary `E####` codes.
                const KNOWN: &[&str] =
                    &["E1006", "E1007", "E1010", "E1011", "E1012", "E0511", "E1013"];
                let code = if rng.below(2) == 0 {
                    (*rng.choose(KNOWN)).to_string()
                } else {
                    format!("E{:04}", rng.below(10_000))
                };
                FaultSpec {
                    code,
                    message: msg_gen.generate(rng, size),
                    help: help_gen.generate(rng, size),
                }
            },
            |_| Vec::new(),
        )
    }

    /// Recursively collect every `.rs` file under `dir`.
    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    collect_rs(&p, out);
                } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                    out.push(p);
                }
            }
        }
    }

    /// Count occurrences of `needle` in `content` that are NOT inside a `//`
    /// line comment. Used by the static exit audit so doc/comment mentions do
    /// not count as real exit-call sites.
    fn count_code_occurrences(content: &str, needle: &str) -> usize {
        let mut total = 0usize;
        for line in content.lines() {
            let comment = line.find("//");
            let mut start = 0usize;
            while let Some(pos) = line[start..].find(needle) {
                let abs = start + pos;
                if comment.map_or(true, |c| abs < c) {
                    total += 1;
                }
                start = abs + needle.len();
            }
        }
        total
    }

    /// Property 3 (dynamic half): for any recoverable fault raised during
    /// evaluation, `catch_fault` returns `Err` carrying the same code/message/
    /// help (the process stays alive), while a genuine non-fault panic is
    /// propagated rather than swallowed.
    fn fault_unwinds_and_nonfault_propagates(spec: &FaultSpec) -> bool {
        // Recoverable fault: unwinds to the catch boundary, never exits.
        let caught = catch_fault(|| -> () {
            runtime_error(&spec.code, &spec.message, &spec.help)
        });
        let recovered = match caught {
            Err(f) => f.code == spec.code && f.message == spec.message && f.help == spec.help,
            Ok(()) => false,
        };

        // A non-`RuntimeFault` panic (a genuine bug) must be re-raised by
        // `catch_fault`, not hidden. We observe that by catching it one level up.
        let propagated = panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = catch_fault(|| panic::panic_any(String::from("genuine bug, not a fault")));
        }))
        .is_err();

        recovered && propagated
    }

    /// Property 3 (static half): scan the runtime/stdlib library sources and
    /// assert every real exit-call site lives in a whitelisted boundary. The
    /// search needle is assembled at runtime so this audit's own source does not
    /// contain the literal token and thus never counts itself.
    fn assert_library_is_exit_free() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        // Assemble "process::exit(" from fragments — the contiguous token never
        // appears in this file, so reading mod.rs back does not self-count.
        let needle = format!("{}{}{}", "process::", "exit", "(");

        // Whitelisted boundaries and the exact number of exit-call sites each is
        // allowed to contain (the classification fixed by tasks 2.1/2.2):
        //   runtime/mod.rs           : watchdog E1006 exit + top-level execute
        //                              exit-70 + the two `fn port` validation exits
        //   runtime/builtins.rs      : the user-requested `exit` builtin
        //   runtime/module_dispatch  : `os.exit` + `log.fatal`
        let whitelist: &[(&str, usize)] = &[
            ("runtime/mod.rs", 4),
            ("runtime/builtins.rs", 1),
            ("runtime/module_dispatch.rs", 2),
        ];

        let mut files = Vec::new();
        for sub in ["src/runtime", "src/stdlib"] {
            collect_rs(&Path::new(manifest).join(sub), &mut files);
        }
        assert!(
            !files.is_empty(),
            "exit audit found no library sources under src/runtime or src/stdlib"
        );

        for path in &files {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            let found = count_code_occurrences(&content, &needle);
            let allowed = whitelist
                .iter()
                .find(|(suffix, _)| path.ends_with(suffix))
                .map(|(_, n)| *n)
                .unwrap_or(0);
            assert_eq!(
                found, allowed,
                "library exit audit failed for {:?}: found {} exit-call site(s) but the \
                 whitelist allows {} — library code must report failures via RuntimeFault/\
                 Result, not by exiting outside a whitelisted boundary",
                path, found, allowed
            );
        }
    }

    /// Property 3: faults unwind to the nearest catch boundary without crashing
    /// the process (and non-fault panics propagate), AND no library source
    /// outside the whitelisted boundaries calls the process-exit primitive.
    ///
    /// Validates: Requirements 3.1, 3.4, 3.6, 4.3, 11.4
    #[test]
    fn prop_faults_unwind_and_library_exit_free() {
        // Dynamic half: many randomly generated faults all unwind cleanly.
        pbt::for_all(
            "P3 faults unwind without crashing",
            &fault_spec_gen(),
            fault_unwinds_and_nonfault_propagates,
        );
        // Static half: the library never exits outside a whitelisted boundary.
        assert_library_is_exit_free();
    }
}

// ============================================================================
// Property 5 — Fault-to-Ran-value carries code, message, error marker (R4.5).
// ============================================================================
#[cfg(test)]
mod fault_to_value_property {
    // Feature: memory-safe-self-hosting, Property 5: Fault-to-Ran-value carries code, message, and error marker
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    #[derive(Clone, Debug)]
    struct FaultSpec {
        code: String,
        message: String,
        help: String,
    }

    fn fault_spec_gen() -> Gen<FaultSpec> {
        let msg_gen = pbt::string(32);
        let help_gen = pbt::string(32);
        Gen::new(
            move |rng: &mut Rng, size| {
                const KNOWN: &[&str] =
                    &["E1006", "E1007", "E1010", "E1011", "E1012", "E0511", "E1013"];
                let code = if rng.below(2) == 0 {
                    (*rng.choose(KNOWN)).to_string()
                } else {
                    format!("E{:04}", rng.below(10_000))
                };
                FaultSpec {
                    code,
                    message: msg_gen.generate(rng, size),
                    help: help_gen.generate(rng, size),
                }
            },
            |_| Vec::new(),
        )
    }

    /// Property 5: converting any `RuntimeFault` into a Ran value yields a map
    /// carrying `error == true`, the same `code`, and the same `message`.
    ///
    /// Validates: Requirement 4.5
    #[test]
    fn prop_fault_to_value_carries_code_message_marker() {
        pbt::for_all(
            "P5 fault-to-Ran-value carries code/message/error marker",
            &fault_spec_gen(),
            |spec: &FaultSpec| {
                let fault = RuntimeFault {
                    code: spec.code.clone(),
                    message: spec.message.clone(),
                    help: spec.help.clone(),
                };
                match fault_to_value(&fault) {
                    Value::Map(m) => {
                        let marker = matches!(m.get("error"), Some(Value::Bool(true)));
                        let code_ok =
                            matches!(m.get("code"), Some(Value::Str(s)) if *s == spec.code);
                        let msg_ok = matches!(
                            m.get("message"),
                            Some(Value::Str(s)) if *s == spec.message
                        );
                        marker && code_ok && msg_ok
                    }
                    _ => false,
                }
            },
        );
    }
}

// ============================================================================
// Property 6 — Checked integer arithmetic never wraps (R7.1, R7.2).
// ============================================================================
#[cfg(test)]
mod checked_arithmetic_property {
    // Feature: memory-safe-self-hosting, Property 6: Checked integer arithmetic never wraps
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// The five arithmetic operators under test.
    #[derive(Clone, Debug)]
    struct ArithCase {
        a: i64,
        b: i64,
        op: u8, // 0=Add 1=Sub 2=Mul 3=Div 4=Mod
    }

    /// The exact, implementation-faithful expectation for one operation.
    #[derive(Debug)]
    enum Expect {
        Int(i64),
        Fault(&'static str),
    }

    fn rand_i64(rng: &mut Rng) -> i64 {
        const EDGES: &[i64] = &[
            0,
            1,
            -1,
            2,
            -2,
            i64::MAX,
            i64::MIN,
            i64::MAX - 1,
            i64::MIN + 1,
            i32::MAX as i64,
            i32::MIN as i64,
        ];
        if rng.below(3) == 0 {
            *rng.choose(EDGES)
        } else {
            rng.next_u64() as i64
        }
    }

    fn arith_gen() -> Gen<ArithCase> {
        Gen::new(
            |rng: &mut Rng, _size| {
                let a = rand_i64(rng);
                // Bias `b` toward zero sometimes so divide/modulo-by-zero is
                // exercised on a meaningful fraction of cases.
                let b = if rng.below(4) == 0 { 0 } else { rand_i64(rng) };
                let op = rng.below(5) as u8;
                ArithCase { a, b, op }
            },
            // Shrink the magnitudes toward zero while preserving the operator.
            |c: &ArithCase| {
                let mut out = Vec::new();
                if c.a != 0 {
                    out.push(ArithCase { a: 0, ..c.clone() });
                }
                if c.b != 0 {
                    out.push(ArithCase { b: 0, ..c.clone() });
                }
                out
            },
        )
    }

    /// Map an i128 result into either an exact in-range `i64` or an `E1010`
    /// overflow fault — exactly the `checked_*` contract in `eval_binary_op`.
    fn fit(x: i128) -> Expect {
        if x >= i64::MIN as i128 && x <= i64::MAX as i128 {
            Expect::Int(x as i64)
        } else {
            Expect::Fault("E1010")
        }
    }

    /// Implementation-faithful oracle: an i128 value oracle for `+ - *`, and the
    /// `checked_div`/`checked_rem` semantics (incl. the `i64::MIN op -1`
    /// overflow and divide/modulo-by-zero) for `/ %`.
    fn expected(c: &ArithCase) -> Expect {
        match c.op {
            0 => fit(c.a as i128 + c.b as i128),
            1 => fit(c.a as i128 - c.b as i128),
            2 => fit(c.a as i128 * c.b as i128),
            3 => {
                if c.b == 0 {
                    Expect::Fault("E1011")
                } else if c.a == i64::MIN && c.b == -1 {
                    Expect::Fault("E1010")
                } else {
                    Expect::Int(c.a / c.b)
                }
            }
            _ => {
                if c.b == 0 {
                    Expect::Fault("E1011")
                } else if c.a == i64::MIN && c.b == -1 {
                    Expect::Fault("E1010")
                } else {
                    Expect::Int(c.a % c.b)
                }
            }
        }
    }

    fn op_of(op: u8) -> BinaryOperator {
        match op {
            0 => BinaryOperator::Add,
            1 => BinaryOperator::Sub,
            2 => BinaryOperator::Mul,
            3 => BinaryOperator::Div,
            _ => BinaryOperator::Mod,
        }
    }

    /// Property 6: for any `i64` pair and any of `+ - * / %`, the runtime yields
    /// the exact mathematical value when it fits in `i64`, otherwise a recover-
    /// able fault (`E1010` overflow / `E1011` divide-or-modulo-by-zero) — never
    /// a wrapped or undefined value.
    ///
    /// Validates: Requirements 7.1, 7.2
    #[test]
    fn prop_checked_integer_arithmetic_never_wraps() {
        pbt::for_all(
            "P6 checked integer arithmetic never wraps",
            &arith_gen(),
            |c: &ArithCase| {
                let env = Environment::new();
                let op = op_of(c.op);
                let res = catch_fault(|| {
                    env.eval_binary_op(&Value::Int(c.a), &op, &Value::Int(c.b))
                });
                match (expected(c), res) {
                    (Expect::Int(n), Ok(Value::Int(m))) => m == n,
                    (Expect::Fault(code), Err(f)) => f.code == code,
                    _ => false,
                }
            },
        );
    }
}

// ============================================================================
// Property 7 — Index access is bounds-safe (R7.3).
// ============================================================================
#[cfg(test)]
mod index_bounds_property {
    // Feature: memory-safe-self-hosting, Property 7: Index access is bounds-safe
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// The container being indexed: an int array or a (possibly multi-byte)
    /// string. Both go through the same checked-index path in `eval_expression`.
    #[derive(Clone, Debug)]
    enum Container {
        Arr(Vec<i64>),
        Str(String),
    }

    #[derive(Clone, Debug)]
    struct IndexCase {
        container: Container,
        index: i64,
    }

    fn index_case_gen() -> Gen<IndexCase> {
        let str_gen = pbt::string(8);
        Gen::new(
            move |rng: &mut Rng, size| {
                let container = if rng.boolean() {
                    let len = rng.upto(8);
                    Container::Arr((0..len).map(|_| rng.range_i64(-1000, 1000)).collect())
                } else {
                    Container::Str(str_gen.generate(rng, size))
                };
                let len = match &container {
                    Container::Arr(a) => a.len(),
                    Container::Str(s) => s.chars().count(),
                } as i64;
                // Mostly near-range indices (incl. negative and just-past-end),
                // occasionally extreme values to probe the `as usize` cast.
                let index = match rng.below(6) {
                    0 => i64::MIN,
                    1 => i64::MAX,
                    _ => rng.range_i64(-(len + 3), len + 3),
                };
                IndexCase { container, index }
            },
            |_| Vec::new(),
        )
    }

    /// Property 7: indexing an array or string returns the i-th element when
    /// `0 <= i < len`, and otherwise raises a recoverable `E1012` fault whose
    /// message carries both the offending index and the length — never a
    /// host-crashing panic.
    ///
    /// Validates: Requirement 7.3
    #[test]
    fn prop_index_access_is_bounds_safe() {
        pbt::for_all(
            "P7 index access is bounds-safe",
            &index_case_gen(),
            |case: &IndexCase| {
                let mut env = Environment::new();
                let (value, len) = match &case.container {
                    Container::Arr(a) => (
                        Value::Array(a.iter().map(|n| Value::Int(*n)).collect()),
                        a.len() as i64,
                    ),
                    Container::Str(s) => (Value::Str(s.clone()), s.chars().count() as i64),
                };
                env.var_set("a", value);
                let expr = Expression::Index {
                    object: Box::new(Expression::Variable("a".to_string())),
                    index: Box::new(Expression::IntLiteral(case.index)),
                };
                let res = catch_fault(move || env.eval_expression(&expr));

                let in_range = case.index >= 0 && case.index < len;
                if in_range {
                    let i = case.index as usize;
                    match (&case.container, res) {
                        (Container::Arr(a), Ok(Value::Int(n))) => n == a[i],
                        (Container::Str(s), Ok(Value::Str(got))) => {
                            s.chars().nth(i).map(|c| c.to_string()) == Some(got)
                        }
                        _ => false,
                    }
                } else {
                    // Out of range (including negative): E1012 carrying index + length.
                    match res {
                        Err(f) => {
                            f.code == "E1012"
                                && f.message.contains(&case.index.to_string())
                                && f.message.contains(&len.to_string())
                        }
                        Ok(_) => false,
                    }
                }
            },
        );
    }
}

// ============================================================================
// `match`-arm `return` propagation (R8.5, task 10.3).
// ============================================================================
#[cfg(test)]
mod match_return_tests {
    use super::*;

    /// Parse `src`, register its free functions (mirroring `execute`), and run
    /// `main`'s body, returning the value `main` produces — exercising the real
    /// parser + interpreter end to end.
    fn run_main(src: &str) -> Value {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions
                    .insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                env.fn_mut
                    .insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
        }
        let main_body = env.functions.get("main").cloned().expect("main present");
        env.run_function_frame(&main_body[..], Vec::new())
    }

    /// R8.5: a `return` inside a `match` arm unwinds its value to the enclosing
    /// function, rather than being swallowed by the match and falling through.
    #[test]
    fn return_in_match_arm_unwinds_to_function() {
        let v = run_main(
            "fn classify(n) { match n { 1 => { return 100 } _ => { return 0 } } return -1 }\n\
             fn main() { return classify(1) }",
        );
        assert!(matches!(v, Value::Int(100)), "expected 100, got {:?}", v);
    }

    /// R8.5: the wildcard arm's `return` also unwinds, and the trailing
    /// statement after the match is never reached.
    #[test]
    fn return_in_wildcard_arm_unwinds() {
        let v = run_main(
            "fn classify(n) { match n { 1 => { return 100 } _ => { return 42 } } return -1 }\n\
             fn main() { return classify(9) }",
        );
        assert!(matches!(v, Value::Int(42)), "expected 42, got {:?}", v);
    }

    /// R8.5: only the matched arm's `return` fires; a later statement after the
    /// match still runs when no arm returns.
    #[test]
    fn match_without_return_falls_through_to_following_statement() {
        let v = run_main(
            "fn pick(n) { let acc = 0\n match n { 1 => { let _x = 5 } _ => { let _y = 6 } } return acc + 7 }\n\
             fn main() { return pick(1) }",
        );
        assert!(matches!(v, Value::Int(7)), "expected 7, got {:?}", v);
    }

    /// `match` used as an expression value (the no-return case) still yields the
    /// matched arm's last expression — the original behavior is preserved.
    #[test]
    fn match_as_expression_value_still_works() {
        let v = run_main(
            "fn label(n) { let r = match n { 1 => 11 _ => 22 }\n return r }\n\
             fn main() { return label(1) }",
        );
        assert!(matches!(v, Value::Int(11)), "expected 11, got {:?}", v);
    }

    /// `match` as an expression value through the wildcard arm.
    #[test]
    fn match_as_expression_value_wildcard() {
        let v = run_main(
            "fn label(n) { let r = match n { 1 => 11 _ => 22 }\n return r }\n\
             fn main() { return label(5) }",
        );
        assert!(matches!(v, Value::Int(22)), "expected 22, got {:?}", v);
    }

    /// R8.5: a `return` inside a `match` arm that sits inside a loop unwinds all
    /// the way out of the enclosing function (not just the loop).
    #[test]
    fn return_in_match_arm_inside_loop_unwinds_function() {
        let v = run_main(
            "fn scan() { for i in [1, 2, 3] { match i { 2 => { return 222 } _ => { let _k = 0 } } } return -1 }\n\
             fn main() { return scan() }",
        );
        assert!(matches!(v, Value::Int(222)), "expected 222, got {:?}", v);
    }
}

#[cfg(test)]
mod trait_dispatch_tests {
    //! R8.6: declaring a trait and implementing it for a type, then dispatching
    //! a method call to the implementation matching the receiver value's type.
    use super::*;

    /// Parse `src`, register declarations the way `execute` does (traits first,
    /// then functions/impls/enums), then run `main` and return its value. This
    /// exercises the real parser + interpreter end to end for trait dispatch.
    fn run_main_full(src: &str) -> Value {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();

        // Traits first so `impl Trait for Type` inherits default bodies.
        for stmt in &program.statements {
            if let Statement::TraitDecl { name, methods, .. } = &stmt.kind {
                env.register_trait(name, methods);
            }
        }
        for stmt in &program.statements {
            match &stmt.kind {
                Statement::FnDecl { name, params, body, .. } => {
                    env.functions.insert(name.clone(), std::sync::Arc::new(body.clone()));
                    env.fn_params
                        .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                    env.fn_mut
                        .insert(name.clone(), params.iter().map(param_is_mut).collect());
                }
                Statement::ImplBlock { type_name, trait_name, methods } => {
                    env.register_impl(type_name, trait_name.as_deref(), methods);
                }
                Statement::EnumDecl { name, variants, .. } => {
                    env.register_enum(name, variants);
                }
                _ => {}
            }
        }

        let main_body = env.functions.get("main").cloned().expect("main present");
        env.run_function_frame(&main_body[..], Vec::new())
    }

    /// R8.6: a method call on an instance dispatches to the `impl Trait for Type`
    /// implementation registered for that value's type.
    #[test]
    fn trait_impl_method_dispatches_to_type_implementation() {
        let v = run_main_full(
            "trait Speaker { fn speak(self) -> int }\n\
             struct Dog { age: int }\n\
             impl Speaker for Dog { fn speak(self) -> int { return 1 } }\n\
             fn main() { let d = Dog { age: 3 }\n return d.speak() }",
        );
        assert!(matches!(v, Value::Int(1)), "expected 1 from Dog impl, got {:?}", v);
    }

    /// R8.6: two types implementing the same trait each dispatch to their own
    /// implementation based on the receiver value's type.
    #[test]
    fn trait_dispatch_selects_receiver_type_implementation() {
        let src = "trait Speaker { fn sound(self) -> int }\n\
                   struct Dog { x: int }\n\
                   struct Cat { x: int }\n\
                   impl Speaker for Dog { fn sound(self) -> int { return 10 } }\n\
                   impl Speaker for Cat { fn sound(self) -> int { return 20 } }\n";
        let dog = run_main_full(&format!(
            "{}fn main() {{ let a = Dog {{ x: 0 }}\n return a.sound() }}",
            src
        ));
        let cat = run_main_full(&format!(
            "{}fn main() {{ let a = Cat {{ x: 0 }}\n return a.sound() }}",
            src
        ));
        assert!(matches!(dog, Value::Int(10)), "Dog should sound 10, got {:?}", dog);
        assert!(matches!(cat, Value::Int(20)), "Cat should sound 20, got {:?}", cat);
    }

    /// R8.6: a type whose impl omits a method with a default body in the trait
    /// dispatches to the trait's default implementation.
    #[test]
    fn trait_default_body_used_when_not_overridden() {
        let v = run_main_full(
            "trait Speaker { fn speak(self) -> int { return 99 } }\n\
             struct Cat { x: int }\n\
             impl Speaker for Cat { }\n\
             fn main() { let c = Cat { x: 0 }\n return c.speak() }",
        );
        assert!(matches!(v, Value::Int(99)), "expected default 99, got {:?}", v);
    }

    /// R8.6: an explicit override wins over the trait's default body.
    #[test]
    fn trait_override_wins_over_default_body() {
        let v = run_main_full(
            "trait Speaker { fn speak(self) -> int { return 99 } }\n\
             struct Dog { x: int }\n\
             impl Speaker for Dog { fn speak(self) -> int { return 7 } }\n\
             fn main() { let d = Dog { x: 0 }\n return d.speak() }",
        );
        assert!(matches!(v, Value::Int(7)), "expected override 7, got {:?}", v);
    }

    /// R8.6: three distinct types implementing the same trait each dispatch to
    /// their own implementation, confirming the receiver-type selection scales
    /// beyond two candidates (no accidental "first impl wins" behavior).
    #[test]
    fn trait_dispatch_across_three_types() {
        let src = "trait Shape { fn sides(self) -> int }\n\
                   struct Triangle { x: int }\n\
                   struct Square { x: int }\n\
                   struct Pentagon { x: int }\n\
                   impl Shape for Triangle { fn sides(self) -> int { return 3 } }\n\
                   impl Shape for Square { fn sides(self) -> int { return 4 } }\n\
                   impl Shape for Pentagon { fn sides(self) -> int { return 5 } }\n";
        let tri = run_main_full(&format!(
            "{}fn main() {{ let a = Triangle {{ x: 0 }}\n return a.sides() }}",
            src
        ));
        let sq = run_main_full(&format!(
            "{}fn main() {{ let a = Square {{ x: 0 }}\n return a.sides() }}",
            src
        ));
        let pent = run_main_full(&format!(
            "{}fn main() {{ let a = Pentagon {{ x: 0 }}\n return a.sides() }}",
            src
        ));
        assert!(matches!(tri, Value::Int(3)), "Triangle should have 3 sides, got {:?}", tri);
        assert!(matches!(sq, Value::Int(4)), "Square should have 4 sides, got {:?}", sq);
        assert!(matches!(pent, Value::Int(5)), "Pentagon should have 5 sides, got {:?}", pent);
    }

    /// R8.6: a trait method body may call another method on `self`; dispatch of
    /// the inner call resolves against the receiver's concrete type. Here the
    /// default `perimeter` body calls the per-type `side_len`, so the result
    /// reflects the receiver implementation (4 * side_len).
    #[test]
    fn trait_method_body_calls_another_method() {
        let src = "trait Poly { fn side_len(self) -> int\n fn perimeter(self) -> int { return self.side_len() * 4 } }\n\
                   struct Small { x: int }\n\
                   struct Big { x: int }\n\
                   impl Poly for Small { fn side_len(self) -> int { return 2 } }\n\
                   impl Poly for Big { fn side_len(self) -> int { return 25 } }\n";
        let small = run_main_full(&format!(
            "{}fn main() {{ let a = Small {{ x: 0 }}\n return a.perimeter() }}",
            src
        ));
        let big = run_main_full(&format!(
            "{}fn main() {{ let a = Big {{ x: 0 }}\n return a.perimeter() }}",
            src
        ));
        assert!(matches!(small, Value::Int(8)), "Small perimeter should be 8, got {:?}", small);
        assert!(matches!(big, Value::Int(100)), "Big perimeter should be 100, got {:?}", big);
    }
}

// ============================================================================
// Property 14 — New core constructs evaluate per reference semantics
// (R8.1–R8.5: closures with capture, `break`, `continue`, `match`-arm `return`).
// ============================================================================
#[cfg(test)]
mod new_core_construct_semantics_property {
    // Feature: memory-safe-self-hosting, Property 14: New core constructs evaluate per reference semantics
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// Parse `src`, register its free functions (mirroring `execute`), and run
    /// `main`'s body, returning the value `main` produces — exercising the real
    /// parser + interpreter end to end.
    fn run_main(src: &str) -> Value {
        let tokens = crate::frontend::lexer::tokenize(src);
        let program = crate::frontend::parser::parse(tokens);
        let mut env = Environment::new();
        for stmt in &program.statements {
            if let Statement::FnDecl { name, params, body, .. } = &stmt.kind {
                env.functions
                    .insert(name.clone(), std::sync::Arc::new(body.clone()));
                env.fn_params
                    .insert(name.clone(), params.iter().map(|p| p.name.clone()).collect());
                env.fn_mut
                    .insert(name.clone(), params.iter().map(param_is_mut).collect());
            }
        }
        let main_body = env.functions.get("main").cloned().expect("main present");
        env.run_function_frame(&main_body[..], Vec::new())
    }

    /// One generated program exercising exactly one new core construct, paired
    /// with the parameters needed to both render its source and compute the
    /// independent Rust oracle.
    #[derive(Clone, Debug)]
    enum Prog {
        /// `let c = n\n let f = fn(x) { return x + c }\n return f(m)` — closure
        /// capturing an enclosing binding (R8.1, R8.2). Oracle: `n + m`.
        ClosureCapture { n: i64, m: i64 },
        /// A `while` loop that sums `i` and `break`s when `i == k`
        /// (R8.3). Oracle: sum of `0..k`.
        BreakSum { k: i64 },
        /// A `while` loop that `continue`s on even `i`, summing odds in `0..n`
        /// (R8.4). Oracle: sum of odd values in `0..n`.
        ContinueOdds { n: i64 },
        /// A function whose `match` arms each `return` a per-arm value
        /// (R8.5). Oracle: `0 => 100, 1 => 200, _ => v + 7`.
        MatchArmReturn { v: i64 },
    }

    impl Prog {
        /// Render the program to `.ran` source text.
        fn source(&self) -> String {
            match self {
                Prog::ClosureCapture { n, m } => format!(
                    "fn main() {{ let c = {n}\n let f = fn(x) {{ return x + c }}\n return f({m}) }}",
                    n = n,
                    m = m
                ),
                Prog::BreakSum { k } => format!(
                    "fn main() {{ let acc = 0\n let i = 0\n while i < 1000000 {{ if i == {k} {{ break }}\n acc = acc + i\n i = i + 1 }} return acc }}",
                    k = k
                ),
                Prog::ContinueOdds { n } => format!(
                    "fn main() {{ let acc = 0\n let i = 0\n while i < {n} {{ if i % 2 == 0 {{ i = i + 1\n continue }}\n acc = acc + i\n i = i + 1 }} return acc }}",
                    n = n
                ),
                Prog::MatchArmReturn { v } => format!(
                    "fn pick(n) {{ match n {{ 0 => {{ return 100 }} 1 => {{ return 200 }} _ => {{ return n + 7 }} }} return -1 }}\n\
                     fn main() {{ return pick({v}) }}",
                    v = v
                ),
            }
        }

        /// The reference semantics computed independently in Rust (i64 oracle).
        fn oracle(&self) -> i64 {
            match self {
                Prog::ClosureCapture { n, m } => n + m,
                Prog::BreakSum { k } => (0..*k).sum(),
                Prog::ContinueOdds { n } => (0..*n).filter(|i| i % 2 != 0).sum(),
                Prog::MatchArmReturn { v } => match v {
                    0 => 100,
                    1 => 200,
                    other => other + 7,
                },
            }
        }
    }

    /// Generator over the four construct programs. Numeric parameters are kept
    /// small enough that the oracle cannot overflow `i64` and loops terminate
    /// quickly, while still covering negatives, zero, and the match-arm edges.
    fn prog_gen() -> Gen<Prog> {
        Gen::new(
            |rng: &mut Rng, _size| match rng.below(4) {
                0 => Prog::ClosureCapture {
                    n: rng.range_i64(-100_000, 100_000),
                    m: rng.range_i64(-100_000, 100_000),
                },
                1 => Prog::BreakSum {
                    k: rng.range_i64(0, 300),
                },
                2 => Prog::ContinueOdds {
                    n: rng.range_i64(0, 300),
                },
                _ => Prog::MatchArmReturn {
                    // Bias toward the special arms 0 and 1 while still covering
                    // the wildcard arm with assorted positives/negatives.
                    v: match rng.below(4) {
                        0 => 0,
                        1 => 1,
                        _ => rng.range_i64(-50, 50),
                    },
                },
            },
            // Shrink numeric parameters toward zero, preserving the variant so
            // the failing construct stays identifiable.
            |p: &Prog| {
                let halve = |x: i64| if x == 0 { Vec::new() } else { vec![0, x / 2] };
                match p {
                    Prog::ClosureCapture { n, m } => {
                        let mut out = Vec::new();
                        for nn in halve(*n) {
                            out.push(Prog::ClosureCapture { n: nn, m: *m });
                        }
                        for mm in halve(*m) {
                            out.push(Prog::ClosureCapture { n: *n, m: mm });
                        }
                        out
                    }
                    Prog::BreakSum { k } => {
                        halve(*k).into_iter().map(|k| Prog::BreakSum { k }).collect()
                    }
                    Prog::ContinueOdds { n } => {
                        halve(*n).into_iter().map(|n| Prog::ContinueOdds { n }).collect()
                    }
                    Prog::MatchArmReturn { v } => {
                        halve(*v).into_iter().map(|v| Prog::MatchArmReturn { v }).collect()
                    }
                }
            },
        )
    }

    /// Each generated program — exercising closures with capture, `break`,
    /// `continue`, and `match`-arm `return` — evaluates to the value predicted
    /// by an independent Rust oracle.
    #[test]
    fn prop_new_core_constructs_match_reference_semantics() {
        pbt::for_all(
            "P14 new core constructs evaluate per reference semantics",
            &prog_gen(),
            |p: &Prog| {
                let got = run_main(&p.source());
                matches!(got, Value::Int(n) if n == p.oracle())
            },
        );
    }
}
