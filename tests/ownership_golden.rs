//! Golden example tests for the ownership rollout (`--ownership=warn|strict`).
//!
//! These lock in the *observable* behavior of the ownership migration so future
//! changes cannot silently alter which diagnostics are emitted, at what
//! severity, or what a program prints/returns. Each curated fixture is run
//! through the real `ran` binary under BOTH modes and compared against stored
//! golden expectations.
//!
//! Fixtures (written to `.tmp_tests/`, gitignored, cleaned up after each run):
//!   * use-after-move  → `E0210`            (Requirements 10.2, 10.3)
//!   * borrow conflict → `E0212`            (Requirements 11.2, 11.4)
//!   * `&mut` write-back observed by caller (Requirement 11.6)
//!   * unsynchronized captured write → `E0613` (Requirement 14.6)
//!
//! Golden granularity (deliberate choice). Comparing the *entire* stderr byte
//! stream would be brittle: the help/fix hints are localized prose that may be
//! reworded without any behavior change. Instead each golden pins the stable,
//! meaningful signals that define the contract:
//!   1. exit code            (0 for `warn`, non-zero for `strict` on violations)
//!   2. the severity+code header (`warning[E0210]:` vs `error[E0210]:`) — this
//!      catches an unintended severity flip OR a changed/removed code
//!   3. the program's stdout (does it run to completion, and what does it print)
//!   4. mode-specific anchors: the `warn` migration summary line, the strict
//!      `aborting due to ... error` line, and the `&mut` write-back note
//! A change to any of these fails the test (the intent of a golden), while
//! pure wording tweaks to the localized hint text stay green.

use std::io::Write;
use std::process::{Command, Output};

/// Resolve the workspace `.tmp_tests/` dir (cargo runs tests at the crate root).
fn tmp_dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp_tests");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn nonce() -> u128 {
    // Collision-free across parallel test binaries: high bits = process id,
    // low bits = a per-process atomic counter. A bare nanosecond timestamp
    // could collide across binaries and clobber each other's temp file.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    ((std::process::id() as u128) << 64) | (COUNTER.fetch_add(1, Ordering::Relaxed) as u128)
}

/// Write `src` to a uniquely-named fixture under `.tmp_tests/`, run
/// `ran run --ownership=<mode> <fixture>`, then delete the fixture.
fn run_mode(src: &str, mode: &str) -> Output {
    let path = tmp_dir().join(format!("own_golden_{}_{}.ran", mode, nonce()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_ran"))
        .arg("run")
        .arg(format!("--ownership={}", mode))
        .arg(&path)
        .output()
        .expect("failed to run ran");
    let _ = std::fs::remove_file(&path);
    out
}

/// Strip ANSI SGR color escapes (`ESC [ ... m`) so colorized diagnostics
/// normalize to plain text. std-only, no regex.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip until the final byte of the escape (a letter), inclusive.
            i += 2;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // consume the final letter (e.g. 'm')
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn stdout_norm(o: &Output) -> String {
    strip_ansi(&String::from_utf8_lossy(&o.stdout))
}
fn stderr_norm(o: &Output) -> String {
    strip_ansi(&String::from_utf8_lossy(&o.stderr))
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

// ---------------------------------------------------------------------------
// Curated fixtures
// ---------------------------------------------------------------------------

/// Use-after-move: `s` (a `str`, non-`Copy`) is moved into `t`, then read.
const FIXTURE_USE_AFTER_MOVE: &str = r#"fn main() {
    let s = "alpha"
    let t = s
    echo t
    echo s
}
"#;

/// Borrow conflict: take `&mut v` while a shared `&v` borrow is still live.
const FIXTURE_BORROW_CONFLICT: &str = r#"fn main() {
    let mut v = [1, 2, 3]
    let a = &v
    let b = &mut v
    echo "done"
}
"#;

/// `&mut` write-back: the callee mutates its `&mut` parameter; the caller must
/// observe the new value after the call (R11.6). Clean program (no violations).
const FIXTURE_MUT_WRITEBACK: &str = r#"fn bump(n: &mut int) {
    n = n + 1
}

fn main() {
    let mut x = 41
    bump(&mut x)
    echo x
}
"#;

/// Data race: a `spawn` block captures `total` and writes it without any
/// synchronization wrapper (R14.6 → `E0613`).
const FIXTURE_DATA_RACE: &str = r#"fn main() {
    let mut total = 0
    spawn {
        total = total + 1
    }
    echo "spawned"
}
"#;

// ---------------------------------------------------------------------------
// E0210 — use-after-move (Requirements 10.2, 10.3)
// ---------------------------------------------------------------------------

#[test]
fn use_after_move_warn_downgrades_and_runs() {
    let o = run_mode(FIXTURE_USE_AFTER_MOVE, "warn");
    let err = stderr_norm(&o);
    // warn mode: violation is a *warning*, program runs to completion (exit 0).
    assert_eq!(code(&o), 0, "stderr: {}", err);
    assert!(err.contains("warning[E0210]:"), "stderr: {}", err);
    assert!(!err.contains("error[E0210]:"), "must not be an error in warn: {}", err);
    // Migration-readiness summary anchors the warn-mode contract.
    assert!(
        err.contains("ownership summary (warn mode): E0210=1 E0212=0 E0214=0 E0215=0 E0613=0"),
        "stderr: {}",
        err
    );
    // Program still executed: both echoes printed.
    assert_eq!(stdout_norm(&o), "alpha\nalpha\n", "stderr: {}", err);
}

#[test]
fn use_after_move_strict_aborts_before_running() {
    let o = run_mode(FIXTURE_USE_AFTER_MOVE, "strict");
    let err = stderr_norm(&o);
    // strict mode: hard error, abort before runtime (non-zero exit, no stdout).
    assert_ne!(code(&o), 0, "strict must abort; stderr: {}", err);
    assert!(err.contains("error[E0210]:"), "stderr: {}", err);
    assert!(err.contains("aborting due to"), "stderr: {}", err);
    assert_eq!(stdout_norm(&o), "", "program must not run in strict: {}", err);
}

// ---------------------------------------------------------------------------
// E0212 — borrow conflict (Requirements 11.2, 11.4)
// ---------------------------------------------------------------------------

#[test]
fn borrow_conflict_warn_downgrades_and_runs() {
    let o = run_mode(FIXTURE_BORROW_CONFLICT, "warn");
    let err = stderr_norm(&o);
    assert_eq!(code(&o), 0, "stderr: {}", err);
    assert!(err.contains("warning[E0212]:"), "stderr: {}", err);
    assert!(
        err.contains("ownership summary (warn mode): E0210=0 E0212=1 E0214=0 E0215=0 E0613=0"),
        "stderr: {}",
        err
    );
    assert_eq!(stdout_norm(&o), "done\n", "stderr: {}", err);
}

#[test]
fn borrow_conflict_strict_aborts_before_running() {
    let o = run_mode(FIXTURE_BORROW_CONFLICT, "strict");
    let err = stderr_norm(&o);
    assert_ne!(code(&o), 0, "strict must abort; stderr: {}", err);
    assert!(err.contains("error[E0212]:"), "stderr: {}", err);
    assert!(err.contains("aborting due to"), "stderr: {}", err);
    assert_eq!(stdout_norm(&o), "", "program must not run in strict: {}", err);
}

// ---------------------------------------------------------------------------
// &mut write-back observed by the caller (Requirement 11.6)
// ---------------------------------------------------------------------------

#[test]
fn mut_writeback_warn_writes_back_and_notes() {
    let o = run_mode(FIXTURE_MUT_WRITEBACK, "warn");
    let err = stderr_norm(&o);
    assert_eq!(code(&o), 0, "stderr: {}", err);
    // Caller observes the mutation: 41 -> 42.
    assert_eq!(stdout_norm(&o), "42\n", "stderr: {}", err);
    // warn mode surfaces the one-line write-back note and counts the &mut site.
    assert!(
        err.contains("now writes back to the caller"),
        "expected &mut write-back note; stderr: {}",
        err
    );
    assert!(err.contains("&mut sites=1"), "stderr: {}", err);
}

#[test]
fn mut_writeback_strict_writes_back_silently() {
    let o = run_mode(FIXTURE_MUT_WRITEBACK, "strict");
    let err = stderr_norm(&o);
    // Clean program: no violations, write-back still active in all phases.
    assert_eq!(code(&o), 0, "stderr: {}", err);
    assert_eq!(stdout_norm(&o), "42\n", "stderr: {}", err);
    // The informational note is warn-only; it must not appear under strict.
    assert!(
        !err.contains("now writes back to the caller"),
        "write-back note must be warn-only; stderr: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// E0613 — unsynchronized captured write / data race (Requirement 14.6)
// ---------------------------------------------------------------------------

#[test]
fn data_race_warn_downgrades_and_runs() {
    let o = run_mode(FIXTURE_DATA_RACE, "warn");
    let err = stderr_norm(&o);
    assert_eq!(code(&o), 0, "stderr: {}", err);
    assert!(err.contains("warning[E0613]:"), "stderr: {}", err);
    assert!(
        err.contains("ownership summary (warn mode): E0210=0 E0212=0 E0214=0 E0215=0 E0613=1"),
        "stderr: {}",
        err
    );
    assert!(stdout_norm(&o).contains("spawned"), "stderr: {}", err);
}

#[test]
fn data_race_strict_aborts_before_running() {
    let o = run_mode(FIXTURE_DATA_RACE, "strict");
    let err = stderr_norm(&o);
    assert_ne!(code(&o), 0, "strict must abort; stderr: {}", err);
    assert!(err.contains("error[E0613]:"), "stderr: {}", err);
    assert!(err.contains("aborting due to"), "stderr: {}", err);
    assert_eq!(stdout_norm(&o), "", "program must not run in strict: {}", err);
}
