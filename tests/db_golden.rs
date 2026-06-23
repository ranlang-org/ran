//! Golden end-to-end integration test for the native `db` (SQLite) module
//! (task 3.12, Kelompok B).
//!
//! This drives the built `ran` binary on real `.ran` programs that exercise the
//! full database flow through the runtime layer: `connect -> exec -> query ->
//! commit/rollback`. It locks in language-visible behavior (R6.1/R6.2 connect,
//! R7.1/R7.3 query/exec, R9.3/R9.4 commit/rollback).
//!
//! Availability-skip: if the system `libsqlite3` is absent, `db.connect`
//! returns a handleable error value instead of a handle; the golden program
//! detects this, prints `SKIP`, and the test passes without asserting the DB
//! flow (CI stays green on machines without the library). `libsqlite3` IS
//! present on this machine, so the golden path is the one normally taken here.
//!
//! All artifacts (the generated `.ran` fixture and the temp database file) live
//! under `.tmp_tests/` (gitignored) and are cleaned up after each test.

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

/// Write `src` to `<.tmp_tests>/<name>` and run the `ran` binary on it.
fn run_program(name: &str, src: &str) -> (Output, PathBuf) {
    let path = tmp_dir().join(name);
    std::fs::write(&path, src).expect("write .ran fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_ran"))
        .arg(&path)
        .output()
        .expect("failed to run ran binary");
    (out, path)
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

/// Golden flow: connect -> exec(CREATE/INSERT with bound params) -> query
/// (assert rows) -> begin/commit (persists) -> begin/rollback (restores).
#[test]
fn db_golden_connect_exec_query_commit_rollback() {
    let id = nonce();
    let db_path = tmp_dir().join(format!("golden_{}.sqlite", id));
    // Start from a clean slate so row counts are deterministic.
    let _ = std::fs::remove_file(&db_path);
    let db_str = db_path.to_string_lossy().replace('\\', "/");

    let src = format!(
        r#"
import "std::db" as db

// Error values are maps carrying an `error: true` marker; a successful
// db.connect returns an opaque Int handle. typeof distinguishes them.
fn is_err(v) {{ return typeof(v) == "map" }}

fn main() {{
    let path = "{db}"
    let conn = db.connect(path)
    if is_err(conn) {{
        // libsqlite3 unavailable: skip cleanly (handleable error, no crash).
        echo "SKIP: libsqlite3 unavailable"
        return
    }}

    // exec CREATE (DDL) then parameterized INSERTs (values are bound, never
    // interpolated). exec returns affected-row count (>= 0).
    db.exec(conn, "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT, balance INTEGER)", [])
    db.exec(conn, "INSERT INTO accounts (name, balance) VALUES (?, ?)", ["alice", 100])
    db.exec(conn, "INSERT INTO accounts (name, balance) VALUES (?, ?)", ["bob", 50])

    // query returns an array of row maps; assert the round-tripped values.
    let rows = db.query(conn, "SELECT name, balance FROM accounts ORDER BY id", [])
    assert(len(rows) == 2, "expected exactly two seeded rows")
    let r0 = rows[0]
    let r1 = rows[1]
    assert(r0["name"] == "alice", "row 0 name should be alice")
    assert(r0["balance"] == 100, "row 0 balance should be 100")
    assert(r1["name"] == "bob", "row 1 name should be bob")
    assert(r1["balance"] == 50, "row 1 balance should be 50")

    // Parameterized SELECT filters by a bound value (R7.4 anti-injection path).
    let rich = db.query(conn, "SELECT name FROM accounts WHERE balance > ?", [75])
    assert(len(rich) == 1, "exactly one account with balance > 75")
    let rich0 = rich[0]
    assert(rich0["name"] == "alice", "the rich account is alice")

    // commit PERSISTS: insert inside a transaction, commit, confirm it stuck.
    db.begin(conn)
    db.exec(conn, "INSERT INTO accounts (name, balance) VALUES (?, ?)", ["carol", 30])
    db.commit(conn)
    let after_commit = db.query(conn, "SELECT name FROM accounts", [])
    assert(len(after_commit) == 3, "commit must persist the inserted row")

    // rollback RESTORES: insert inside a transaction, see it, roll back, and
    // confirm the prior state is restored exactly.
    db.begin(conn)
    db.exec(conn, "INSERT INTO accounts (name, balance) VALUES (?, ?)", ["dave", 10])
    let mid = db.query(conn, "SELECT name FROM accounts", [])
    assert(len(mid) == 4, "uncommitted row is visible inside its transaction")
    db.rollback(conn)
    let after_rollback = db.query(conn, "SELECT name FROM accounts", [])
    assert(len(after_rollback) == 3, "rollback must restore the pre-transaction state")

    db.close(conn)
    echo "GOLDEN_OK"
}}
"#,
        db = db_str
    );

    let (out, fixture) = run_program(&format!("db_golden_{}.ran", id), &src);
    let so = stdout(&out);

    // Clean up artifacts regardless of outcome.
    let _ = std::fs::remove_file(&fixture);
    let _ = std::fs::remove_file(&db_path);
    // WAL/SHM side files from `PRAGMA journal_mode=WAL`.
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-shm"));

    if so.contains("SKIP") {
        eprintln!("db golden flow skipped: libsqlite3 unavailable");
        return;
    }

    assert_eq!(code(&out), 0, "golden program should exit 0; stderr: {}", stderr(&out));
    assert!(
        so.contains("GOLDEN_OK"),
        "golden flow did not complete; stdout: {} stderr: {}",
        so,
        stderr(&out)
    );
}

/// `db.connect` on an uncreatable path returns a handleable error value
/// (`Map{error, code, ...}`) WITHOUT crashing the process (R6.x). This is also
/// the contract that makes the "library absent" fallback safe: the same
/// handleable-error shape is returned, so callers never face a crash.
#[test]
fn db_connect_bad_path_is_handleable_no_crash() {
    let id = nonce();
    // A path whose parent directory does not exist: SQLite cannot create the
    // file, so connect must fail gracefully rather than abort the interpreter.
    let bad = tmp_dir()
        .join(format!("no_such_dir_{}", id))
        .join("nested")
        .join("cannot_create.sqlite");
    let bad_str = bad.to_string_lossy().replace('\\', "/");

    let src = format!(
        r#"
import "std::db" as db

fn main() {{
    let conn = db.connect("{bad}")
    if typeof(conn) == "map" {{
        let c = conn["code"]
        echo "ERR_HANDLED code=$c"
    }} else {{
        echo "UNEXPECTED_OK handle=$conn"
    }}
    // Reaching this line proves the process kept running (no crash).
    echo "NO_CRASH"
}}
"#,
        bad = bad_str
    );

    let (out, fixture) = run_program(&format!("db_bad_path_{}.ran", id), &src);
    let so = stdout(&out);
    let _ = std::fs::remove_file(&fixture);

    // The interpreter must NOT crash: the program runs to completion, exit 0.
    assert_eq!(code(&out), 0, "connect failure must not crash; stderr: {}", stderr(&out));
    assert!(so.contains("NO_CRASH"), "process did not run to completion; stdout: {}", so);
    assert!(
        so.contains("ERR_HANDLED"),
        "bad-path connect should yield a handleable error value; stdout: {}",
        so
    );
    // The handleable error carries a DB diagnostic code (E05xx family).
    assert!(
        so.contains("code=E05"),
        "expected an E05xx DB error code in the handleable value; stdout: {}",
        so
    );
}
