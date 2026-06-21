//! Module `db` — SQLite connectivity exposed to the Ran language (Kelompok B).
//!
//! This is the thin stdlib layer that sits between the Ran runtime dispatch
//! (`runtime::call_module_method`, arm `("db", ...)`) and the FFI/lifecycle
//! layer in [`crate::support::sqlite_ffi`]. It owns a process-global registry
//! of open connections keyed by an opaque `u64` handle, following the exact
//! pattern used by the concurrency registries in `src/stdlib/concurrency.rs`
//! (`OnceLock<Mutex<HashMap<...>>>` + an `AtomicU64` id source).
//!
//! Ran programs never see a raw pointer: `db.connect(path)` returns an
//! `Int(handle)` that indexes this registry, and every later call
//! (`query`/`exec`/`begin`/`commit`/`rollback`/`close`) looks the connection up
//! by handle. An unknown/closed handle yields [`DbError::invalid_handle`]
//! (`E0503`).
//!
//! All operations return `Result<_, DbError>`; the runtime turns a `DbError`
//! into a handleable Ran error value (`Map{error, code, message}`) plus a
//! diagnostic, mirroring the concurrency `*_send_closed`/`*_invalid` helpers,
//! so a database error never crashes the interpreter.
//!
//! Value mapping (Ran `Value` ↔ [`DbValue`]) lives in the runtime dispatch
//! layer, keeping this module independent of the runtime `Value` type.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::support::sqlite_ffi::{DbError, DbRow, DbValue, SqliteConn};

/// Process-global registry of open SQLite connections, keyed by opaque handle.
///
/// `SqliteConn` is `Send` (see `sqlite_ffi`), so it can live behind this mutex
/// and be reached from any thread. Holding the registry lock for the duration
/// of an operation also serializes concurrent access to the connection set,
/// which is sufficient for the single-process model here.
fn registry() -> &'static Mutex<HashMap<u64, SqliteConn>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, SqliteConn>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Monotonic source of connection handle ids (never reused within a process).
fn next_handle() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Open (creating if missing) a SQLite database at `path` and register it.
///
/// Returns the opaque handle id on success. On failure the original
/// [`DbError`] is returned unchanged (e.g. `E0501` when the file cannot be
/// accessed or `libsqlite3` is unavailable, `E0502` when the file is not a
/// valid SQLite database) so the caller can surface a handleable Ran error
/// without crashing (R6.1, R6.2, R6.3, R6.4).
pub fn connect(path: &str) -> Result<u64, DbError> {
    let conn = SqliteConn::open(path)?;
    let id = next_handle();
    crate::stdlib::lock_or_fault(registry(), "db connection registry").insert(id, conn);
    Ok(id)
}

/// Close the connection behind `handle`, remove it from the registry, and mark
/// it invalid (R6.5). An unknown/already-closed handle yields `E0503` (R6.6).
pub fn close(handle: u64) -> Result<(), DbError> {
    let mut reg = crate::stdlib::lock_or_fault(registry(), "db connection registry");
    match reg.get_mut(&handle) {
        Some(conn) => {
            let result = conn.close();
            // Drop the entry regardless: the connection is finalized and the
            // handle must never be reused (R6.5).
            reg.remove(&handle);
            result
        }
        None => Err(DbError::invalid_handle()),
    }
}

/// Run a parameterized `SELECT` on `handle`, returning the result rows (R7.1).
/// Zero matching rows yield an empty `Vec` (R7.2). Unknown handle → `E0503`.
pub fn query(handle: u64, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>, DbError> {
    let reg = crate::stdlib::lock_or_fault(registry(), "db connection registry");
    match reg.get(&handle) {
        Some(conn) => conn.query(sql, params),
        None => Err(DbError::invalid_handle()),
    }
}

/// Run a parameterized write/DDL command on `handle`, returning the number of
/// affected rows (≥0, R7.3). Unknown handle → `E0503`.
pub fn exec(handle: u64, sql: &str, params: &[DbValue]) -> Result<i64, DbError> {
    let reg = crate::stdlib::lock_or_fault(registry(), "db connection registry");
    match reg.get(&handle) {
        Some(conn) => conn.exec(sql, params),
        None => Err(DbError::invalid_handle()),
    }
}

/// Begin a transaction on `handle` (R9.1). Already-active → `E0509` (R9.2);
/// unknown handle → `E0503`.
pub fn begin(handle: u64) -> Result<(), DbError> {
    let mut reg = crate::stdlib::lock_or_fault(registry(), "db connection registry");
    match reg.get_mut(&handle) {
        Some(conn) => conn.begin(),
        None => Err(DbError::invalid_handle()),
    }
}

/// Commit the active transaction on `handle` (R9.3). No active transaction →
/// `E0510` (R9.5); unknown handle → `E0503`.
pub fn commit(handle: u64) -> Result<(), DbError> {
    let mut reg = crate::stdlib::lock_or_fault(registry(), "db connection registry");
    match reg.get_mut(&handle) {
        Some(conn) => conn.commit(),
        None => Err(DbError::invalid_handle()),
    }
}

/// Roll back the active transaction on `handle` (R9.4). No active transaction →
/// `E0510` (R9.5); unknown handle → `E0503`.
pub fn rollback(handle: u64) -> Result<(), DbError> {
    let mut reg = crate::stdlib::lock_or_fault(registry(), "db connection registry");
    match reg.get_mut(&handle) {
        Some(conn) => conn.rollback(),
        None => Err(DbError::invalid_handle()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests exercise the registry bookkeeping that does not require the
    // system `libsqlite3`. Full end-to-end coverage with a real database lives
    // in the availability-skipped integration tests (task 3.12).

    #[test]
    fn close_unknown_handle_is_invalid() {
        // A handle that was never registered must report E0503, not panic.
        let err = close(u64::MAX).unwrap_err();
        assert_eq!(err.code, "E0503");
    }

    #[test]
    fn query_unknown_handle_is_invalid() {
        let err = query(u64::MAX, "SELECT 1", &[]).unwrap_err();
        assert_eq!(err.code, "E0503");
    }

    #[test]
    fn exec_unknown_handle_is_invalid() {
        let err = exec(u64::MAX, "CREATE TABLE t(x)", &[]).unwrap_err();
        assert_eq!(err.code, "E0503");
    }

    #[test]
    fn tx_ops_on_unknown_handle_are_invalid() {
        assert_eq!(begin(u64::MAX).unwrap_err().code, "E0503");
        assert_eq!(commit(u64::MAX).unwrap_err().code, "E0503");
        assert_eq!(rollback(u64::MAX).unwrap_err().code, "E0503");
    }
}
