#[allow(dead_code)]
mod frontend;
#[allow(dead_code)]
mod semantics;
#[allow(dead_code)]
mod backend;
#[allow(dead_code)]
mod runtime;
#[allow(dead_code)]
mod stdlib;
#[allow(dead_code)]
mod support;

use std::env;
use std::fs;
use std::io::{self, Write};
use std::process;

#[allow(unused_imports)]
use frontend::{lexer, parser};
#[allow(unused_imports)]
use semantics::analyzer as compiler;
use semantics::analyzer::OwnershipMode;
#[allow(unused_imports)]
use backend::codegen;
#[allow(unused_imports)]
use support::modules;
#[allow(unused_imports)]
use backend::vm::{BytecodeCompiler, VM};

/// Execution engine selection (R9.4). The Bytecode_VM is now the DEFAULT engine:
/// the normal `ran run` / `ran <file>` path attempts the VM first and falls back
/// automatically (and transparently) to the tree-walking interpreter whenever a
/// program uses constructs the VM does not yet support, the VM returns a
/// recoverable error (E1008/E1009/E1010/E1011/E1012/unsupported), or a
/// bytecode-compile panic occurs (see `run_via_vm`).
///
/// Escape hatch: `--no-vm` (alias `--interp`) forces the interpreter by clearing
/// this flag. `--vm` is retained as an explicit, *verbose* opt-in — same engine
/// as the default, but it prints a one-line banner plus fallback notes (see
/// `VM_VERBOSE`) for developers exercising the VM path directly.
static USE_VM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// True only when `--vm` was passed explicitly. Gates the VM banner and the
/// "falling back to the interpreter" notes so the *default* VM path stays fully
/// transparent (no extra stderr chatter, no double output): on a clean VM run
/// the buffered stdout is flushed once; on fallback the interpreter produces the
/// authoritative output. Developers who pass `--vm` opt into the diagnostics.
static VM_VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() {
    // Quiet, recoverable handling of runtime faults (see runtime::RuntimeFault).
    runtime::install_fault_hook();
    // Memory-safety watchdog: stop the process itself before the OS OOM-killer
    // would, for every path (interpreter, compiled binary, server, --vm).
    runtime::install_memory_watchdog();

    // If this binary has embedded source appended (compiled via `ran build`),
    // extract and run it. The payload is obfuscated .ran source, not bytecode.
    if let Some(source) = extract_embedded_program() {
        // Honour `--port N` in compiled binaries too: parse it before running
        // so the embedded program's `fn port(p)` entry point is selected
        // (sets RAN_PORT, which the runtime reads). Without this, a built site
        // ignores `--port` and always runs `main()`.
        let mut embedded_args: Vec<String> = env::args().collect();
        extract_port_flag(&mut embedded_args);
        // Honour `--max-depth=<N>` in compiled binaries too (recursion guard).
        extract_max_depth(&mut embedded_args);
        run_embedded(&source);
        return;
    }

    let mut args: Vec<String> = env::args().collect();

    // Execution engine selection (R9.4). The Bytecode_VM is the DEFAULT engine
    // (USE_VM starts `true`), with automatic, transparent fallback to the
    // interpreter inside `run_via_vm`. Two flags adjust this; both are consumed
    // here so downstream parsing is unaffected:
    //
    //   * `--vm`            keep the VM (the default) but run *verbosely*:
    //                       print the engine banner and fallback notes.
    //   * `--no-vm`/`--interp`  force the tree-walking interpreter (escape hatch
    //                       for users/tests that need to opt out of the VM).
    //
    // `--no-vm`/`--interp` wins if combined with `--vm` (explicit opt-out).
    if let Some(pos) = args.iter().position(|a| a == "--vm") {
        // VM is already the default; `--vm` just enables verbose diagnostics.
        VM_VERBOSE.store(true, std::sync::atomic::Ordering::Relaxed);
        args.remove(pos);
    }
    while let Some(pos) = args.iter().position(|a| a == "--no-vm" || a == "--interp") {
        USE_VM.store(false, std::sync::atomic::Ordering::Relaxed);
        args.remove(pos);
    }

    // Extract a global --port <N> flag (overrides http.server port at runtime)
    extract_port_flag(&mut args);

    // Resolve the ownership rollout mode (--ownership=warn|strict, env
    // RAN_OWNERSHIP, else default warn for this release). Removes the flag
    // from args so downstream parsing is unaffected.
    let ownership_mode = extract_ownership_mode(&mut args);

    // Extract an explicit build memory limit (`--mem-limit <BYTES>` /
    // `--mem-limit=<BYTES>`), passed to the resource-aware build manager.
    // Removed from args here so downstream parsing is unaffected (mirrors the
    // port/ownership flag handling). Only meaningful for `ran build`.
    let mem_limit = extract_mem_limit(&mut args);

    // Extract the recursion-depth limit (`--max-depth <N>` / `--max-depth=<N>`).
    // Applied globally via runtime::set_max_call_depth; an invalid value is
    // recoverable (diagnostic + keep the default), never an abrupt exit (R1.5).
    extract_max_depth(&mut args);

    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let first_arg = &args[1];

    // If first arg is a .ran file, run it directly
    if first_arg.ends_with(".ran") {
        execute_file(first_arg, false, ownership_mode);
        return;
    }

    match first_arg.as_str() {
        "run" => cmd_run(&args[2..], ownership_mode),
        "build" => cmd_build(&args[2..], ownership_mode, mem_limit),
        "test" => cmd_test(&args[2..], ownership_mode),
        "init" => cmd_init(&args[2..]),
        "repl" => cmd_repl(),
        "version" | "--version" | "-v" => cmd_version(),
        "help" | "--help" | "-h" => print_usage(),
        _ => {
            eprintln!("ran: unknown command '{}'", first_arg);
            eprintln!("Run 'ran help' for usage.");
            process::exit(1);
        }
    }
}

/// `ran test` — run all `test_*` functions in the entry file (and its imports).
fn cmd_test(args: &[String], ownership_mode: OwnershipMode) {
    let explicit = args.first().filter(|a| a.ends_with(".ran")).map(|s| s.as_str());
    let filename = resolve_entry(explicit);

    let source = match fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ran: cannot read '{}': {}", filename, e);
            process::exit(1);
        }
    };

    let program = modules::load_program(&filename).unwrap_or_else(|| {
        let tokens = lexer::tokenize(&source);
        let (p, syntax_diags) = parser::parse_checked(tokens);
        support::diagnostics::abort_on_syntax_errors(syntax_diags, &filename, &source);
        p
    });
    let checked = compiler::analyze_with_file(&program, &filename, &source, ownership_mode);
    let code = runtime::run_tests(&checked);
    process::exit(code);
}

/// Parse and remove `--port <N>` (or `--port=<N>`) from args.
/// Sets the RAN_PORT environment variable so the HTTP server can pick it up.
fn extract_port_flag(args: &mut Vec<String>) {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--port" {
            if i + 1 < args.len() {
                env::set_var("RAN_PORT", &args[i + 1]);
                args.drain(i..=i + 1);
                continue;
            } else {
                args.remove(i);
                continue;
            }
        } else if let Some(val) = args[i].strip_prefix("--port=") {
            env::set_var("RAN_PORT", val);
            args.remove(i);
            continue;
        }
        i += 1;
    }
}

/// Parse and remove the `--ownership` flag (`--ownership=<mode>` or
/// `--ownership <mode>`) from args, then resolve the effective mode.
///
/// Precedence (highest first):
///   1. explicit CLI flag `--ownership=warn|strict`
///   2. env var `RAN_OWNERSHIP` (fish: `set -x RAN_OWNERSHIP strict`)
///   3. default `warn` (compatibility-first rollout for this release)
///
/// Invalid values (from either the flag or the env var) abort with a clear
/// error message rather than silently falling back.
fn extract_ownership_mode(args: &mut Vec<String>) -> OwnershipMode {
    let mut cli_value: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--ownership" {
            if i + 1 < args.len() {
                cli_value = Some(args[i + 1].clone());
                args.drain(i..=i + 1);
                continue;
            } else {
                eprintln!("\x1b[31;1merror\x1b[0m: --ownership requires a value (`warn` or `strict`)");
                eprintln!("  \x1b[36m= help\x1b[0m: e.g. `ran run --ownership=strict app.ran`");
                process::exit(1);
            }
        } else if let Some(val) = args[i].strip_prefix("--ownership=") {
            cli_value = Some(val.to_string());
            args.remove(i);
            continue;
        }
        i += 1;
    }

    // 1. Explicit CLI flag wins.
    if let Some(val) = cli_value {
        return parse_ownership_or_exit(&val, "--ownership flag");
    }

    // 2. Environment override (handy for CI: `set -x RAN_OWNERSHIP strict`).
    if let Ok(val) = env::var("RAN_OWNERSHIP") {
        if !val.trim().is_empty() {
            return parse_ownership_or_exit(&val, "RAN_OWNERSHIP env var");
        }
    }

    // 3. Default for this release.
    OwnershipMode::DEFAULT
}

/// Parse an ownership mode, aborting with a clear, sourced error on failure.
fn parse_ownership_or_exit(value: &str, source: &str) -> OwnershipMode {
    match OwnershipMode::parse(value) {
        Ok(mode) => mode,
        Err(msg) => {
            eprintln!("\x1b[31;1merror\x1b[0m: {} ({})", msg, source);
            eprintln!("  \x1b[36m= help\x1b[0m: use `--ownership=warn` or `--ownership=strict`");
            eprintln!("           (fish: `set -x RAN_OWNERSHIP strict`)");
            process::exit(1);
        }
    }
}

/// Parse and remove the explicit build memory-limit flag (`--mem-limit <BYTES>`
/// or `--mem-limit=<BYTES>`) from args, returning the limit in bytes if present.
///
/// Mirrors `extract_port_flag`/`extract_ownership_mode`: the flag is consumed
/// here so downstream argument parsing is unaffected. The value is a plain byte
/// count (e.g. `536870912`) or an integer with an optional binary unit suffix
/// (`KB`/`MB`/`GB`, case-insensitive; `K`/`M`/`G` accepted too — 1024-based).
///
/// An invalid value is *recoverable* (R5.3): a missing value or a syntactically
/// malformed one emits a non-fatal `E0703` warning and falls back to the
/// computed budget WITHOUT aborting the process. The semantically invalid value
/// `0` (and any value `> total`) parses successfully and is forwarded to the
/// build manager, which likewise reports it as an invalid limit (`E0703`) and
/// falls back to the computed budget rather than aborting.
fn extract_mem_limit(args: &mut Vec<String>) -> Option<u64> {
    use support::diagnostics::Diagnostic;

    let mut raw: Option<String> = None;
    let mut missing_value = false;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--mem-limit" {
            if i + 1 < args.len() {
                raw = Some(args[i + 1].clone());
                args.drain(i..=i + 1);
                continue;
            } else {
                // Missing value: recoverable (R5.3) — warn and fall back to the
                // computed budget instead of aborting the process.
                missing_value = true;
                args.remove(i);
                continue;
            }
        } else if let Some(val) = args[i].strip_prefix("--mem-limit=") {
            raw = Some(val.to_string());
            args.remove(i);
            continue;
        }
        i += 1;
    }

    if missing_value {
        Diagnostic::from_code(
            "E0703",
            "--mem-limit diberikan tanpa nilai; memakai anggaran memori terhitung.",
        )
        .emit("");
        return None;
    }

    let raw = raw?;
    match parse_byte_size(&raw) {
        Some(bytes) => Some(bytes),
        None => {
            // Unparseable value: recoverable (R5.3) — emit the E0703 build
            // warning (code + context + help, R5.6) and fall back to the
            // computed budget instead of abruptly exiting the process.
            Diagnostic::from_code(
                "E0703",
                format!(
                    "nilai --mem-limit '{}' tidak valid (pakai byte count mis. 536870912, atau sufiks 512MB/2GB); memakai anggaran memori terhitung.",
                    raw
                ),
            )
            .emit("");
            None
        }
    }
}

/// Parse a byte-size string: a plain integer (bytes) or an integer with an
/// optional unit suffix (`KB`/`MB`/`GB`, case-insensitive; bare `K`/`M`/`G`
/// also accepted). Units are 1024-based. Returns `None` on malformed input or
/// on multiplication overflow.
fn parse_byte_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    // Order matters: check the two-letter suffix before the one-letter one.
    let (num_part, mult) = if let Some(n) = lower.strip_suffix("gb").or_else(|| lower.strip_suffix('g')) {
        (n, GIB)
    } else if let Some(n) = lower.strip_suffix("mb").or_else(|| lower.strip_suffix('m')) {
        (n, MIB)
    } else if let Some(n) = lower.strip_suffix("kb").or_else(|| lower.strip_suffix('k')) {
        (n, KIB)
    } else {
        (lower.as_str(), 1u64)
    };
    let value: u64 = num_part.trim().parse().ok()?;
    value.checked_mul(mult)
}

/// Parse and remove the recursion-depth flag (`--max-depth <N>` or
/// `--max-depth=<N>`) from args, applying it via `runtime::set_max_call_depth`.
///
/// Mirrors `extract_mem_limit`/`extract_ownership_mode`: the flag is consumed
/// here so downstream argument parsing is unaffected. Unlike the build flags,
/// an invalid value is *recoverable* (R1.5): a non-positive value, a
/// non-integer, or a missing value emits a Diagnostic with a help hint and the
/// runtime keeps the compiled-in default (10000) WITHOUT exiting the process.
/// This guards against a typo silently disarming the recursion guard or
/// abruptly killing a run.
fn extract_max_depth(args: &mut Vec<String>) {
    let mut raw: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i] == "--max-depth" {
            if i + 1 < args.len() {
                raw = Some(args[i + 1].clone());
                args.drain(i..=i + 1);
                continue;
            } else {
                // Missing value: recoverable — warn and keep the default.
                eprintln!("\x1b[31;1merror\x1b[0m: --max-depth requires a positive integer value");
                eprintln!("  \x1b[36m= help\x1b[0m: e.g. `--max-depth=10000`; keeping the default recursion limit (10000)");
                args.remove(i);
                continue;
            }
        } else if let Some(val) = args[i].strip_prefix("--max-depth=") {
            raw = Some(val.to_string());
            args.remove(i);
            continue;
        }
        i += 1;
    }

    let raw = match raw {
        Some(r) => r,
        None => return, // flag not given: keep the default
    };

    match raw.trim().parse::<usize>() {
        Ok(n) if n > 0 => runtime::set_max_call_depth(n),
        _ => {
            // Non-positive / non-integer / overflowing: recoverable (R1.5).
            eprintln!("\x1b[31;1merror\x1b[0m: invalid --max-depth value '{}'", raw);
            eprintln!("  \x1b[36m= help\x1b[0m: use a positive integer (e.g. --max-depth=10000); keeping the default recursion limit (10000)");
        }
    }
}

/// Run a checked program on a dedicated thread with a large explicit stack so
/// the Recursion_Guard (`E1007`, raised at the call boundary in
/// `runtime/frame.rs`) trips *before* the OS stack can overflow (SIGSEGV).
///
/// Each Ran tree-walking call consumes a sizeable slice of Rust stack, so the
/// main thread's ~8 MiB OS stack overflows long before the default 10000-frame
/// depth limit is reached. Running execution on a thread with a generous stack
/// (1 GiB) gives the guard room to fire first, turning an uncatchable SIGSEGV
/// into a clean, recoverable `E1007` diagnostic + exit code 70.
///
/// `runtime::execute` installs its own top-level Catch_Boundary and calls
/// `process::exit(70)` on fault, which terminates the whole process (from any
/// thread) — so the exit code propagates without an explicit join dance. A
/// genuine (non-fault) panic is reported by the global fault hook on this
/// thread; we surface it as a non-zero exit after join.
fn run_checked_on_big_stack(checked: compiler::CheckedProgram) {
    // 1 GiB: comfortably accommodates the default 10000-frame depth limit so
    // E1007 fires well before this stack is exhausted.
    const EXEC_STACK_BYTES: usize = 1024 * 1024 * 1024;

    // Scoped thread so the worker can borrow `checked` (no move/clone needed)
    // and a spawn failure can still fall back to running inline.
    let spawned = std::thread::scope(|s| {
        match std::thread::Builder::new()
            .stack_size(EXEC_STACK_BYTES)
            .spawn_scoped(s, || {
                runtime::execute(&checked);
            }) {
            Ok(handle) => {
                if handle.join().is_err() {
                    // A non-RuntimeFault panic (real bug): the fault hook
                    // already reported it on the worker thread. Exit non-zero
                    // rather than silently succeeding.
                    process::exit(70);
                }
                true
            }
            Err(e) => {
                eprintln!("\x1b[33mwarning\x1b[0m: could not start execution thread ({}); running inline", e);
                false
            }
        }
    });

    if !spawned {
        // Rare: large-stack thread could not be created. Run inline so the
        // program still executes (the guard still applies, with less headroom).
        runtime::execute(&checked);
    }
}

/// Act on a single `Degradation` decision from the build manager (R18).
///
/// Non-fatal rungs are logged so the operator can see the build adapting to
/// memory pressure; `Degradation::Abort` surfaces the `E0704` diagnostic the
/// manager recorded on this tick, releases build-management state, and stops the
/// build with a non-zero exit (R18.4/18.5).
fn handle_degradation(
    mgr: &mut backend::build_manager::BuildResourceManager,
    degradation: backend::build_manager::Degradation,
    phase: &str,
) {
    use backend::build_manager::Degradation;
    match degradation {
        Degradation::Normal => {}
        Degradation::ReduceParallelism(n) => println!(
            "\x1b[33m   Resources\x1b[0m {}: memory pressure - reducing parallelism to {} jobs",
            phase, n
        ),
        Degradation::Serialize => println!(
            "\x1b[33m   Resources\x1b[0m {}: memory pressure - serializing to a single job",
            phase
        ),
        Degradation::Delay(d) => println!(
            "\x1b[33m   Resources\x1b[0m {}: memory pressure - delaying next job {} ms",
            phase,
            d.as_millis()
        ),
        Degradation::Abort => {
            // tick() recorded the E0704 diagnostic on the transition to Abort;
            // surface it (location-less build diagnostic -> empty source).
            if let Some(diag) = mgr.warnings().last() {
                diag.emit("");
            }
            mgr.finish();
            process::exit(1);
        }
    }
}

fn cmd_run(args: &[String], ownership_mode: OwnershipMode) {
    let explicit = args.first().filter(|a| a.ends_with(".ran")).map(|s| s.as_str());
    if let Some(f) = explicit {
        if !f.ends_with(".ran") {
            eprintln!("ran: file must have .ran extension");
            process::exit(1);
        }
    }
    let entry = resolve_entry(explicit);
    execute_file(&entry, false, ownership_mode);
}

/// Resolve the entry `.ran` file for run/build/test.
///
/// Order: explicit file arg > `entry` in ran.toml > common locations
/// (src/main.ran, main.ran) > any `.ran` containing `fn main` in `.` then
/// `src/`. ran.toml is optional; a standalone file needs none.
fn resolve_entry(explicit: Option<&str>) -> String {
    if let Some(f) = explicit {
        return f.to_string();
    }
    if let Some(e) = read_manifest_entry() {
        if fs::metadata(&e).is_ok() {
            return e;
        } else {
            eprintln!("\x1b[33mwarning\x1b[0m: ran.toml entry '{}' not found; auto-detecting", e);
        }
    }
    for c in ["src/main.ran", "main.ran"] {
        if fs::metadata(c).is_ok() {
            return c.to_string();
        }
    }
    if let Some(f) = find_ran_with_main(".") {
        return f;
    }
    if let Some(f) = find_ran_with_main("src") {
        return f;
    }
    eprintln!("\x1b[31;1merror\x1b[0m: no entry file found");
    eprintln!("  \x1b[36m= help\x1b[0m: pass a file (`ran run app.ran`), add `entry` to ran.toml,");
    eprintln!("           or place a .ran file containing `fn main()` here or in src/");
    process::exit(1);
}

/// Find the first `*.ran` file in `dir` that defines `fn main`, sorted for
/// determinism. Returns the path relative to the current directory.
fn find_ran_with_main(dir: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    let entries = fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) == Some("ran") {
            if let Ok(src) = fs::read_to_string(&path) {
                if source_defines_main(&src) {
                    candidates.push(path.to_string_lossy().to_string());
                }
            }
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

/// True if the source defines a top-level `fn main(`.
fn source_defines_main(src: &str) -> bool {
    src.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("fn main(") || t.starts_with("fn main (")
    })
}

/// A lightweight braille spinner shown during a concise (non-`--debug`) build,
/// so there is a live compile animation. Only animates on a TTY; on a pipe it
/// does nothing (so captured/redirected output stays clean).
struct Spinner {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(label: &str) -> Option<Self> {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() {
            return None;
        }
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::io::Write;
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let label = label.to_string();
        let handle = std::thread::spawn(move || {
            let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let t = std::time::Instant::now();
            let mut i = 0usize;
            while !s2.load(Ordering::Relaxed) {
                print!(
                    "\r  \x1b[35m{}\x1b[0m Compiling {} \x1b[2m[{:.1}s]\x1b[0m ",
                    frames[i % frames.len()],
                    label,
                    t.elapsed().as_secs_f64()
                );
                let _ = std::io::stdout().flush();
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            print!("\r\x1b[2K"); // clear the spinner line
            let _ = std::io::stdout().flush();
        });
        Some(Self { stop, handle: Some(handle) })
    }

    fn stop(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Print one detailed build stage line (only in `--debug` mode). Flushed so
/// progress is visible immediately even when piped.
fn build_stage(debug: bool, start: std::time::Instant, label: &str, msg: &str) {
    if !debug {
        return;
    }
    use std::io::Write;
    println!(
        "  \x1b[35m▸\x1b[0m \x1b[1m{:<9}\x1b[0m {} \x1b[2m[+{:.3}s]\x1b[0m",
        label,
        msg,
        start.elapsed().as_secs_f64()
    );
    let _ = std::io::stdout().flush();
}

/// Count top-level declarations for the build summary.
fn count_decls(program: &frontend::ast::Program) -> (usize, usize, usize, usize) {
    use frontend::ast::Statement;
    let (mut f, mut s, mut e, mut i) = (0usize, 0usize, 0usize, 0usize);
    for st in &program.statements {
        match &st.kind {
            Statement::FnDecl { .. } => f += 1,
            Statement::StructDecl { .. } => s += 1,
            Statement::EnumDecl { .. } => e += 1,
            Statement::Import { .. } => i += 1,
            _ => {}
        }
    }
    (f, s, e, i)
}

/// Write a build artifact dump under `target/<name>.<ext>`, returning the path
/// on success. Dumps exist so developers can inspect exactly what the compiler
/// saw (tokens / AST / analysis) — essential for debugging the language itself.
fn write_dump(name: &str, ext: &str, content: &str) -> Option<String> {
    let dir = std::path::Path::new("debug");
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    let path = dir.join(format!("{}.{}", name, ext));
    match fs::write(&path, content) {
        Ok(()) => Some(path.display().to_string()),
        Err(_) => None,
    }
}

fn cmd_build(args: &[String], ownership_mode: OwnershipMode, mem_limit: Option<u64>) {
    use std::time::Instant;
    let start = Instant::now();

    // `--debug`: emit the full artifact dump (./debug/) and a detailed,
    // per-stage build log. Without it, the build is concise with a live spinner.
    let debug = args.iter().any(|a| a == "--debug");
    // Native AOT vs embed-source selection:
    //   * `--native` / `--aot`  : FORCE native codegen (emit C -> system `cc` ->
    //     native ELF). Out-of-subset constructs are a hard E0606 (no fallback).
    //   * `--embed`             : FORCE the embed-source standalone (program
    //     bundled with the interpreter) — for when `cc` is unavailable or you
    //     explicitly want that path.
    //   * DEFAULT (no flag)     : AUTO — build native when the program lies in
    //     the native subset (Go/Rust-class speed, ~1–2 MB binary), else fall
    //     back to embed-source. Native output is byte-for-byte identical to the
    //     interpreter, so the automatic choice is safe.
    let force_native = args.iter().any(|a| a == "--native" || a == "--aot");
    let force_embed = args.iter().any(|a| a == "--embed" || a == "--embed-source");
    let link_static = args.iter().any(|a| a == "--link-static");
    let args: Vec<String> = args
        .iter()
        .filter(|a| {
            *a != "--debug"
                && *a != "--native"
                && *a != "--aot"
                && *a != "--embed"
                && *a != "--embed-source"
                && *a != "--link-static"
        })
        .cloned()
        .collect();
    let args = &args[..];

    // Resolve entry: explicit arg, else project detection.
    let explicit = args.first().filter(|a| a.ends_with(".ran")).map(|s| s.as_str());
    let has_explicit = explicit.is_some();
    let filename = resolve_entry(explicit);

    // Output flag can appear after the optional file arg.
    let out_arg_idx = if has_explicit { 1 } else { 0 };
    let output = if args.len() > out_arg_idx + 1 && args[out_arg_idx] == "-o" {
        args[out_arg_idx + 1].clone()
    } else {
        read_manifest_field("name").unwrap_or_else(|| {
            std::path::Path::new(&filename)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "app".to_string())
        })
    };

    let proj = read_manifest_field("name").unwrap_or_else(|| output.clone());

    let source = match fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ran: cannot read '{}': {}", filename, e);
            process::exit(1);
        }
    };
    let source_size = source.len();

    // Discrete, always-visible stage lines (compilation can be near-instant).
    println!("  \x1b[35;1m◆ Compiling\x1b[0m {} \x1b[2m(entry: {})\x1b[0m", proj, filename);

    // Resource-aware build manager (Kelompok D, R15–R18): detect the host OS,
    // probe memory, and compute an adaptive Memory_Budget with a safety reserve
    // *before* any heavy build work begins. Honoured minimally below by ticking
    // the degradation ladder at each significant build step. Must outlive the
    // build steps, so it is declared `mut` here.
    let mut build_mgr = backend::build_manager::BuildResourceManager::init(mem_limit);
    // Surface the non-fatal warnings init collected (E0701 unknown OS / E0702
    // probe failure / E0703 invalid limit or available<=reserve) so the user
    // sees them; these are location-less build diagnostics (empty source).
    for w in build_mgr.warnings() {
        w.emit("");
    }
    if debug {
        use support::sysinfo as si;
        println!(
            "  \x1b[2mPlatform\x1b[0m  {} {} · {} CPU · RAM {} free / {} total (reserving {} for the OS)",
            si::os_name(),
            si::arch(),
            si::cpu_count(),
            si::human_bytes(si::mem_available()),
            si::human_bytes(si::mem_total()),
            si::human_bytes(si::os_reserve_bytes()),
        );
        let limit_note = match build_mgr.user_limit() {
            Some(l) => format!(" · user limit {}", si::human_bytes(l)),
            None => String::new(),
        };
        println!(
            "  \x1b[2mResources\x1b[0m memory budget {} (safety reserve {}){}",
            si::human_bytes(build_mgr.budget()),
            si::human_bytes(build_mgr.safety_reserve()),
            limit_note,
        );
    }

    // ---- Phase 1: load + merge modules ----
    {
        let degr = build_mgr.tick();
        handle_degradation(&mut build_mgr, degr, "module load");
    }
    let program = modules::load_program(&filename).unwrap_or_else(|| {
        let tokens = lexer::tokenize(&source);
        let (p, syntax_diags) = parser::parse_checked(tokens);
        support::diagnostics::abort_on_syntax_errors(syntax_diags, &filename, &source);
        p
    });
    // Single merged source (local imports inlined): used for the token dump and
    // embedded into the standalone binary so it runs with no `ran` install and
    // no separate source files on the target machine.
    let merged_source = modules::load_merged_source(&filename).unwrap_or_else(|| source.clone());

    // ---- Phase 2: lex (count + timing) ----
    let t_lex = Instant::now();
    let tokens = lexer::tokenize(&merged_source);
    build_stage(debug, start,
        "Lexing",
        &format!(
            "{} → {} tokens in {:.3}s",
            filename,
            tokens.len(),
            t_lex.elapsed().as_secs_f64()
        ),
    );

    // ---- Phase 3: parse summary + module resolution ----
    let (n_fn, n_struct, n_enum, n_import) = count_decls(&program);
    build_stage(debug, start,
        "Parsing",
        &format!("{} statements, no syntax errors", program.statements.len()),
    );
    if n_import > 0 {
        build_stage(debug, start,
            "Resolving",
            &format!("{} import statement(s) merged into one program", n_import),
        );
    }

    // Auto-manage ran.toml only when appropriate (see policy below).
    let used = collect_imports(&program);
    let manage_manifest = !has_explicit || std::path::Path::new("ran.toml").exists();
    sync_manifest_dependencies(&used, manage_manifest, &filename);

    // ---- Phase 4: semantic + ownership/borrow checking ----
    {
        let degr = build_mgr.tick();
        handle_degradation(&mut build_mgr, degr, "analysis");
    }
    let own = match ownership_mode {
        OwnershipMode::Strict => "strict",
        _ => "warn",
    };
    build_stage(debug, start,
        "Checking",
        &format!(
            "{} fn · {} struct · {} enum · ownership={}",
            n_fn, n_struct, n_enum, own
        ),
    );
    let t_check = Instant::now();
    let checked = compiler::analyze_with_file(&program, &filename, &source, ownership_mode);
    if !checked.has_main {
        // Build step (checking) failed: release build-management state before
        // the build stops (R5.5).
        build_mgr.finish();
        eprintln!("\x1b[31;1merror\x1b[0m[E0700]: no main() function found in {}", filename);
        eprintln!("  \x1b[36m= help\x1b[0m: add `fn main() {{ ... }}` as the entry point");
        process::exit(1);
    }
    build_stage(debug, start,
        "Checked",
        &format!("semantics OK in {:.3}s", t_check.elapsed().as_secs_f64()),
    );

    // ---- Phase 5: emit debug dumps (tokens / AST / analysis / bytecode) ----
    // Only in `--debug`: writes ./debug/<name>.{tokens,ast,check,bc}.txt.
    let dump_name = std::path::Path::new(&output)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "program".to_string());
    let mut dumps: Vec<String> = Vec::new();
    if debug {
        if let Some(p) = write_dump(&dump_name, "tokens.txt", &format!("{:#?}", tokens)) {
            dumps.push(p);
        }
        if let Some(p) = write_dump(&dump_name, "ast.txt", &format!("{:#?}", program)) {
            dumps.push(p);
        }
        {
            let mut summary = String::new();
            summary.push_str(&format!("entry:        {}\n", filename));
            summary.push_str(&format!("ownership:    {}\n", own));
            summary.push_str(&format!("statements:   {}\n", program.statements.len()));
            summary.push_str(&format!("functions:    {}\n", n_fn));
            summary.push_str(&format!("structs:      {}\n", n_struct));
            summary.push_str(&format!("enums:        {}\n", n_enum));
            summary.push_str(&format!("imports:      {}\n", n_import));
            summary.push_str(&format!("tokens:       {}\n", tokens.len()));
            summary.push_str(&format!(
                "dependencies: {}\n",
                if used.is_empty() { "(none)".to_string() } else { used.join(", ") }
            ));
            summary.push_str("\nfunctions:\n");
            for st in &program.statements {
                if let frontend::ast::Statement::FnDecl { name, params, .. } = &st.kind {
                    summary.push_str(&format!("  fn {}({} params)\n", name, params.len()));
                }
            }
            if let Some(p) = write_dump(&dump_name, "check.txt", &summary) {
                dumps.push(p);
            }
        }
        // Bytecode dump (experimental VM target): compile the AST to bytecode and
        // disassemble it. Guarded — VM codegen is experimental, so a panic here
        // must NOT fail the build; we suppress it and note the dump is partial.
        {
            let prev_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                backend::vm::BytecodeCompiler::compile(&program)
            }));
            std::panic::set_hook(prev_hook);
            let bc_text = match compiled {
                Ok(result) => backend::vm::disassemble(&result),
                Err(_) => {
                    "; Ran bytecode disassembly (experimental VM target)\n\
                     ; bytecode codegen does not yet cover every construct in this\n\
                     ; program; execution uses the tree-walking interpreter.\n"
                        .to_string()
                }
            };
            if let Some(p) = write_dump(&dump_name, "bc.txt", &bc_text) {
                dumps.push(p);
            }
        }
        if !dumps.is_empty() {
            build_stage(debug, start, "Emitting", &dumps.join(", "));
        }
    }

    // ---- Phase 6: link the standalone binary ----
    // Decide native vs embed. `--native` forces native; `--embed` forces embed;
    // otherwise AUTO — native when the program is within the native subset.
    let native = if force_native {
        true
    } else if force_embed {
        false
    } else {
        backend::aot::lower::supported(&checked, &filename).is_ok()
    };
    let mut spinner;
    let mut success = false;
    let mut did_native = false;
    if native {
        // Real native AOT codegen: emit C, compile + link with the system C
        // compiler, produce a genuine native ELF with NO embedded interpreter
        // and NO `.ran` source. Out-of-subset constructs are a hard E0606 error
        // (never a silent interpreter fallback).
        build_stage(debug, start, "Finishing", "native codegen (emit C → cc → link)");
        spinner = if debug { None } else { Spinner::start(&proj) };
        let opts = backend::aot::AotOptions { file: filename.clone(), link_static };
        match backend::aot::compile_native(&checked, &output, &opts) {
            Ok(()) => {
                if let Some(s) = spinner.take() { s.stop(); }
                success = true;
                did_native = true;
            }
            Err(diag) => {
                if let Some(s) = spinner.take() { s.stop(); }
                if force_native {
                    // The user explicitly asked for native: surface the error.
                    diag.emit(&source);
                    build_mgr.finish();
                    process::exit(1);
                }
                // AUTO mode: a native build failed (e.g. no C compiler on this
                // host). Fall back to the self-contained embed-source binary so
                // the build still succeeds; note it so the slowdown is visible.
                eprintln!(
                    "  \x1b[33mNote\x1b[0m native build unavailable ({}); \
                     falling back to the embed-source binary. Pass `--native` to \
                     require native, or install a C compiler for full speed.",
                    diag.code.as_deref().unwrap_or("E06xx")
                );
            }
        }
    }
    if !did_native {
        build_stage(debug, start, "Finishing", "linking standalone binary (runtime + embedded program)");
        // Live compile animation only around the slow linking phase — all the
        // abort-prone phases (parse/analyze) already ran above with no spinner, so
        // their diagnostics print cleanly.
        spinner = if debug { None } else { Spinner::start(&proj) };
        success = codegen::compile_standalone(&merged_source, &output);
        if let Some(s) = spinner.take() { s.stop(); }
    }
    if !success {
        build_mgr.finish(); // release build-management state (R18.5)
        process::exit(1);
    }

    let elapsed = start.elapsed();
    let bin_size = fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    if debug && !used.is_empty() {
        println!("  \x1b[2mDependencies\x1b[0m {}", used.join(", "));
    }
    println!(
        "  \x1b[32;1m✓ Finished\x1b[0m {} [optimized] {} B src → {:.1} KB bin \x1b[2min {:.3}s\x1b[0m",
        output,
        source_size,
        bin_size as f64 / 1024.0,
        elapsed.as_secs_f64()
    );
    let shown = if output.starts_with('/') || output.starts_with("./") {
        output.clone()
    } else {
        format!("./{}", output)
    };
    println!("    \x1b[32;1m✓ Built\x1b[0m {}", shown);
    if !dumps.is_empty() {
        println!("    \x1b[35m▸ Dump\x1b[0m  ./debug/{}.{{tokens,ast,check,bc}}.txt", dump_name);
    }
    println!(
        "  \x1b[36m▸ Standalone\x1b[0m runs on another machine with no `ran` install"
    );
    if did_native {
        println!(
            "  \x1b[32;1m▸ Native\x1b[0m true native binary — no embedded interpreter, no `.ran` source inside"
        );
    }
    if !used.is_empty() {
        println!(
            "        \x1b[33mNote\x1b[0m uses {} — the target also needs the matching system library (TLS/SQLite) unless statically linked",
            used.join(", ")
        );
    }
    // Build completed: release all build-management resources (R18.5).
    build_mgr.finish();
}

/// Collect stdlib module names imported anywhere in the (merged) program.
fn collect_imports(program: &frontend::ast::Program) -> Vec<String> {
    use frontend::ast::Statement;
    let known = [
        "http", "time", "fs", "json", "os", "math", "html", "str", "rand",
        "log", "decimal", "env",
    ];
    let mut found: Vec<String> = Vec::new();
    for stmt in &program.statements {
        if let Statement::Import { path, .. } = &stmt.kind {
            if known.contains(&path.as_str()) && !found.contains(path) {
                found.push(path.clone());
            }
        }
    }
    found.sort();
    found
}

/// Ensure every used stdlib module is listed under `[dependencies]` in ran.toml.
/// Policy: if `allow_create` is false and no ran.toml exists, do nothing (a
/// one-off `ran build file.ran` should not litter a manifest). If ran.toml
/// exists, missing deps are always appended.
fn sync_manifest_dependencies(used: &[String], allow_create: bool, entry: &str) {
    if used.is_empty() {
        return;
    }
    let existing = fs::read_to_string("ran.toml").ok();
    match existing {
        None => {
            if !allow_create {
                return; // standalone one-off build: leave the directory clean
            }
            // No manifest: create a minimal one with the real entry + deps.
            let name = std::path::Path::new(entry)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "app".to_string());
            let mut s = format!(
                "[project]\nname = \"{}\"\nversion = \"0.1.0\"\n\n[build]\nentry = \"{}\"\n\n[dependencies]\n",
                name, entry
            );
            for m in used {
                s.push_str(&format!("{} = \"std\"\n", m));
            }
            let _ = fs::write("ran.toml", s);
            println!("\x1b[33m     Created\x1b[0m ran.toml with {} dependencies", used.len());
        }
        Some(text) => {
            let mut missing: Vec<&String> = used
                .iter()
                .filter(|m| !manifest_lists_dep(&text, m))
                .collect();
            missing.dedup();
            if missing.is_empty() {
                return;
            }
            let mut new_text = text.clone();
            if !new_text.contains("[dependencies]") {
                if !new_text.ends_with('\n') {
                    new_text.push('\n');
                }
                new_text.push_str("\n[dependencies]\n");
            }
            // Append missing deps right after the [dependencies] header.
            let mut lines: Vec<String> = new_text.lines().map(|l| l.to_string()).collect();
            if let Some(idx) = lines.iter().position(|l| l.trim() == "[dependencies]") {
                for (k, m) in missing.iter().enumerate() {
                    lines.insert(idx + 1 + k, format!("{} = \"std\"", m));
                }
            }
            let _ = fs::write("ran.toml", lines.join("\n") + "\n");
            let names: Vec<String> = missing.iter().map(|s| s.to_string()).collect();
            println!("\x1b[33m     Updated\x1b[0m ran.toml +deps: {}", names.join(", "));
        }
    }
}

/// True if ran.toml already declares dependency `name`.
fn manifest_lists_dep(text: &str, name: &str) -> bool {
    let mut in_deps = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if in_deps {
            if let Some(key) = t.split('=').next() {
                if key.trim() == name {
                    return true;
                }
            }
        }
    }
    false
}

/// Read a top-level string field (e.g. `name`) from `[project]` in ran.toml.
fn read_manifest_field(field: &str) -> Option<String> {
    let text = fs::read_to_string("ran.toml").ok()?;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(field) {
            let rest = rest.trim_start();
            if let Some(eq) = rest.strip_prefix('=') {
                let val = eq.trim().trim_matches('"').to_string();
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

fn cmd_init(args: &[String]) {
    let dir = if args.is_empty() {
        ".".to_string()
    } else {
        args[0].clone()
    };

    // Project name = basename of the target directory (never the full path).
    let project_name = if args.is_empty() {
        std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "myproject".to_string())
    } else {
        std::path::Path::new(&dir)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.clone())
    };

    // Create project structure
    if dir != "." {
        let _ = fs::create_dir_all(&dir);
    }

    let base = if dir == "." { String::new() } else { format!("{}/", dir) };

    // ran.toml - project manifest (entry now lives under src/)
    let manifest = format!(
        r#"[project]
name = "{}"
version = "0.1.0"
description = ""
authors = []
license = "MIT"

[build]
entry = "src/main.ran"
output = "{}"
strip = true
optimize = true

[dependencies]
# Auto-managed by `ran build`. Std modules used in code are listed here.
"#,
        project_name, project_name
    );
    let _ = fs::write(format!("{}ran.toml", base), manifest);

    // src/ layout
    let _ = fs::create_dir_all(format!("{}src", base));
    let _ = fs::create_dir_all(format!("{}src/lib", base));

    // src/main.ran
    let main_content = format!(
        r#"#!/usr/bin/env ran
# {} - created with `ran init`

import "std::log" as log

fn main() {{
    log.info("starting {}")
    echo "Hello from {}!"
}}

fn test_greeting() {{
    assert(1 + 1 == 2, "math works")
}}
"#,
        project_name, project_name, project_name
    );
    let _ = fs::write(format!("{}src/main.ran", base), main_content);

    // .gitignore
    let gitignore = r#"# Ran build output
/target/
*.ranc
/dist/

# OS
.DS_Store
Thumbs.db

# IDE
.vscode/
.idea/
"#;
    let _ = fs::write(format!("{}.gitignore", base), gitignore);

    // public/ directory for web projects
    let _ = fs::create_dir_all(format!("{}public", base));

    if dir == "." {
        println!("ran: initialized project '{}' in current directory", project_name);
    } else {
        println!("ran: created project '{}'", project_name);
    }
    println!();
    println!("  {}ran.toml          project manifest", base);
    println!("  {}src/main.ran      entry point", base);
    println!("  {}src/lib/          local modules", base);
    println!("  {}public/           static files (web)", base);
    println!("  {}.gitignore", base);
    println!();
    let prefix = if dir == "." { String::new() } else { format!("cd {} && ", dir) };
    println!("Run:   {}ran run", prefix);
    println!("Test:  {}ran test", prefix);
    println!("Build: {}ran build", prefix);
}

fn cmd_repl() {
    println!("Ran REPL v0.3.9");
    println!("Type expressions or statements. Type 'exit' or Ctrl+D to quit.");
    println!();

    let stdin = io::stdin();
    let mut buffer = String::new();
    let mut in_multiline = false;
    let mut brace_depth: i32 = 0;

    loop {
        if in_multiline {
            print!("...  ");
        } else {
            print!("ran> ");
        }
        io::stdout().flush().unwrap();

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("ran repl: read error: {}", e);
                break;
            }
        }

        for ch in line.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                _ => {}
            }
        }

        buffer.push_str(&line);

        if brace_depth > 0 {
            in_multiline = true;
            continue;
        }

        in_multiline = false;
        brace_depth = 0;

        let input = buffer.trim().to_string();
        buffer.clear();

        if input.is_empty() {
            continue;
        }

        // REPL commands
        match input.as_str() {
            "exit" | "exit;" | "quit" | "quit;" | ".exit" | ".quit" => {
                println!("Bye!");
                break;
            }
            "help" | ".help" => {
                println!("  exit / quit   Exit REPL");
                println!("  help          Show this help");
                println!("  .clear        Clear screen");
                continue;
            }
            ".clear" => {
                print!("\x1b[2J\x1b[H");
                io::stdout().flush().unwrap();
                continue;
            }
            _ => {}
        }

        // Execute via interpreter for REPL
        let tokens = lexer::tokenize(&input);
        let program = parser::parse(tokens);

        // For REPL: execute all statements directly (no need for main())
        let checked = compiler::analyze(&program);
        // Force execution even without main
        let mut checked_program = checked;
        checked_program.has_main = false; // Don't look for main

        // Manually execute all statements
        runtime::execute_statements(&checked_program.program);
    }
}

fn cmd_version() {
    println!("ran v0.3.9");
    println!("The Ran Programming Language");
    println!("A self-hosted language for internal systems and business tooling.");
    println!("Engine: bytecode VM (default) with tree-walking interpreter fallback");
}

fn print_usage() {
    println!("Ran Programming Language v0.3.9");
    println!();
    println!("Usage:");
    println!("  ran <file.ran>          Run a .ran file");
    println!("  ran run [file.ran]      Run the project (defaults to src/main.ran)");
    println!("  ran build [file.ran]    Compile to a standalone native binary");
    println!("  ran test [file.ran]     Run all test_* functions");
    println!("  ran init [name]         Create a new project (src/ layout)");
    println!("  ran repl                Interactive REPL");
    println!("  ran version             Show version info");
    println!("  ran help                Show this help");
    println!();
    println!("Build flags:");
    println!("  ran build -o myapp            Custom output name");
    println!("  ran build src/main.ran -o app Explicit entry + output");
    println!("  (default)                     Auto: native machine code when the program");
    println!("                                fits the native subset, else embed-source");
    println!("  --native | --aot              Force native codegen (E0606 if out of subset)");
    println!("  --embed                       Force the embed-source binary (bundles the interpreter)");
    println!("  --link-static                 Static-link native binaries where possible");
    println!();
    println!("Global flags:");
    println!("  --ownership=warn|strict       Ownership/borrow checking mode (default: warn)");
    println!("                                Override via env (fish: set -x RAN_OWNERSHIP strict)");
    println!("  --max-depth=<N>               Max Ran recursion depth before E1007 (default: 10000)");
    println!("  --no-vm | --interp            Force the tree-walking interpreter");
    println!("                                (default engine is the bytecode VM with auto-fallback)");
    println!("  --vm                          Use the VM verbosely (banner + fallback notes; same engine as default)");
    println!();
    println!("Notes:");
    println!("  - `ran build`/`run`/`test` auto-detect the entry (src/main.ran).");
    println!("  - `ran build` auto-fills ran.toml [dependencies] from your imports.");
    println!();
    println!("Examples:");
    println!("  ran init myproject && cd myproject && ran run");
    println!("  ran build && ./<name>");
    println!("  ran test");
    println!();
    println!("Compiled binaries are fully standalone - no runtime dependencies.");
}

/// Read the `entry = "..."` value from `[build]` in ran.toml, if any.
fn read_manifest_entry() -> Option<String> {
    let text = fs::read_to_string("ran.toml").ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("entry") {
            if let Some(eq) = rest.find('=') {
                let val = rest[eq + 1..].trim().trim_matches('"').to_string();
                if !val.is_empty() {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Conservative VM-eligibility pre-flight (R9.4). The Bytecode_VM is the default
/// execution engine, but a few interpreter behaviors are not yet reproduced
/// identically by the VM. Until the VM clears the full equivalence/regression
/// gate (task 12.5 / R9.5), this restricts the VM to the subset it runs exactly
/// like the tree-walking interpreter and falls back for everything else. Falling
/// back is always correct (the interpreter is authoritative), so this can only
/// make execution *more* conservative, never wrong.
///
/// Excluded (→ interpreter) because the VM does not match the interpreter yet:
///   * `let` bindings declared *inside* a control-flow block (if/for/while/
///     match-arm/spawn body): block- and loop-locals must not leak past the
///     block; the VM currently leaks them.
///   * `&`/`&mut` reference parameters and `&`/`&mut`/`*` reference operators:
///     `&mut` write-back to the caller is interpreter-only today.
///   * closures (`fn(...) { ... }`): captured-scope semantics differ.
fn vm_can_run(program: &frontend::ast::Program) -> bool {
    program.statements.iter().all(|s| stmt_vm_safe(s, false))
}

/// Walk a statement for VM-equivalence safety. `in_block` is true when the
/// statement sits inside a nested control-flow block, where a `let` would leak
/// in the VM (so a `VarDecl` there forces the interpreter). A `let` at a
/// function-body top level (or program top level) is fine.
fn stmt_vm_safe(stmt: &frontend::ast::Stmt, in_block: bool) -> bool {
    use frontend::ast::Statement::*;
    match &stmt.kind {
        VarDecl { value, .. } => {
            if in_block {
                return false; // block/loop-local: leaks in the VM today
            }
            expr_vm_safe(value)
        }
        FnDecl { params, body, .. } => {
            if params.iter().any(param_has_ref) {
                return false; // &mut write-back is interpreter-only
            }
            body.iter().all(|s| stmt_vm_safe(s, false))
        }
        ImplBlock { methods, .. } | TraitDecl { methods, .. } => {
            methods.iter().all(|s| stmt_vm_safe(s, false))
        }
        Expr(e) | Echo { expr: e, .. } => expr_vm_safe(e),
        Return(opt) => opt.as_ref().map_or(true, expr_vm_safe),
        If { condition, then_body, else_body } => {
            expr_vm_safe(condition)
                && then_body.iter().all(|s| stmt_vm_safe(s, true))
                && else_body
                    .as_ref()
                    .map_or(true, |b| b.iter().all(|s| stmt_vm_safe(s, true)))
        }
        For { iterable, body, .. } => {
            expr_vm_safe(iterable) && body.iter().all(|s| stmt_vm_safe(s, true))
        }
        While { condition, body } => {
            expr_vm_safe(condition) && body.iter().all(|s| stmt_vm_safe(s, true))
        }
        Spawn { body } => body.iter().all(|s| stmt_vm_safe(s, true)),
        // Pure declarations / control signals: no scoping or write-back concern.
        StructDecl { .. } | EnumDecl { .. } | Import { .. } | Break | Continue => true,
    }
}

/// Walk an expression for VM-equivalence safety: reject reference/deref
/// operators (drive `&mut` write-back) and closures (captured-scope semantics),
/// recursing into every sub-expression and nested block.
fn expr_vm_safe(expr: &frontend::ast::Expression) -> bool {
    use frontend::ast::Expression::*;
    use frontend::ast::UnaryOperator;
    match expr {
        IntLiteral(_) | FloatLiteral(_) | StringLiteral(_) | BoolLiteral(_) | Variable(_) => true,
        BinaryOp { left, right, .. } => expr_vm_safe(left) && expr_vm_safe(right),
        UnaryOp { op, operand } => {
            if matches!(
                op,
                UnaryOperator::Ref | UnaryOperator::MutRef | UnaryOperator::Deref
            ) {
                return false;
            }
            expr_vm_safe(operand)
        }
        FnCall { callee, args } => expr_vm_safe(callee) && args.iter().all(expr_vm_safe),
        MethodCall { object, args, .. } => expr_vm_safe(object) && args.iter().all(expr_vm_safe),
        FieldAccess { object, .. } => expr_vm_safe(object),
        Index { object, index } => expr_vm_safe(object) && expr_vm_safe(index),
        Pipe { left, right } => expr_vm_safe(left) && expr_vm_safe(right),
        ChanSend { channel, value } => expr_vm_safe(channel) && expr_vm_safe(value),
        ChanRecv { channel } => expr_vm_safe(channel),
        Lambda { .. } => false, // closures: captured-scope semantics differ
        StructInit { fields, .. } => fields.iter().all(|(_, v)| expr_vm_safe(v)),
        Array(items) => items.iter().all(expr_vm_safe),
        Await(inner) => expr_vm_safe(inner),
        Match { subject, arms } => {
            expr_vm_safe(subject)
                && arms
                    .iter()
                    .all(|a| a.body.iter().all(|s| stmt_vm_safe(s, true)))
        }
    }
}

/// True if a parameter is a reference (`&T` / `&mut T`): such parameters carry
/// caller write-back semantics the VM does not reproduce yet.
fn param_has_ref(p: &frontend::ast::Param) -> bool {
    matches!(
        p.type_annotation,
        Some(frontend::ast::TypeExpr::Ref { .. })
    )
}

/// Default bytecode-VM execution path (R9.4). Compiles the program to bytecode
/// and runs it on the safety-bounded VM (step budget + stack cap + panic guard,
/// so it can never loop forever or leak). Returns `true` only if the VM actually
/// executed the program to completion; otherwise (silently, unless `--vm` was
/// passed) returns `false` so the caller falls back to the interpreter. The VM
/// does not yet implement every construct (e.g. it excludes programs with
/// user-defined functions via the `all_supported` pre-flight), so the
/// interpreter remains the authoritative fallback engine.
///
/// Hardening (R6.3 / R6.4) — the `--vm` path must NEVER crash the process and
/// must always produce the correct result:
///
///   * R6.4: bytecode *compilation* (and execution) is wrapped in
///     `catch_unwind`. A panic anywhere inside — including a compiler panic on
///     an unsupported AST shape — is caught here and turns into a clean
///     fall-back to the interpreter, never a process crash.
///   * R6.3: an unsupported construct (detected by the `all_supported`
///     pre-flight) or any recoverable `Err` returned by the VM (including the
///     bounded `E1008` step-budget and `E1009` stack-cap faults from task 7.1,
///     or an unimplemented opcode hit mid-run) makes this return `false` so the
///     caller re-runs the program on the tree-walking interpreter.
///
/// Output correctness: the VM buffers everything `echo`/`print` emit (see
/// `VM::take_output`). We flush that buffer to stdout *only* on a clean run.
/// If the VM falls back after emitting partial output (e.g. a runaway loop that
/// trips `E1008` after printing), the buffer is discarded so the interpreter
/// re-run does not duplicate that output.
fn run_via_vm(program: &frontend::ast::Program) -> bool {
    let verbose = VM_VERBOSE.load(std::sync::atomic::Ordering::Relaxed);
    // Conservative eligibility pre-flight (R9.4): the Bytecode_VM is the default
    // engine, but it does not yet reproduce every interpreter behavior exactly.
    // Programs outside its known-equivalent subset fall back transparently to
    // the interpreter (which remains authoritative) BEFORE any work or banner.
    if !vm_can_run(program) {
        if verbose {
            eprintln!("\x1b[33m[ran]\x1b[0m --vm: program uses constructs not yet equivalently supported by the VM — using the interpreter");
        }
        return false;
    }
    if verbose {
        eprintln!("\x1b[33m[ran]\x1b[0m \x1b[1m--vm\x1b[0m bytecode VM (default engine; bounded; falls back to the interpreter when unsupported)");
    }

    // Catch *any* panic from compilation or execution (R6.4). The empty hook
    // keeps the fallback quiet; the previous hook is always restored, even if
    // the closure unwinds.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Compilation runs inside the guard so a compiler panic (R6.4) is
        // caught and becomes a fall-back rather than a crash.
        let result = backend::vm::BytecodeCompiler::compile(program);
        // Pre-flight: only run on the VM if every opcode is implemented, so the
        // VM never emits partial or wrong output for a program it cannot fully
        // execute. Otherwise signal "unsupported" and fall back (R6.3).
        if !backend::vm::all_supported(&result.chunks) {
            return Err("uses constructs not yet implemented in the VM".to_string());
        }
        let mut vm = backend::vm::VM::new();
        vm.chunks = result.chunks;
        vm.global_names = result.global_names;
        // Run the top-level program. (Programs with functions are excluded by
        // the pre-flight check above and run on the interpreter.) On success,
        // hand back the buffered output so the driver can flush it exactly once.
        vm.run().map(|()| vm.take_output())
    }));
    std::panic::set_hook(prev);
    match outcome {
        // Clean run: flush the program's buffered stdout exactly once, then
        // report success so the caller does NOT also run the interpreter.
        Ok(Ok(output)) => {
            print!("{}", output);
            let _ = io::stdout().flush();
            true
        }
        // Recoverable VM error (unsupported construct, E1008/E1009, or an
        // unimplemented opcode hit mid-run): discard any buffered output and
        // fall back to the interpreter (R6.3).
        Ok(Err(reason)) => {
            if verbose {
                eprintln!("\x1b[33m[ran]\x1b[0m --vm: {} — using the interpreter", reason);
            }
            false
        }
        // A panic during compilation or execution (R6.4): caught here, so the
        // process stays alive and we fall back to the interpreter.
        Err(_) => {
            if verbose {
                eprintln!("\x1b[33m[ran]\x1b[0m --vm hit an internal error — using the interpreter");
            }
            false
        }
    }
}

fn execute_file(filename: &str, use_interp: bool, ownership_mode: OwnershipMode) {
    let source = match fs::read_to_string(filename) {
        Ok(content) => content,
        Err(e) => {
            eprintln!("ran: cannot read '{}': {}", filename, e);
            process::exit(1);
        }
    };

    // Resolve module imports (merges local imported files into one program)
    if let Some(program) = modules::load_program(filename) {
        let checked = compiler::analyze_with_file(&program, filename, &source, ownership_mode);
        // VM is the default engine (R9.4); `use_interp` (compiled binaries) or
        // `--no-vm`/`--interp` force the tree-walking interpreter. On a clean VM
        // run `run_via_vm` flushes the buffered output and returns true; on any
        // unsupported construct / recoverable fault / compile panic it returns
        // false and we fall back transparently to the interpreter below.
        if !use_interp && USE_VM.load(std::sync::atomic::Ordering::Relaxed) && run_via_vm(&program) {
            return;
        }
        run_checked_on_big_stack(checked);
    } else {
        run_source(&source, filename, use_interp, ownership_mode);
    }
}

fn run_source(source: &str, filename: &str, use_interp: bool, ownership_mode: OwnershipMode) {
    let tokens = lexer::tokenize(source);
    let (program, syntax_diags) = parser::parse_checked(tokens);
    support::diagnostics::abort_on_syntax_errors(syntax_diags, filename, source);
    let checked = compiler::analyze_with_file(&program, filename, source, ownership_mode);

    if !checked.has_main && !program.statements.is_empty() {
        // Check if there are only top-level expressions (allow for scripting)
        let has_fns = program.statements.iter().any(|s| matches!(s.kind, crate::frontend::ast::Statement::FnDecl { .. }));
        if has_fns {
            eprintln!("\x1b[31;1merror\x1b[0m: no main() function found");
            eprintln!("  \x1b[36m= help\x1b[0m: add `fn main() {{ ... }}` as the entry point");
            process::exit(1);
        }
    }

    // VM is the default engine (R9.4); `use_interp` (compiled binaries) or
    // `--no-vm`/`--interp` force the tree-walking interpreter. Fallback is
    // automatic and transparent (see `run_via_vm`).
    if !use_interp && USE_VM.load(std::sync::atomic::Ordering::Relaxed) && run_via_vm(&program) {
        return;
    }
    run_checked_on_big_stack(checked);
}

#[allow(dead_code)]
fn compile_file(_filename: &str, _output: &str) {
    // Handled inline in cmd_build now
}

/// Check if this binary has embedded encrypted .ran source appended to it.
fn extract_embedded_program() -> Option<Vec<u8>> {
    let exe_path = env::current_exe().ok()?;
    let data = fs::read(&exe_path).ok()?;
    codegen::extract_embedded_source(&data).map(|s| s.into_bytes())
}

/// Run embedded .ran program (compiled binary mode)
fn run_embedded(source_bytes: &[u8]) {
    let source = match String::from_utf8(source_bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("ran: corrupted embedded program");
            process::exit(1);
        }
    };
    // Compiled binaries use interpreter (full stdlib including http.listen).
    // Embedded programs were already checked at `ran build` time; run them in
    // the default ownership mode for this release.
    run_source(&source, "<compiled>", true, OwnershipMode::DEFAULT);
}

#[cfg(test)]
mod tests {
    use super::parse_byte_size;
    use super::extract_max_depth;
    use crate::support::pbt;

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    // -- plain byte counts (R5.2) -----------------------------------------

    #[test]
    fn plain_byte_count_parses_as_bytes() {
        assert_eq!(parse_byte_size("0"), Some(0));
        assert_eq!(parse_byte_size("536870912"), Some(536_870_912));
        assert_eq!(parse_byte_size("1"), Some(1));
    }

    // -- 1024-based suffixes, case-insensitive (R5.2) ---------------------

    #[test]
    fn suffixes_are_1024_based_and_case_insensitive() {
        // Two-letter forms.
        assert_eq!(parse_byte_size("512MB"), Some(512 * MIB));
        assert_eq!(parse_byte_size("2GB"), Some(2 * GIB));
        assert_eq!(parse_byte_size("4KB"), Some(4 * KIB));
        // Bare one-letter forms.
        assert_eq!(parse_byte_size("512M"), Some(512 * MIB));
        assert_eq!(parse_byte_size("2G"), Some(2 * GIB));
        assert_eq!(parse_byte_size("4K"), Some(4 * KIB));
        // Case-insensitive + surrounding whitespace.
        assert_eq!(parse_byte_size("512mb"), Some(512 * MIB));
        assert_eq!(parse_byte_size("2gB"), Some(2 * GIB));
        assert_eq!(parse_byte_size("  1Gb  "), Some(GIB));
    }

    // -- malformed input is total: None, never panic (R5.2 / Property 10) -

    #[test]
    fn malformed_input_returns_none() {
        assert_eq!(parse_byte_size(""), None);
        assert_eq!(parse_byte_size("   "), None);
        assert_eq!(parse_byte_size("abc"), None);
        assert_eq!(parse_byte_size("12x"), None);
        assert_eq!(parse_byte_size("-5"), None);
        assert_eq!(parse_byte_size("MB"), None); // suffix with no number
        assert_eq!(parse_byte_size("1.5GB"), None); // no fractional support
    }

    // -- multiplication overflow saturates to None, never wraps -----------

    #[test]
    fn overflow_returns_none() {
        // u64::MAX gigabytes overflows u64 when multiplied by 1024^3.
        assert_eq!(parse_byte_size(&format!("{}GB", u64::MAX)), None);
        assert_eq!(parse_byte_size(&format!("{}G", u64::MAX)), None);
    }

    // ====================================================================
    // Task 1.6 — recursion-depth default + `--max-depth` flag handling.
    //
    // Validates Requirement 1.5: an invalid/missing `--max-depth` is
    // recoverable — the flag is still consumed (removed from the argument
    // vector) and the call never panics or exits the process.
    //
    // This test is deliberately constrained to the race-free, purely-local
    // behavior of `extract_max_depth` and never mutates the process-global
    // `MAX_CALL_DEPTH` (see the in-body note for why). The compiled-in default
    // (R1.3) and the "valid flag is applied" behavior are covered by the
    // runtime depth tests, which serialize on the shared `DEPTH_TEST_LOCK`.
    // ====================================================================

    /// `extract_max_depth` consumes every `--max-depth` flag form (removing it
    /// from args so downstream parsing is unaffected) and treats an invalid,
    /// non-positive, or missing value as recoverable: no panic, no process
    /// exit, and the global recursion limit is left untouched (R1.5).
    #[test]
    fn depth_default_and_flag_handling() {
        // Test-isolation note (intermittent-failure fix):
        //
        // The PREFERRED isolation is to serialize this test against the shared
        // depth-test lock used by the runtime recursion-guard tests
        // (`crate::runtime::frame::recursion_guard_tests::DEPTH_TEST_LOCK`),
        // holding it across the whole body and restoring the default while
        // still holding it — exactly as those tests do. That lock is not
        // reachable from this module, though: `frame` is declared `mod frame;`
        // (private) in `src/runtime/mod.rs`, so the `pub(crate)` lock cannot be
        // named through the private module boundary, and the fix is scoped to
        // this file only.
        //
        // So, per the fallback strategy, this test is made robust WITHOUT the
        // shared lock by NEVER mutating the process-global `MAX_CALL_DEPTH`.
        // It therefore exercises `extract_max_depth` only with values that take
        // the recoverable path (invalid / non-positive / missing), which leave
        // the global untouched and so cannot race a concurrent runtime depth
        // test. Crucially it must NOT use a *valid* `--max-depth=<N>` here: that
        // would call `set_max_call_depth` and, racing the recursion-guard test
        // (which lowers the limit to 64 and then recurses unbounded expecting
        // the guard to fire), could raise the limit mid-recursion and blow the
        // OS stack. We assert only the race-free, purely-local behavior: that
        // each flag form is consumed/removed from the argument vector and that
        // invalid/missing values are recoverable (the call returns normally —
        // no panic, no process exit) per R1.5. Verifying that a *valid* flag is
        // actually applied to the global is covered by the runtime depth tests,
        // which hold `DEPTH_TEST_LOCK` while doing so.

        // -- R1.5: a non-integer value is recoverable; flag consumed, global
        //    left untouched (the `_ =>` branch only emits a diagnostic) -------
        let mut args = vec![
            "ran".to_string(),
            "--max-depth=abc".to_string(),
            "app.ran".to_string(),
        ];
        extract_max_depth(&mut args);
        assert_eq!(args, vec!["ran".to_string(), "app.ran".to_string()]);

        // -- R1.5: a non-positive value (0) is recoverable; flag consumed,
        //    global left untouched -------------------------------------------
        let mut args = vec![
            "ran".to_string(),
            "--max-depth=0".to_string(),
            "app.ran".to_string(),
        ];
        extract_max_depth(&mut args);
        assert_eq!(args, vec!["ran".to_string(), "app.ran".to_string()]);

        // A missing value is likewise recoverable: flag consumed, no panic/exit,
        // global left untouched.
        let mut args = vec!["ran".to_string(), "--max-depth".to_string()];
        extract_max_depth(&mut args);
        assert_eq!(args, vec!["ran".to_string()]);
    }

    // ====================================================================
    // Task 8.5 — Property 10: byte-size parsing is correct and total.
    //
    // Feature: memory-safe-self-hosting, Property 10: Byte-size parsing is
    // correct and total — for a random non-negative `n` and a valid 1024-based
    // unit suffix (K/M/G, KB/MB/GB, case-insensitive) `parse_byte_size` returns
    // `n * 1024^k` (or `None` on overflow); for any string input it is total
    // (returns `None` rather than panicking).
    //
    // Validates: Requirements 5.2
    // ====================================================================

    /// Map a suffix variant index to (suffix text, power-of-1024 exponent).
    fn variant_suffix(v: u8) -> (&'static str, u32) {
        match v {
            0 => ("", 0),
            1 => ("K", 1),
            2 => ("KB", 1),
            3 => ("M", 2),
            4 => ("MB", 2),
            5 => ("G", 3),
            _ => ("GB", 3),
        }
    }

    /// Generator for a well-formed byte-size case:
    /// `(n, variant, uppercase_suffix, surrounding_padding)`.
    fn byte_size_case() -> pbt::Gen<(u64, u8, bool, bool)> {
        pbt::Gen::new(
            |rng, _size| {
                // Bias toward boundary magnitudes plus full-range values so we
                // exercise both exact multiples and overflow.
                let n: u64 = if rng.below(3) == 0 {
                    *rng.choose(&[0u64, 1, 2, 1023, 1024, 1025, u64::MAX])
                } else {
                    rng.next_u64() >> rng.below(64)
                };
                let variant = rng.below(7) as u8;
                let upper = rng.boolean();
                let pad = rng.boolean();
                (n, variant, upper, pad)
            },
            |&(n, v, up, pad)| {
                let mut out = Vec::new();
                if n != 0 {
                    out.push((0u64, v, up, pad));
                    out.push((n / 2, v, up, pad));
                }
                if v != 0 {
                    out.push((n, 0u8, up, pad));
                }
                out
            },
        )
    }

    // Feature: memory-safe-self-hosting, Property 10: Byte-size parsing is
    // correct and total.
    // Validates: Requirements 5.2
    #[test]
    fn prop_byte_size_parse_correct_and_total() {
        // (a) Well-formed inputs parse to exactly `n * 1024^k`, or `None` when
        //     the multiplication overflows u64 — never a wrapped value.
        pbt::for_all(
            "parse_byte_size: n * 1024^k (or None on overflow)",
            &byte_size_case(),
            |&(n, v, up, pad): &(u64, u8, bool, bool)| {
                let (suf, k) = variant_suffix(v);
                let suf = if up {
                    suf.to_string()
                } else {
                    suf.to_ascii_lowercase()
                };
                let mut s = format!("{}{}", n, suf);
                if pad {
                    // Surrounding whitespace must be ignored (helper trims).
                    s = format!("  {}  ", s);
                }
                // k <= 3, so 1024^k fits comfortably in u64.
                let mult = 1024u64.pow(k);
                let expected = n.checked_mul(mult);
                parse_byte_size(&s) == expected
            },
        );

        // (b) Totality: parsing is defined for ANY string — it returns a value
        //     (Some/None) and never panics. The harness turns a panic into a
        //     property failure, so reaching `true` for every case proves the
        //     function is total (covers malformed inputs returning None).
        pbt::for_all(
            "parse_byte_size: total over arbitrary strings (never panics)",
            &pbt::string(16),
            |s: &String| {
                let _ = parse_byte_size(s);
                true
            },
        );
    }
}
