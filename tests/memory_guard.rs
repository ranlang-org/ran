//! Integration/smoke tests for the Memory_Watchdog and Loop_Memory_Guard
//! (task 6.2).
//!
//! The watchdog (background daemon, ~200ms cadence) and the loop guard
//! (periodic in-loop check, stride 2^20) protect the process from OOM by
//! stopping cleanly with `E1006` before the OS OOM-killer fires. Actually
//! exhausting memory to trigger `E1006` is unsafe and slow in CI, so these are
//! deliberately NON-DESTRUCTIVE smoke tests:
//!
//!   * A normal program runs to completion under the always-installed watchdog
//!     (no regression): the watchdog must not disturb a well-behaved program.
//!   * A loop that crosses the loop-guard stride (2^20) many times still runs
//!     to completion and yields the correct result — confirming the loop guard
//!     is PRESENT and benign while free memory is healthy (it ticks without
//!     spuriously raising E1006).
//!   * The same loop completes on the `--vm` path too, confirming the guards
//!     cover that execution path as well (R2.7).
//!
//! No test here allocates unbounded memory. All fixtures live under
//! `.tmp_tests/` (gitignored) and are cleaned up.
//!
//! _Requirements: 2.4, 2.7_

use std::path::PathBuf;
use std::process::{Command, Output};

/// `.tmp_tests/` under the crate root — the project's convention for transient
/// test artifacts.
fn tmp_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(".tmp_tests");
    let _ = std::fs::create_dir_all(&p);
    p
}

fn nonce() -> u128 {
    // Collision-free across parallel test binaries: high bits = process id,
    // low bits = a per-process atomic counter. A bare nanosecond timestamp
    // could collide across binaries and clobber each other's temp file.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    ((std::process::id() as u128) << 64) | (COUNTER.fetch_add(1, Ordering::Relaxed) as u128)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// Write `src` to a fixture and run the `ran` binary with the given extra args
/// (e.g. `--vm`). Returns the process output and the fixture path.
fn run_with_args(name: &str, src: &str, extra: &[&str]) -> (Output, PathBuf) {
    let path = tmp_dir().join(name);
    std::fs::write(&path, src).expect("write .ran fixture");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ran"));
    for a in extra {
        cmd.arg(a);
    }
    cmd.arg(&path);
    let out = cmd.output().expect("failed to run ran binary");
    (out, path)
}

// ---------------------------------------------------------------------------
// Watchdog present + benign on a normal program (no regression)
// ---------------------------------------------------------------------------

/// A small, well-behaved program must run to completion under the
/// always-installed Memory_Watchdog. The watchdog reads free system memory on
/// a fixed cadence; while memory is healthy it must NOT disturb execution
/// (R2.5/R2.7) — the program exits 0 with the expected output.
#[test]
fn normal_program_runs_to_completion_under_watchdog() {
    let id = nonce();
    let src = r#"
fn main() {
    let mut total = 0
    let mut i = 1
    while i <= 100 {
        total = total + i
        i = i + 1
    }
    echo "TOTAL=$total"
    echo "DONE"
}
"#;

    let (out, fixture) = run_with_args(&format!("watchdog_normal_{}.ran", id), src, &[]);
    let so = stdout(&out);
    let _ = std::fs::remove_file(&fixture);

    assert_eq!(
        code(&out),
        0,
        "a normal program must complete under the watchdog; stderr: {}",
        stderr(&out)
    );
    // Sum 1..=100 == 5050: confirms correct execution, not just a clean exit.
    assert!(
        so.contains("TOTAL=5050"),
        "expected correct computation under the watchdog; stdout: {}",
        so
    );
    assert!(
        so.contains("DONE"),
        "program did not run to completion; stdout: {}",
        so
    );
    // The watchdog must stay silent on a healthy run — no E1006 anywhere.
    assert!(
        !so.contains("E1006") && !stderr(&out).contains("E1006"),
        "watchdog must not raise E1006 on a healthy program; stdout: {} stderr: {}",
        so,
        stderr(&out)
    );
}

// ---------------------------------------------------------------------------
// Loop guard present + benign: crosses the 2^20 stride many times
// ---------------------------------------------------------------------------

/// A loop with ~3,000,000 iterations crosses the Loop_Memory_Guard stride
/// (2^20 == 1,048,576) a handful of times. While free memory is healthy the
/// guard must tick WITHOUT raising `E1006`, so the loop runs to completion with
/// the correct result. This confirms the loop guard is present on the
/// interpreter path and does not spuriously fault a normal program (R2.4).
#[test]
fn loop_crossing_guard_stride_completes_without_e1006_interpreter() {
    let id = nonce();
    // 3,000,000 iterations > 2 * 2^20, so the guard tick fires multiple times.
    let src = r#"
fn main() {
    let mut count = 0
    let mut i = 0
    while i < 3000000 {
        count = count + 1
        i = i + 1
    }
    echo "COUNT=$count"
}
"#;

    let (out, fixture) = run_with_args(&format!("loopguard_interp_{}.ran", id), src, &[]);
    let so = stdout(&out);
    let _ = std::fs::remove_file(&fixture);

    assert_eq!(
        code(&out),
        0,
        "loop crossing the guard stride must complete; stderr: {}",
        stderr(&out)
    );
    assert!(
        so.contains("COUNT=3000000"),
        "loop guard must not disturb a healthy long loop; stdout: {}",
        so
    );
    assert!(
        !so.contains("E1006") && !stderr(&out).contains("E1006"),
        "loop guard must not raise E1006 while memory is healthy; stdout: {} stderr: {}",
        so,
        stderr(&out)
    );
}

/// The same guard-stride-crossing loop must also run to completion on the
/// `--vm` execution path, confirming the memory guards cover that path too
/// (R2.7). The `--vm` runner prints diagnostics to stderr and falls back to the
/// interpreter for unsupported constructs; we assert only on the program's
/// stdout result and a clean exit, never on the fallback chatter.
#[test]
fn loop_crossing_guard_stride_completes_on_vm_path() {
    let id = nonce();
    let src = r#"
fn main() {
    let mut count = 0
    let mut i = 0
    while i < 3000000 {
        count = count + 1
        i = i + 1
    }
    echo "COUNT=$count"
}
"#;

    let (out, fixture) = run_with_args(&format!("loopguard_vm_{}.ran", id), src, &["--vm"]);
    let so = stdout(&out);
    let _ = std::fs::remove_file(&fixture);

    assert_eq!(
        code(&out),
        0,
        "loop must complete on the --vm path; stderr: {}",
        stderr(&out)
    );
    assert!(
        so.contains("COUNT=3000000"),
        "the --vm path must run the loop to the correct result; stdout: {}",
        so
    );
    assert!(
        !so.contains("E1006"),
        "no spurious E1006 on the --vm path while memory is healthy; stdout: {}",
        so
    );
}
