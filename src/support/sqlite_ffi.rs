//! Native SQLite backend over the system `libsqlite3`, via FFI.
//!
//! This is the FFI scaffolding + connection lifecycle layer for Kelompok B
//! (module `db`). It mirrors the pattern established in `src/support/tls.rs`
//! and `src/support/js_bridge.rs`:
//!   * opaque `*mut c_void` handles model the SQLite connection/statement,
//!   * RAII (`Drop`) guarantees deterministic release (and rolls back any
//!     in-flight transaction),
//!   * `unsafe impl Send` documents the controlled handle-registry usage, and
//!   * the system library is reached through a C-ABI surface (no Cargo crates).
//!
//! Like other optional system libraries, `libsqlite3` may be absent on a given
//! host, and the
//! project's `build.rs` only links it conditionally (auto-detected or forced
//! via `RAN_ENABLE_SQLITE`). To keep a stock `cargo build` green everywhere
//! *and* to implement the design's "availability checked at runtime on
//! `db.connect`" contract, this module resolves the SQLite entry points
//! dynamically at runtime through `dlopen`/`dlsym` (libdl, always present)
//! rather than referencing them as link-time symbols. The function-pointer
//! type aliases below are the C-ABI declarations for those entry points.
//!
//! Scope of this module (task 3.1): the `extern "C"` surface + opaque types,
//! the `SqliteConn` struct, `open` (with `PRAGMA journal_mode=WAL;`), an
//! idempotent `close`, and a deterministic `Drop` that rolls back when a
//! transaction is still active. Parameterized `query`/`exec` (3.2), the decimal
//! mapping (3.3), and the transaction bodies (3.4) are filled in by later
//! tasks; clean signatures/stubs are provided here for them to build on.
//!
//! Open/close diagnostics: permission failure → `E0501`, a file that is not a
//! valid SQLite database → `E0502`, and operations on an invalid/closed handle
//! → `E0503` (codes registered in `support::diagnostics`).

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

use crate::support::decimal::Decimal;
use crate::support::diagnostics::Diagnostic;

// --- Opaque SQLite handle types ---------------------------------------------
// `sqlite3` (connection) and `sqlite3_stmt` (prepared statement) are only ever
// exposed behind pointers; we never peek inside them, exactly like the
// OpenSSL `SSL`/`SSL_CTX` handles in `tls.rs`.
type sqlite3 = c_void;
type sqlite3_stmt = c_void;

// --- SQLite result codes (subset we classify) -------------------------------
const SQLITE_OK: c_int = 0;
const SQLITE_PERM: c_int = 3;
const SQLITE_BUSY: c_int = 5;
const SQLITE_READONLY: c_int = 8;
const SQLITE_CORRUPT: c_int = 11;
/// Constraint violation (e.g. UNIQUE/NOT NULL/CHECK/FK). SQLite also defines
/// *extended* result codes that share this value in their low byte
/// (`SQLITE_CONSTRAINT_UNIQUE = 19 | (8<<8)`, etc.), so callers classify a
/// constraint failure with `rc & 0xff == SQLITE_CONSTRAINT`. A single failing
/// statement is rolled back by SQLite at the statement level, so DB state is
/// left exactly as it was before the command (R8.6).
const SQLITE_CONSTRAINT: c_int = 19;
const SQLITE_CANTOPEN: c_int = 14;
const SQLITE_AUTH: c_int = 23;
const SQLITE_NOTADB: c_int = 26;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;

// --- Fundamental column datatypes (`sqlite3_column_type`) -------------------
// These classify the *value* stored in a column for the current row, not the
// declared column affinity. Used by the read mapping in `query` (task 3.2).
const SQLITE_INTEGER: c_int = 1;
const SQLITE_FLOAT: c_int = 2;
const SQLITE_TEXT: c_int = 3;
const SQLITE_BLOB: c_int = 4;
const SQLITE_NULL: c_int = 5;

// --- `sqlite3_open_v2` flags ------------------------------------------------
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;

/// Special destructor value passed to `sqlite3_bind_text`: instructs SQLite to
/// make its own private copy of the bound bytes. Used by the binding path in
/// task 3.2; declared here alongside the FFI surface.
const SQLITE_TRANSIENT: isize = -1;

// --- C-ABI declarations of the libsqlite3 entry points ----------------------
// Declared as `extern "C"` function-pointer type aliases (resolved at runtime
// via `dlsym`) so the default build links cleanly when `libsqlite3` is absent.
type FnOpenV2 =
    unsafe extern "C" fn(*const c_char, *mut *mut sqlite3, c_int, *const c_char) -> c_int;
type FnCloseV2 = unsafe extern "C" fn(*mut sqlite3) -> c_int;
type FnPrepareV2 = unsafe extern "C" fn(
    *mut sqlite3,
    *const c_char,
    c_int,
    *mut *mut sqlite3_stmt,
    *mut *const c_char,
) -> c_int;
type FnBindInt64 = unsafe extern "C" fn(*mut sqlite3_stmt, c_int, i64) -> c_int;
type FnBindDouble = unsafe extern "C" fn(*mut sqlite3_stmt, c_int, f64) -> c_int;
type FnBindText =
    unsafe extern "C" fn(*mut sqlite3_stmt, c_int, *const c_char, c_int, *const c_void) -> c_int;
type FnBindNull = unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int;
type FnStep = unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int;
type FnColumnCount = unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int;
type FnColumnType = unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int;
type FnColumnInt64 = unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> i64;
type FnColumnDouble = unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> f64;
type FnColumnText = unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const u8;
type FnColumnName = unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_char;
type FnChanges = unsafe extern "C" fn(*mut sqlite3) -> c_int;
type FnFinalize = unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int;
type FnErrmsg = unsafe extern "C" fn(*mut sqlite3) -> *const c_char;
type FnBindParameterCount = unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int;

// --- libdl (dynamic loader): always available via libc ----------------------
const RTLD_NOW: c_int = 2;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
}

/// The `libsqlite3` shared-object names probed on first use, most specific
/// first. Covers the common Linux and macOS packaging.
const SQLITE_LIB_NAMES: &[&str] = &[
    "libsqlite3.so",
    "libsqlite3.so.0",
    "libsqlite3.dylib",
    "libsqlite3.0.dylib",
];

/// Resolved SQLite entry points plus the owning `dlopen` handle.
///
/// One set of symbols is resolved per connection. `dlopen` is reference counted
/// by the loader, so repeated opens on the same process are cheap and return
/// the same underlying library.
struct Symbols {
    handle: *mut c_void,
    open_v2: FnOpenV2,
    close_v2: FnCloseV2,
    prepare_v2: FnPrepareV2,
    bind_int64: FnBindInt64,
    bind_double: FnBindDouble,
    bind_text: FnBindText,
    bind_null: FnBindNull,
    step: FnStep,
    column_count: FnColumnCount,
    column_type: FnColumnType,
    column_int64: FnColumnInt64,
    column_double: FnColumnDouble,
    column_text: FnColumnText,
    column_name: FnColumnName,
    changes: FnChanges,
    finalize: FnFinalize,
    errmsg: FnErrmsg,
    bind_parameter_count: FnBindParameterCount,
}

impl Symbols {
    /// Locate `libsqlite3` on the host and resolve every entry point we need.
    /// Returns `E0501` if the library is unavailable or a symbol is missing —
    /// this is the runtime availability check promised by the design.
    fn load() -> Result<Symbols, DbError> {
        let handle = open_sqlite().ok_or_else(DbError::backend_unavailable)?;

        // SAFETY: `handle` is a valid library handle from `dlopen`. Each symbol
        // is checked for null before being transmuted to its C-ABI signature.
        unsafe {
            let sym = |name: &str| -> Result<*mut c_void, DbError> {
                let cname = CString::new(name).map_err(|_| DbError::backend_unavailable())?;
                let ptr = dlsym(handle, cname.as_ptr());
                if ptr.is_null() {
                    Err(DbError::new(
                        "E0501",
                        format!("simbol SQLite `{name}` tidak ditemukan di libsqlite3"),
                    ))
                } else {
                    Ok(ptr)
                }
            };

            let symbols = Symbols {
                handle,
                open_v2: std::mem::transmute::<*mut c_void, FnOpenV2>(sym("sqlite3_open_v2")?),
                close_v2: std::mem::transmute::<*mut c_void, FnCloseV2>(sym("sqlite3_close_v2")?),
                prepare_v2: std::mem::transmute::<*mut c_void, FnPrepareV2>(sym(
                    "sqlite3_prepare_v2",
                )?),
                bind_int64: std::mem::transmute::<*mut c_void, FnBindInt64>(sym(
                    "sqlite3_bind_int64",
                )?),
                bind_double: std::mem::transmute::<*mut c_void, FnBindDouble>(sym(
                    "sqlite3_bind_double",
                )?),
                bind_text: std::mem::transmute::<*mut c_void, FnBindText>(sym("sqlite3_bind_text")?),
                bind_null: std::mem::transmute::<*mut c_void, FnBindNull>(sym("sqlite3_bind_null")?),
                step: std::mem::transmute::<*mut c_void, FnStep>(sym("sqlite3_step")?),
                column_count: std::mem::transmute::<*mut c_void, FnColumnCount>(sym(
                    "sqlite3_column_count",
                )?),
                column_type: std::mem::transmute::<*mut c_void, FnColumnType>(sym(
                    "sqlite3_column_type",
                )?),
                column_int64: std::mem::transmute::<*mut c_void, FnColumnInt64>(sym(
                    "sqlite3_column_int64",
                )?),
                column_double: std::mem::transmute::<*mut c_void, FnColumnDouble>(sym(
                    "sqlite3_column_double",
                )?),
                column_text: std::mem::transmute::<*mut c_void, FnColumnText>(sym(
                    "sqlite3_column_text",
                )?),
                column_name: std::mem::transmute::<*mut c_void, FnColumnName>(sym(
                    "sqlite3_column_name",
                )?),
                changes: std::mem::transmute::<*mut c_void, FnChanges>(sym("sqlite3_changes")?),
                finalize: std::mem::transmute::<*mut c_void, FnFinalize>(sym("sqlite3_finalize")?),
                errmsg: std::mem::transmute::<*mut c_void, FnErrmsg>(sym("sqlite3_errmsg")?),
                bind_parameter_count: std::mem::transmute::<*mut c_void, FnBindParameterCount>(sym(
                    "sqlite3_bind_parameter_count",
                )?),
            };
            Ok(symbols)
        }
    }
}

/// Try each candidate library name; return the first handle that opens.
fn open_sqlite() -> Option<*mut c_void> {
    for name in SQLITE_LIB_NAMES {
        if let Ok(cname) = CString::new(*name) {
            // SAFETY: `cname` is a valid NUL-terminated string for the duration
            // of the call; `dlopen` returns null on failure (handled below).
            let handle = unsafe { dlopen(cname.as_ptr(), RTLD_NOW) };
            if !handle.is_null() {
                return Some(handle);
            }
        }
    }
    None
}

// --- Error type carried across the backend ----------------------------------

/// An error raised by the SQLite backend.
///
/// Carries a registered diagnostic code (`E05xx`, see
/// `support::diagnostics::DIAGNOSTIC_CATALOG`) plus a human-readable message.
/// The `db` stdlib layer turns this into either a Ran-level error value or a
/// `Diagnostic` at the call site via [`DbError::to_diagnostic`] (mirrors
/// `JsError` in `js_bridge`).
#[derive(Debug, Clone)]
pub struct DbError {
    /// Diagnostic code, e.g. `"E0501"` / `"E0502"` / `"E0503"`.
    pub code: &'static str,
    /// Detail message; surfaced in the diagnostic and/or Ran error value.
    pub message: String,
}

impl DbError {
    /// Build an error with an explicit registered code and message.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// `E0501` — `libsqlite3` is unavailable on this host. Treated as an access
    /// failure per the design's runtime-availability contract on `db.connect`.
    pub fn backend_unavailable() -> Self {
        Self::new(
            "E0501",
            "Backend SQLite (libsqlite3) tidak tersedia pada host ini",
        )
    }

    /// `E0501` — the database path could not be read/written (permissions or
    /// the file could not be opened).
    pub fn permission(detail: impl Into<String>) -> Self {
        Self::new("E0501", detail.into())
    }

    /// `E0502` — the file exists but is not a valid SQLite database.
    pub fn not_a_database(detail: impl Into<String>) -> Self {
        Self::new("E0502", detail.into())
    }

    /// `E0503` — an operation was attempted on a closed/invalid connection.
    pub fn invalid_handle() -> Self {
        Self::new(
            "E0503",
            "Handle koneksi sudah ditutup atau tidak valid",
        )
    }

    /// `E0509` — `begin` was called while a transaction is already active.
    /// Non-handleable: the caller must `commit`/`rollback` first (R8.2).
    pub fn tx_already_active() -> Self {
        Self::new(
            "E0509",
            "Transaksi sudah aktif; commit/rollback dulu sebelum begin lagi",
        )
    }

    /// `E0510` — `commit`/`rollback` was called with no active transaction.
    /// Non-handleable: the caller must `begin` first (R8.5).
    pub fn no_active_tx() -> Self {
        Self::new(
            "E0510",
            "Tidak ada transaksi aktif; panggil begin sebelum commit/rollback",
        )
    }

    /// Render this error as a project [`Diagnostic`], adopting the catalog
    /// severity and default fix hint for its code.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::from_code(self.code, self.message.clone())
    }
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for DbError {}

/// Classify a SQLite result code returned while *opening* a database into the
/// open/close diagnostic codes that are in scope for this layer.
fn classify_open_error(rc: c_int, sqlite_msg: &str, path: &str) -> DbError {
    let suffix = if sqlite_msg.is_empty() {
        String::new()
    } else {
        format!(": {sqlite_msg}")
    };
    match rc {
        SQLITE_NOTADB | SQLITE_CORRUPT => {
            DbError::not_a_database(format!("`{path}` bukan database SQLite yang valid{suffix}"))
        }
        SQLITE_CANTOPEN | SQLITE_PERM | SQLITE_READONLY | SQLITE_AUTH | SQLITE_BUSY => {
            DbError::permission(format!("tidak dapat mengakses database `{path}`{suffix}"))
        }
        _ => DbError::permission(format!(
            "gagal membuka database `{path}` (rc={rc}){suffix}"
        )),
    }
}

/// Classify a SQLite result code returned while executing a simple statement
/// (e.g. the WAL pragma or a rollback). Maps corruption to `E0502`, access
/// failures to `E0501`, a constraint violation to `E0505` (handleable, R8.6),
/// and anything else to a generic SQL error (`E0504`).
fn classify_stmt_error(rc: c_int, sqlite_msg: &str) -> DbError {
    let suffix = if sqlite_msg.is_empty() {
        String::new()
    } else {
        format!(": {sqlite_msg}")
    };
    // Constraint violations carry SQLITE_CONSTRAINT (19) in the low byte, both
    // for the primary code and every extended variant
    // (UNIQUE/NOTNULL/CHECK/FOREIGNKEY/…). Surface them as the handleable
    // `E0505`: the failing statement is rolled back by SQLite on its own, so
    // the command leaves DB state exactly as before (statement atomicity, R8.6).
    if rc & 0xff == SQLITE_CONSTRAINT {
        return DbError::new(
            "E0505",
            format!("perintah melanggar constraint{suffix}"),
        );
    }
    match rc {
        SQLITE_NOTADB | SQLITE_CORRUPT => {
            DbError::not_a_database(format!("bukan database SQLite yang valid{suffix}"))
        }
        SQLITE_CANTOPEN | SQLITE_PERM | SQLITE_READONLY | SQLITE_AUTH => {
            DbError::permission(format!("akses database ditolak{suffix}"))
        }
        _ => DbError::new("E0504", format!("kesalahan SQLite (rc={rc}){suffix}")),
    }
}

// --- Backend value type -----------------------------------------------------

/// A value read from / bound to a SQLite column.
///
/// Minimal scaffolding for the lifecycle layer; the parameterized query/exec
/// path (task 3.2) and the decimal mapping (task 3.3) operate on this type.
/// `Null` corresponds to SQLite `NULL` (Ran `void`).
///
/// `Decimal` is the **opt-in monetary** value (R7.5/R7.7): there is no SQLite
/// storage class for it, so money is persisted as exact-decimal `TEXT`
/// (`Decimal::to_string`, no rounding) on the bind side and reconstructed from
/// the chosen TEXT columns on the read side via [`SqliteConn::query_money`].
/// Which columns are monetary is decided by the *caller* (naming convention /
/// `opts`), never inferred from the SQLite column type.
#[derive(Debug, Clone)]
pub enum DbValue {
    Null,
    Int(i64),
    Float(f64),
    Str(String),
    Decimal(Decimal),
}

impl PartialEq for DbValue {
    /// Structural equality. `Decimal` values compare by their exact decimal
    /// text (`Decimal::to_string`), so both the digits *and* the scale must
    /// match — i.e. `"1.50"` and `"1.5"` are distinct. This mirrors the
    /// "identical decimally, without losing digits" guarantee of R7.5 and keeps
    /// the type usable in `assert_eq!`-style tests despite `Decimal` not
    /// deriving `PartialEq`.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (DbValue::Null, DbValue::Null) => true,
            (DbValue::Int(a), DbValue::Int(b)) => a == b,
            (DbValue::Float(a), DbValue::Float(b)) => a == b,
            (DbValue::Str(a), DbValue::Str(b)) => a == b,
            (DbValue::Decimal(a), DbValue::Decimal(b)) => a.to_string() == b.to_string(),
            _ => false,
        }
    }
}

/// A single result row: column name → value, in column order.
pub type DbRow = Vec<(String, DbValue)>;

// --- Connection + lifecycle -------------------------------------------------

/// A single open SQLite connection.
///
/// Stored in the runtime handle registry (`HashMap<u64, SqliteConn>`) and
/// referenced from Ran programs by an opaque `Int(HandleId)`. `valid` tracks
/// whether the handle may still be used (R5.5/R5.6); `in_tx` tracks whether a
/// transaction is currently active (R8) so `Drop`/`close` can roll it back.
pub struct SqliteConn {
    db: *mut sqlite3,
    /// Whether a transaction is currently active on this connection (R8.1/8.2).
    ///
    /// A [`Cell`] (interior mutability) so the read/write entry points
    /// `query`/`exec` — which take `&self` per the design surface — can clear
    /// the flag when a constraint failure triggers an automatic `ROLLBACK`
    /// inside an active transaction (R6.7/R8.6). The transaction *control*
    /// methods (`begin`/`commit`/`rollback`) still take `&mut self`.
    in_tx: Cell<bool>,
    /// Whether this handle is still valid; set to `false` by `close`/`Drop`
    /// so later use surfaces `E0503` (R5.5/R5.6).
    valid: bool,
    /// Resolved SQLite entry points; used by every operation and by `Drop`.
    sym: Symbols,
}

impl SqliteConn {
    /// Open (creating if missing) a SQLite database at `path` and enable WAL.
    ///
    /// * Missing file → created (`SQLITE_OPEN_CREATE`, R5.2).
    /// * Permission/access failure → `E0501` (R5.3), no handle returned.
    /// * Existing file that is not a valid SQLite database → `E0502` (R5.4),
    ///   surfaced when `PRAGMA journal_mode=WAL;` first touches the file.
    ///
    /// Returns a valid, ready-to-use connection on success (R5.1).
    pub fn open(path: &str) -> Result<SqliteConn, DbError> {
        let sym = Symbols::load()?; // E0501 when libsqlite3 is unavailable.

        let cpath = CString::new(path).map_err(|_| {
            DbError::permission(format!("path database mengandung byte NUL: {path:?}"))
        })?;
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE;

        // SAFETY: `sym` holds valid, non-null SQLite entry points (verified in
        // `Symbols::load`). `cpath` is a valid NUL-terminated string for the
        // duration of the call; `db` is checked and released deterministically.
        unsafe {
            let mut db: *mut sqlite3 = std::ptr::null_mut();
            let rc = (sym.open_v2)(cpath.as_ptr(), &mut db, flags, std::ptr::null());
            if rc != SQLITE_OK {
                // Capture the message (if any) before releasing the handle.
                let msg = errmsg_of(&sym, db);
                if !db.is_null() {
                    (sym.close_v2)(db);
                }
                return Err(classify_open_error(rc, &msg, path));
            }

            let conn = SqliteConn {
                db,
                in_tx: Cell::new(false),
                valid: true,
                sym,
            };

            // Enable WAL for better read concurrency (open decision #4). This
            // also forces SQLite to read the file header, surfacing `E0502`
            // (SQLITE_NOTADB) for a file that is not a valid database.
            conn.exec_simple("PRAGMA journal_mode=WAL;")?;
            Ok(conn)
        }
    }

    /// Close the connection, release the file handle, and mark the handle
    /// invalid (R5.5). Idempotent at the FFI level: the underlying handle is
    /// freed exactly once. Calling `close` on an already-closed/invalid handle
    /// returns `E0503` (R5.6).
    pub fn close(&mut self) -> Result<(), DbError> {
        if !self.valid {
            return Err(DbError::invalid_handle());
        }
        // Roll back any in-flight transaction so closing never silently commits
        // partial work (consistent with `Drop`).
        if self.in_tx.get() {
            let _ = self.rollback_internal();
        }
        // SAFETY: `db` was produced by `sqlite3_open_v2` and is freed exactly
        // once here; the pointer is nulled to prevent a double free in `Drop`.
        unsafe {
            if !self.db.is_null() {
                (self.sym.close_v2)(self.db);
                self.db = std::ptr::null_mut();
            }
        }
        self.valid = false;
        Ok(())
    }

    /// Whether this connection handle is still valid (not yet closed).
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Whether a transaction is currently active on this connection.
    pub fn in_transaction(&self) -> bool {
        self.in_tx.get()
    }

    /// Run a parameterized `SELECT` and return the rows.
    ///
    /// Parameters are **always bound** through `sqlite3_bind_*`, never spliced
    /// into the SQL text (R6.4). The number of supplied parameters must equal
    /// `sqlite3_bind_parameter_count`; a mismatch returns `E0506` *without*
    /// executing anything (R6.6). Invalid SQL returns `E0504` (R6.5).
    ///
    /// Each result row is read column-by-column using the runtime value type
    /// (`sqlite3_column_type`) and mapped per the design's type table (R7):
    /// INTEGER→`Int`, REAL→`Float` (bit-pattern preserved), TEXT→`Str`
    /// (including the empty string), NULL→`Null`. A BLOB column — or any other
    /// unsupported type — aborts the conversion with `E0507` (R7.6).
    ///
    /// Zero matching rows yield an empty `Vec` and no error (R6.2). The
    /// prepared statement is finalized on every return path.
    pub fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>, DbError> {
        if !self.valid {
            return Err(DbError::invalid_handle());
        }

        // SAFETY: `self.db` is a valid open connection; `prepare_and_bind`
        // returns a non-null, parameter-bound statement and finalizes it itself
        // on any error path. We finalize on every path out of this block.
        unsafe {
            let stmt = self.prepare_and_bind(sql, params)?;
            let ncols = (self.sym.column_count)(stmt);

            let mut rows: Vec<DbRow> = Vec::new();
            loop {
                let step_rc = (self.sym.step)(stmt);
                if step_rc == SQLITE_ROW {
                    let mut row: DbRow = Vec::with_capacity(ncols.max(0) as usize);
                    for i in 0..ncols {
                        let name = column_name_of(&self.sym, stmt, i);
                        let ctype = (self.sym.column_type)(stmt, i);
                        let value = match ctype {
                            SQLITE_INTEGER => DbValue::Int((self.sym.column_int64)(stmt, i)),
                            // `column_double` returns the IEEE-754 f64 verbatim,
                            // so the bit pattern is preserved (R7.2).
                            SQLITE_FLOAT => DbValue::Float((self.sym.column_double)(stmt, i)),
                            SQLITE_TEXT => DbValue::Str(column_text_of(&self.sym, stmt, i)),
                            SQLITE_NULL => DbValue::Null,
                            other => {
                                // BLOB (4) or anything outside the supported
                                // base types → abort and finalize (E0507).
                                (self.sym.finalize)(stmt);
                                return Err(DbError::new(
                                    "E0507",
                                    format!(
                                        "tipe kolom `{}` (kolom `{}`) tidak didukung",
                                        sqlite_type_name(other),
                                        name
                                    ),
                                ));
                            }
                        };
                        row.push((name, value));
                    }
                    rows.push(row);
                } else if step_rc == SQLITE_DONE {
                    break;
                } else {
                    let msg = errmsg_of(&self.sym, self.db);
                    (self.sym.finalize)(stmt);
                    return Err(classify_stmt_error(step_rc, &msg));
                }
            }

            (self.sym.finalize)(stmt);
            Ok(rows)
        }
    }

    /// Like [`query`](Self::query), but additionally reinterprets the named
    /// `money_columns` as exact-decimal [`Decimal`] values (R7.5/R7.7).
    ///
    /// Monetary columns are **caller-selected** (by convention/`opts`), never
    /// inferred from the SQLite storage class: this method first runs the
    /// ordinary read mapping, then post-processes only the columns whose name
    /// appears in `money_columns`:
    ///   * `TEXT` → parsed to `Decimal` via [`parse_money`] (exact, no rounding);
    ///   * `NULL` → left as `Null` (a missing amount stays `void`);
    ///   * any other storage class (INTEGER/REAL/…) → `E0508`, because money
    ///     must be persisted as an exact-decimal string.
    ///
    /// A malformed or out-of-range decimal string aborts the whole call with
    /// `E0508` **without producing a `Decimal`** (R7.7); the base-type mapping
    /// of task 3.2 is unchanged for every non-money column.
    pub fn query_money(
        &self,
        sql: &str,
        params: &[DbValue],
        money_columns: &[&str],
    ) -> Result<Vec<DbRow>, DbError> {
        let mut rows = self.query(sql, params)?;
        for row in rows.iter_mut() {
            for (name, value) in row.iter_mut() {
                if money_columns.iter().any(|c| *c == name.as_str()) {
                    *value = money_value_from(name, value)?;
                }
            }
        }
        Ok(rows)
    }

    /// Run a parameterized write/DDL command (`INSERT`/`UPDATE`/`DELETE`/DDL)
    /// and return the number of affected rows (`sqlite3_changes`, always ≥0).
    ///
    /// Shares the parameter-binding contract with [`query`](Self::query):
    /// parameters are always bound (R6.4), invalid SQL → `E0504` (R6.5), and a
    /// parameter-count mismatch → `E0506` without executing (R6.6). The
    /// statement is stepped to completion and finalized on every path.
    ///
    /// ## Atomicity / auto-rollback on constraint failure (R6.7/R8.6)
    ///
    /// When a step fails with a constraint violation (`SQLITE_CONSTRAINT`,
    /// result code 19, including every extended variant whose low byte is 19)
    /// the error is surfaced as the handleable `E0505`.
    ///
    /// SQLite already rolls a *single* failed statement back at the statement
    /// level, so outside a transaction the DB is left exactly as before the
    /// command. **Inside an active transaction** (`in_tx == true`) the design
    /// requires the whole in-flight transaction to return to the state that
    /// existed before it began (the failed command is not silently retried and
    /// no partial work is left pending for a later `commit`). This method
    /// therefore issues an explicit `ROLLBACK;` and clears `in_tx` before
    /// returning `E0505`: the connection is left with **no active transaction**
    /// and the database back at its pre-transaction state. The caller, on
    /// receiving the handleable `E0505`, may `begin` a fresh transaction and
    /// retry. (`exec` takes `&self`; the flag lives in a `Cell` so the
    /// auto-rollback can clear it here.)
    pub fn exec(&self, sql: &str, params: &[DbValue]) -> Result<i64, DbError> {
        if !self.valid {
            return Err(DbError::invalid_handle());
        }

        // SAFETY: as in `query` — `prepare_and_bind` yields a non-null bound
        // statement (self-finalizing on error) and we finalize on every path.
        unsafe {
            let stmt = self.prepare_and_bind(sql, params)?;
            loop {
                let step_rc = (self.sym.step)(stmt);
                if step_rc == SQLITE_ROW {
                    // A write/DDL statement should not produce rows; ignore any
                    // that appear (e.g. `INSERT ... RETURNING`) and keep going.
                    continue;
                }
                if step_rc == SQLITE_DONE {
                    break;
                }
                let msg = errmsg_of(&self.sym, self.db);
                (self.sym.finalize)(stmt);
                // Auto-rollback (R6.7/R8.6): a constraint violation while a
                // transaction is active rolls the whole transaction back so the
                // database returns to its pre-transaction state, then clears the
                // active-transaction flag. The error itself is still surfaced as
                // the handleable `E0505`.
                if step_rc & 0xff == SQLITE_CONSTRAINT && self.in_tx.get() {
                    let _ = self.exec_simple("ROLLBACK;");
                    self.in_tx.set(false);
                }
                return Err(classify_stmt_error(step_rc, &msg));
            }
            (self.sym.finalize)(stmt);
            Ok((self.sym.changes)(self.db) as i64)
        }
    }

    /// Begin a transaction (R8.1/8.2).
    ///
    /// If a transaction is already active, returns `E0509` and changes nothing
    /// (no new transaction is started, `in_tx` stays `true`). Otherwise issues
    /// `BEGIN;` and marks the connection as in-transaction. A closed/invalid
    /// handle returns `E0503`.
    pub fn begin(&mut self) -> Result<(), DbError> {
        if !self.valid {
            return Err(DbError::invalid_handle());
        }
        if self.in_tx.get() {
            // Already inside a transaction → reject without touching state.
            return Err(DbError::tx_already_active());
        }
        self.exec_simple("BEGIN;")?;
        self.in_tx.set(true);
        Ok(())
    }

    /// Commit the active transaction (R8.3/8.5).
    ///
    /// If no transaction is active, returns `E0510` and changes nothing.
    /// Otherwise issues `COMMIT;` and clears the in-transaction flag, making
    /// the changes durable. A closed/invalid handle returns `E0503`.
    pub fn commit(&mut self) -> Result<(), DbError> {
        if !self.valid {
            return Err(DbError::invalid_handle());
        }
        if !self.in_tx.get() {
            return Err(DbError::no_active_tx());
        }
        self.exec_simple("COMMIT;")?;
        self.in_tx.set(false);
        Ok(())
    }

    /// Roll back the active transaction (R8.4/8.5).
    ///
    /// If no transaction is active, returns `E0510` and changes nothing.
    /// Otherwise issues `ROLLBACK;` and clears the in-transaction flag; the
    /// database state returns exactly to what it was before `begin`. A
    /// closed/invalid handle returns `E0503`.
    pub fn rollback(&mut self) -> Result<(), DbError> {
        if !self.valid {
            return Err(DbError::invalid_handle());
        }
        if !self.in_tx.get() {
            return Err(DbError::no_active_tx());
        }
        self.exec_simple("ROLLBACK;")?;
        self.in_tx.set(false);
        Ok(())
    }

    // --- private helpers ----------------------------------------------------

    /// Prepare `sql`, verify the parameter count, and bind every parameter.
    ///
    /// Returns a ready-to-step statement on success. On any failure the
    /// statement is finalized internally before returning, so callers never
    /// need to clean up on the error path:
    ///   * invalid SQL (or interior NUL) → `E0504` (R6.5),
    ///   * `params.len()` ≠ `sqlite3_bind_parameter_count` → `E0506`, with no
    ///     binding or execution performed (R6.6),
    ///   * a failing `bind_*` call → classified SQLite error.
    ///
    /// Parameters are bound positionally (1-based) via the typed `bind_*`
    /// entry points — never interpolated into the SQL text (R6.4). `TEXT` is
    /// bound by pointer+length with `SQLITE_TRANSIENT`, so SQLite copies the
    /// bytes immediately (the borrowed `&str` need not outlive the call) and
    /// empty/interior-NUL strings are handled without a `CString` round-trip.
    ///
    /// # Safety
    /// `self.db` must be a valid open connection. The returned pointer is
    /// non-null and owned by the caller, who must `finalize` it.
    unsafe fn prepare_and_bind(
        &self,
        sql: &str,
        params: &[DbValue],
    ) -> Result<*mut sqlite3_stmt, DbError> {
        let csql = CString::new(sql)
            .map_err(|_| DbError::new("E0504", "SQL mengandung byte NUL"))?;

        let mut stmt: *mut sqlite3_stmt = std::ptr::null_mut();
        let rc = (self.sym.prepare_v2)(
            self.db,
            csql.as_ptr(),
            -1,
            &mut stmt,
            std::ptr::null_mut(),
        );
        if rc != SQLITE_OK {
            let msg = errmsg_of(&self.sym, self.db);
            if !stmt.is_null() {
                (self.sym.finalize)(stmt);
            }
            // Invalid SQL surfaces as E0504 (classify_stmt_error maps the
            // generic SQLite error code there and carries the message).
            return Err(classify_stmt_error(rc, &msg));
        }

        // Reject a parameter-count mismatch before binding/executing (R6.6).
        let expected = (self.sym.bind_parameter_count)(stmt);
        if params.len() as c_int != expected {
            (self.sym.finalize)(stmt);
            return Err(DbError::new(
                "E0506",
                format!(
                    "Jumlah parameter ({}) tidak sama dengan placeholder ({}).",
                    params.len(),
                    expected
                ),
            ));
        }

        // Bind positionally (SQLite placeholders are 1-based).
        for (idx, param) in params.iter().enumerate() {
            let pos = (idx + 1) as c_int;
            let brc = match param {
                DbValue::Null => (self.sym.bind_null)(stmt, pos),
                DbValue::Int(v) => (self.sym.bind_int64)(stmt, pos, *v),
                DbValue::Float(v) => (self.sym.bind_double)(stmt, pos, *v),
                DbValue::Str(s) => (self.sym.bind_text)(
                    stmt,
                    pos,
                    s.as_ptr() as *const c_char,
                    s.len() as c_int,
                    SQLITE_TRANSIENT as *const c_void,
                ),
                // Money is stored as exact-decimal TEXT (R7.5): bind the
                // canonical `Decimal::to_string` with no rounding. SQLite copies
                // the bytes (`SQLITE_TRANSIENT`), so the temporary `s` may drop
                // immediately after this call.
                DbValue::Decimal(dec) => {
                    let s = dec.to_string();
                    (self.sym.bind_text)(
                        stmt,
                        pos,
                        s.as_ptr() as *const c_char,
                        s.len() as c_int,
                        SQLITE_TRANSIENT as *const c_void,
                    )
                }
            };
            if brc != SQLITE_OK {
                let msg = errmsg_of(&self.sym, self.db);
                (self.sym.finalize)(stmt);
                return Err(classify_stmt_error(brc, &msg));
            }
        }

        Ok(stmt)
    }

    /// Best-effort `ROLLBACK` used by `Drop`/`close` when a transaction is
    /// still active. Clears the `in_tx` flag regardless of the outcome.
    fn rollback_internal(&mut self) -> Result<(), DbError> {
        let res = self.exec_simple("ROLLBACK;");
        self.in_tx.set(false);
        res
    }

    /// Prepare/step/finalize a statement that returns no meaningful rows
    /// (pragmas, `ROLLBACK`, …). Rows produced (e.g. by `PRAGMA journal_mode`)
    /// are stepped through and discarded. Errors are classified via
    /// [`classify_stmt_error`].
    fn exec_simple(&self, sql: &str) -> Result<(), DbError> {
        let csql = CString::new(sql)
            .map_err(|_| DbError::new("E0504", "SQL mengandung byte NUL"))?;

        // SAFETY: `self.db` is a valid open connection; `csql` outlives the
        // `prepare_v2` call; `stmt` is finalized on every path.
        unsafe {
            let mut stmt: *mut sqlite3_stmt = std::ptr::null_mut();
            let rc = (self.sym.prepare_v2)(
                self.db,
                csql.as_ptr(),
                -1,
                &mut stmt,
                std::ptr::null_mut(),
            );
            if rc != SQLITE_OK {
                let msg = errmsg_of(&self.sym, self.db);
                if !stmt.is_null() {
                    (self.sym.finalize)(stmt);
                }
                return Err(classify_stmt_error(rc, &msg));
            }

            loop {
                let step_rc = (self.sym.step)(stmt);
                if step_rc == SQLITE_ROW {
                    continue; // discard any produced rows
                }
                if step_rc == SQLITE_DONE {
                    break;
                }
                let msg = errmsg_of(&self.sym, self.db);
                (self.sym.finalize)(stmt);
                return Err(classify_stmt_error(step_rc, &msg));
            }

            (self.sym.finalize)(stmt);
        }
        Ok(())
    }
}

/// Read the latest error message from a connection handle, if any.
fn errmsg_of(sym: &Symbols, db: *mut sqlite3) -> String {    if db.is_null() {
        return String::new();
    }
    // SAFETY: `db` is non-null and `errmsg` returns either null or a pointer to
    // a NUL-terminated, connection-owned C string valid until the next call.
    unsafe {
        let ptr = (sym.errmsg)(db);
        if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

/// Read a column's name as an owned `String`. Names are SQLite-owned,
/// NUL-terminated C strings valid for the lifetime of the statement.
fn column_name_of(sym: &Symbols, stmt: *mut sqlite3_stmt, i: c_int) -> String {
    // SAFETY: `stmt` is a live prepared statement and `i` is in
    // `0..column_count`; `column_name` returns null only on allocation failure.
    unsafe {
        let ptr = (sym.column_name)(stmt, i);
        if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

/// Read a `TEXT` column's value as an owned UTF-8 `String`.
///
/// SQLite returns NUL-terminated UTF-8 for `column_text`; an empty TEXT cell is
/// a valid non-null pointer to `""` (R7.3). A null pointer (e.g. on allocation
/// failure) is treated as the empty string.
fn column_text_of(sym: &Symbols, stmt: *mut sqlite3_stmt, i: c_int) -> String {
    // SAFETY: caller has verified the column type is TEXT for this row; the
    // returned pointer is valid until the next step/finalize on `stmt`.
    unsafe {
        let ptr = (sym.column_text)(stmt, i);
        if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr as *const c_char)
                .to_string_lossy()
                .into_owned()
        }
    }
}

/// Human-readable label for a `sqlite3_column_type` code (for diagnostics).
fn sqlite_type_name(t: c_int) -> &'static str {
    match t {
        SQLITE_INTEGER => "INTEGER",
        SQLITE_FLOAT => "REAL",
        SQLITE_TEXT => "TEXT",
        SQLITE_BLOB => "BLOB",
        SQLITE_NULL => "NULL",
        _ => "tidak dikenal",
    }
}

/// Parse an exact-decimal money string into a [`Decimal`] (R7.5/R7.7).
///
/// This is the single monetary read primitive: it never rounds and never
/// approximates. A malformed string or one whose mantissa exceeds the
/// representable range (`Decimal::parse` reports it as "too large") is reported
/// as `E0508` *without* producing a `Decimal`, satisfying the "abort the
/// conversion without yielding a decimal" contract of R7.7.
pub fn parse_money(text: &str) -> Result<Decimal, DbError> {
    Decimal::parse(text).map_err(|detail| {
        DbError::new(
            "E0508",
            format!("nilai moneter `{text}` tidak dapat dikonversi ke decimal: {detail}"),
        )
    })
}

/// Reinterpret one already-read column value as a monetary [`Decimal`].
///
/// Used by [`SqliteConn::query_money`] for caller-selected money columns:
///   * `Str` → [`parse_money`] (`E0508` on malformed/out-of-range input),
///   * `Null` → preserved as `Null` (a missing amount stays `void`),
///   * `Decimal` → returned unchanged (idempotent),
///   * `Int`/`Float`/other → `E0508`, since money must be stored as exact
///     decimal `TEXT` rather than a lossy numeric storage class.
fn money_value_from(column: &str, value: &DbValue) -> Result<DbValue, DbError> {
    match value {
        DbValue::Null => Ok(DbValue::Null),
        DbValue::Str(s) => Ok(DbValue::Decimal(parse_money(s)?)),
        DbValue::Decimal(d) => Ok(DbValue::Decimal(*d)),
        other => Err(DbError::new(
            "E0508",
            format!(
                "kolom moneter `{column}` harus berupa TEXT desimal eksak, \
                 bukan {}",
                db_value_kind(other)
            ),
        )),
    }
}

/// Short label for a [`DbValue`] variant, used in monetary diagnostics.
fn db_value_kind(v: &DbValue) -> &'static str {
    match v {
        DbValue::Null => "NULL",
        DbValue::Int(_) => "INTEGER",
        DbValue::Float(_) => "REAL",
        DbValue::Str(_) => "TEXT",
        DbValue::Decimal(_) => "decimal",
    }
}

impl Drop for SqliteConn {
    /// Deterministic release: roll back any in-flight transaction (R8.6) then
    /// close the connection via `sqlite3_close_v2` exactly once. Satisfies the
    /// "no leaked resources" contract for the `db` module.
    fn drop(&mut self) {
        if self.in_tx.get() {
            let _ = self.rollback_internal();
        }
        // SAFETY: `db` was produced by SQLite and is freed exactly once (this
        // connection owns it; null is guarded for defensiveness and to make a
        // prior explicit `close` a no-op here).
        unsafe {
            if !self.db.is_null() {
                (self.sym.close_v2)(self.db);
                self.db = std::ptr::null_mut();
            }
            // The dlopen handle is intentionally left open for the process
            // lifetime: the loader reference-counts it and a later connection
            // reuses the same library cheaply.
            let _ = dlclose; // keep the symbol referenced/documented
        }
        self.valid = false;
    }
}

// The connection wraps raw SQLite pointers used under controlled,
// single-owner access (the runtime handle registry). `Send` is declared so the
// handle can live in that registry and be moved between runtime structures,
// mirroring the `unsafe impl Send for JsEngine` pattern in `js_bridge.rs`.
unsafe impl Send for SqliteConn {}

/// Report whether `libsqlite3` is available on this host without opening any
/// database. Useful for the `db.connect` availability check and for tests that
/// should skip when the system library is absent.
pub fn is_available() -> bool {
    match open_sqlite() {
        Some(handle) => {
            // SAFETY: `handle` came from a successful `dlopen`.
            unsafe {
                dlclose(handle);
            }
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Per-test temp path under the gitignored `.tmp_tests/` directory.
    fn tmp_path(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp_tests");
        std::fs::create_dir_all(&dir).expect("create .tmp_tests");
        dir.join(name)
    }

    fn cleanup(path: &PathBuf) {
        // Remove the database and any WAL/SHM sidecar files.
        let _ = std::fs::remove_file(path);
        for ext in ["-wal", "-shm"] {
            let mut side = path.clone().into_os_string();
            side.push(ext);
            let _ = std::fs::remove_file(PathBuf::from(side));
        }
    }

    #[test]
    fn db_error_carries_code_and_renders_diagnostic() {
        // No system library needed: pure error/diagnostic plumbing.
        let err = DbError::invalid_handle();
        assert_eq!(err.code, "E0503");
        let diag = err.to_diagnostic();
        assert_eq!(diag.code.as_deref(), Some("E0503"));
        // Catalog hint is pre-filled for a registered code.
        assert!(diag.help.is_some());

        assert_eq!(DbError::backend_unavailable().code, "E0501");
        assert_eq!(DbError::permission("x").code, "E0501");
        assert_eq!(DbError::not_a_database("x").code, "E0502");
    }

    #[test]
    fn classify_open_error_maps_codes() {
        assert_eq!(classify_open_error(SQLITE_NOTADB, "", "p").code, "E0502");
        assert_eq!(classify_open_error(SQLITE_CORRUPT, "", "p").code, "E0502");
        assert_eq!(classify_open_error(SQLITE_CANTOPEN, "", "p").code, "E0501");
        assert_eq!(classify_open_error(SQLITE_PERM, "", "p").code, "E0501");
        assert_eq!(classify_open_error(SQLITE_READONLY, "", "p").code, "E0501");
    }

    #[test]
    fn open_creates_new_database_file() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let path = tmp_path("ffi_open_create.db");
        cleanup(&path);
        assert!(!path.exists(), "precondition: db file must not exist yet");

        let conn = SqliteConn::open(path.to_str().unwrap())
            .expect("open should create a fresh SQLite database");
        assert!(conn.is_valid());
        assert!(!conn.in_transaction());
        assert!(path.exists(), "open must create the database file (R5.2)");

        drop(conn);
        cleanup(&path);
    }

    #[test]
    fn close_is_idempotent_and_second_close_is_e0503() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let path = tmp_path("ffi_close_idem.db");
        cleanup(&path);

        let mut conn =
            SqliteConn::open(path.to_str().unwrap()).expect("open should succeed");
        // First close succeeds and invalidates the handle (R5.5).
        conn.close().expect("first close should succeed");
        assert!(!conn.is_valid());

        // Second close on the now-invalid handle → E0503 (R5.6).
        let err = conn.close().expect_err("second close must fail");
        assert_eq!(err.code, "E0503");

        // Operations on a closed handle are also rejected with E0503.
        assert_eq!(conn.query("SELECT 1", &[]).unwrap_err().code, "E0503");

        cleanup(&path);
    }

    #[test]
    fn open_non_sqlite_file_is_e0502() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let path = tmp_path("ffi_not_a_db.db");
        cleanup(&path);
        // Write bytes that are not a valid SQLite header.
        std::fs::write(&path, b"this is definitely not a sqlite database file\n")
            .expect("write junk file");

        let err = match SqliteConn::open(path.to_str().unwrap()) {
            Ok(_) => panic!("opening a non-SQLite file must fail"),
            Err(e) => e,
        };
        assert_eq!(err.code, "E0502", "got: {err}");

        cleanup(&path);
    }

    #[test]
    fn open_in_missing_directory_is_e0501() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        // Target a database under a directory that does not exist. SQLite's
        // open (even with SQLITE_OPEN_CREATE) cannot create the parent path, so
        // it fails with SQLITE_CANTOPEN, classified as an access failure
        // (E0501, R5.3). This avoids relying on filesystem permission bits,
        // which a privileged test runner could bypass.
        let dir = tmp_path("ffi_no_such_dir_e0501");
        // Ensure the directory really is absent.
        let _ = std::fs::remove_dir_all(&dir);
        let db_path = dir.join("inside.db");

        let err = match SqliteConn::open(db_path.to_str().unwrap()) {
            Ok(_) => panic!("opening under a missing directory must fail"),
            Err(e) => e,
        };
        assert_eq!(err.code, "E0501", "access failure → E0501; got: {err}");
        assert!(!db_path.exists(), "no database file should be created");
    }

    // --- task 3.2: query/exec + read type mapping ---------------------------

    /// Open a fresh DB with a small `items` table for the query/exec tests.
    fn open_with_items(name: &str) -> (SqliteConn, PathBuf) {
        let path = tmp_path(name);
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).expect("open should succeed");
        let affected = conn
            .exec(
                "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, price REAL, note TEXT);",
                &[],
            )
            .expect("CREATE TABLE should succeed");
        assert!(affected >= 0, "exec must report a non-negative row count");
        (conn, path)
    }

    #[test]
    fn exec_insert_with_params_reports_affected_rows() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_exec_insert.db");

        let affected = conn
            .exec(
                "INSERT INTO items (id, name, price, note) VALUES (?, ?, ?, ?);",
                &[
                    DbValue::Int(1),
                    DbValue::Str("widget".to_string()),
                    DbValue::Float(3.5),
                    DbValue::Null,
                ],
            )
            .expect("INSERT should succeed");
        assert_eq!(affected, 1, "one row affected (R6.3)");

        // UPDATE affecting the row also reports 1.
        let updated = conn
            .exec(
                "UPDATE items SET price = ? WHERE id = ?;",
                &[DbValue::Float(9.0), DbValue::Int(1)],
            )
            .expect("UPDATE should succeed");
        assert_eq!(updated, 1);

        // UPDATE matching nothing reports 0 (still >= 0).
        let none = conn
            .exec(
                "UPDATE items SET price = ? WHERE id = ?;",
                &[DbValue::Float(1.0), DbValue::Int(999)],
            )
            .expect("UPDATE with no match should succeed");
        assert_eq!(none, 0);

        cleanup(&path);
    }

    #[test]
    fn query_maps_all_base_types_including_null_and_empty_text() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_query_types.db");

        conn.exec(
            "INSERT INTO items (id, name, price, note) VALUES (?, ?, ?, ?);",
            &[
                DbValue::Int(1),
                DbValue::Str("widget".to_string()),
                DbValue::Float(3.5),
                DbValue::Null,
            ],
        )
        .unwrap();
        // Row with an empty TEXT value (R7.3).
        conn.exec(
            "INSERT INTO items (id, name, price, note) VALUES (?, ?, ?, ?);",
            &[
                DbValue::Int(2),
                DbValue::Str(String::new()),
                DbValue::Float(-2.25),
                DbValue::Str("ok".to_string()),
            ],
        )
        .unwrap();

        let rows = conn
            .query(
                "SELECT id, name, price, note FROM items WHERE id = ?;",
                &[DbValue::Int(1)],
            )
            .expect("query should succeed");
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row[0], ("id".to_string(), DbValue::Int(1)));
        assert_eq!(row[1], ("name".to_string(), DbValue::Str("widget".to_string())));
        assert_eq!(row[2], ("price".to_string(), DbValue::Float(3.5)));
        assert_eq!(row[3], ("note".to_string(), DbValue::Null)); // NULL→void

        // Empty TEXT reads back as an empty string, not NULL.
        let rows2 = conn
            .query("SELECT name FROM items WHERE id = ?;", &[DbValue::Int(2)])
            .unwrap();
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0][0].1, DbValue::Str(String::new()));

        cleanup(&path);
    }

    #[test]
    fn query_preserves_float_bit_pattern() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_query_float.db");

        for (id, v) in [
            (1_i64, std::f64::consts::PI),
            (2, 0.1 + 0.2),
            (3, f64::MIN_POSITIVE),
        ] {
            conn.exec(
                "INSERT INTO items (id, price) VALUES (?, ?);",
                &[DbValue::Int(id), DbValue::Float(v)],
            )
            .unwrap();
            let rows = conn
                .query("SELECT price FROM items WHERE id = ?;", &[DbValue::Int(id)])
                .unwrap();
            match rows[0][0].1 {
                DbValue::Float(got) => {
                    assert_eq!(got.to_bits(), v.to_bits(), "bit pattern preserved (R7.2)")
                }
                ref other => panic!("expected Float, got {other:?}"),
            }
        }

        cleanup(&path);
    }

    #[test]
    fn query_with_zero_rows_returns_empty_vec() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_query_empty.db");

        let rows = conn
            .query("SELECT id FROM items WHERE id = ?;", &[DbValue::Int(42)])
            .expect("query should succeed even with no rows");
        assert!(rows.is_empty(), "zero rows → empty Vec, no error (R6.2)");

        cleanup(&path);
    }

    #[test]
    fn param_count_mismatch_is_e0506() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_param_mismatch.db");

        // One placeholder, zero params supplied → E0506, no execution.
        let err = conn
            .query("SELECT id FROM items WHERE id = ?;", &[])
            .expect_err("mismatch must fail");
        assert_eq!(err.code, "E0506", "got: {err}");

        // Too many params is also rejected.
        let err2 = conn
            .exec(
                "INSERT INTO items (id) VALUES (?);",
                &[DbValue::Int(1), DbValue::Int(2)],
            )
            .expect_err("mismatch must fail");
        assert_eq!(err2.code, "E0506", "got: {err2}");

        cleanup(&path);
    }

    #[test]
    fn invalid_sql_is_e0504() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_invalid_sql.db");

        let err = conn
            .exec("SELEKT bogus FROM nope", &[])
            .expect_err("invalid SQL must fail");
        assert_eq!(err.code, "E0504", "got: {err}");

        let err2 = conn
            .query("SELECT FROM WHERE", &[])
            .expect_err("invalid SQL must fail");
        assert_eq!(err2.code, "E0504", "got: {err2}");

        cleanup(&path);
    }

    #[test]
    fn blob_column_is_e0507() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let path = tmp_path("ffi_blob_reject.db");
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).unwrap();
        conn.exec("CREATE TABLE blobs (id INTEGER, data BLOB);", &[])
            .unwrap();
        // Insert a BLOB literal so the stored value type is BLOB.
        conn.exec("INSERT INTO blobs (id, data) VALUES (1, x'00ff10');", &[])
            .unwrap();

        let err = conn
            .query("SELECT data FROM blobs WHERE id = ?;", &[DbValue::Int(1)])
            .expect_err("reading a BLOB column must fail");
        assert_eq!(err.code, "E0507", "got: {err}");

        cleanup(&path);
    }

    // --- task 3.3: monetary decimal mapping (R7.5/R7.7) ---------------------

    /// `parse_money` is the pure read primitive; exercise it without a DB.
    #[test]
    fn parse_money_accepts_exact_decimals_and_rejects_garbage() {
        // Valid exact decimals round-trip through `to_string` unchanged,
        // preserving scale (trailing zeros) and sign.
        for s in ["19.99", "0.00", "-12345.6789", "0.000000001", "42", "1000.50"] {
            let dec = parse_money(s).expect("valid money string should parse");
            assert_eq!(dec.to_string(), s, "exact decimal text preserved");
        }

        // Malformed input → E0508, and no decimal is produced.
        for bad in ["", "abc", "1.2.3", "12,34", "$5.00", "1.0e3"] {
            let err = parse_money(bad).expect_err("malformed money must be rejected");
            assert_eq!(err.code, "E0508", "got: {err} for input {bad:?}");
        }

        // Out-of-range (mantissa exceeds i128) → E0508, still no decimal.
        let huge = "1".repeat(40); // 40 digits overflows i128 (~38 digits)
        let err = parse_money(&huge).expect_err("out-of-range money must be rejected");
        assert_eq!(err.code, "E0508", "got: {err}");
    }

    /// Open a fresh DB with a `ledger` table whose `amount` column is TEXT
    /// (the exact-decimal money convention).
    fn open_with_ledger(name: &str) -> (SqliteConn, PathBuf) {
        let path = tmp_path(name);
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).expect("open should succeed");
        conn.exec(
            "CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount TEXT);",
            &[],
        )
        .expect("CREATE TABLE should succeed");
        (conn, path)
    }

    #[test]
    fn money_written_as_text_reads_back_exactly() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_ledger("ffi_money_roundtrip.db");

        // Edge cases: cents, exact zero, negative, large scale, trailing zeros.
        let amounts = ["19.99", "0.00", "-250.75", "0.000000001", "1000.50"];
        for (i, s) in amounts.iter().enumerate() {
            let dec = Decimal::parse(s).unwrap();
            conn.exec(
                "INSERT INTO ledger (id, amount) VALUES (?, ?);",
                &[DbValue::Int(i as i64), DbValue::Decimal(dec)],
            )
            .expect("INSERT of a Decimal (bound as TEXT) should succeed");
        }

        // Without money handling, the column reads back as exact TEXT.
        let raw = conn
            .query("SELECT amount FROM ledger WHERE id = ?;", &[DbValue::Int(0)])
            .unwrap();
        assert_eq!(raw[0][0].1, DbValue::Str("19.99".to_string()));

        // With money handling, the chosen column becomes a Decimal that is
        // decimally identical (digits + scale) to the value written (R7.5).
        for (i, s) in amounts.iter().enumerate() {
            let rows = conn
                .query_money(
                    "SELECT id, amount FROM ledger WHERE id = ?;",
                    &[DbValue::Int(i as i64)],
                    &["amount"],
                )
                .expect("query_money should succeed");
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0][1].1,
                DbValue::Decimal(Decimal::parse(s).unwrap()),
                "money column `{s}` must round-trip exactly"
            );
            // The non-money `id` column keeps its base-type mapping (task 3.2).
            assert_eq!(rows[0][0].1, DbValue::Int(i as i64));
        }

        cleanup(&path);
    }

    #[test]
    fn malformed_money_text_is_e0508() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_ledger("ffi_money_malformed.db");

        // A non-decimal string slipped into a money column.
        conn.exec(
            "INSERT INTO ledger (id, amount) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Str("not-a-number".to_string())],
        )
        .unwrap();

        let err = conn
            .query_money(
                "SELECT amount FROM ledger WHERE id = ?;",
                &[DbValue::Int(1)],
                &["amount"],
            )
            .expect_err("malformed money must abort with E0508");
        assert_eq!(err.code, "E0508", "got: {err}");

        // Reading the same column WITHOUT money handling still succeeds as TEXT,
        // proving the base-type mapping (task 3.2) is untouched.
        let ok = conn
            .query("SELECT amount FROM ledger WHERE id = ?;", &[DbValue::Int(1)])
            .expect("plain query of the TEXT column should still succeed");
        assert_eq!(ok[0][0].1, DbValue::Str("not-a-number".to_string()));

        cleanup(&path);
    }

    #[test]
    fn money_null_stays_void_and_numeric_storage_is_e0508() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_ledger("ffi_money_null_numeric.db");

        // NULL amount: a missing money value stays void, not a decimal.
        conn.exec(
            "INSERT INTO ledger (id, amount) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Null],
        )
        .unwrap();
        let rows = conn
            .query_money(
                "SELECT amount FROM ledger WHERE id = ?;",
                &[DbValue::Int(1)],
                &["amount"],
            )
            .expect("NULL money should be preserved, not rejected");
        assert_eq!(rows[0][0].1, DbValue::Null);

        // A money column whose storage class is REAL (lossy) → E0508. A column
        // with REAL affinity keeps the value as REAL, so query_money sees a
        // Float rather than exact-decimal TEXT and refuses to fabricate a
        // decimal from it (R7.7).
        conn.exec("CREATE TABLE prices (id INTEGER PRIMARY KEY, amount REAL);", &[])
            .unwrap();
        conn.exec(
            "INSERT INTO prices (id, amount) VALUES (?, ?);",
            &[DbValue::Int(2), DbValue::Float(19.99)],
        )
        .unwrap();
        let err = conn
            .query_money(
                "SELECT amount FROM prices WHERE id = ?;",
                &[DbValue::Int(2)],
                &["amount"],
            )
            .expect_err("REAL-stored money must be rejected with E0508");
        assert_eq!(err.code, "E0508", "got: {err}");

        cleanup(&path);
    }

    // --- task 3.4: transactions (begin/commit/rollback + atomicity) ---------

    /// A constraint violation surfaces as the handleable `E0505`, including its
    /// SQLite *extended* result codes (low byte == 19). Pure classifier check,
    /// no system library required.
    #[test]
    fn classify_stmt_error_maps_constraint_to_e0505() {
        // Primary constraint code.
        assert_eq!(classify_stmt_error(SQLITE_CONSTRAINT, "").code, "E0505");
        // Extended codes share the low byte (e.g. SQLITE_CONSTRAINT_UNIQUE =
        // 19 | (8<<8) = 2067, SQLITE_CONSTRAINT_NOTNULL = 19 | (5<<8) = 1299).
        assert_eq!(classify_stmt_error(2067, "").code, "E0505");
        assert_eq!(classify_stmt_error(1299, "").code, "E0505");
        // The E0505 hint is handleable per the catalog.
        assert!(classify_stmt_error(SQLITE_CONSTRAINT, "").to_diagnostic().help.is_some());
        // A generic SQL error still falls through to E0504.
        assert_eq!(classify_stmt_error(1, "").code, "E0504");
    }

    fn row_count(conn: &SqliteConn) -> i64 {
        let rows = conn
            .query("SELECT COUNT(*) FROM items;", &[])
            .expect("count query should succeed");
        match rows[0][0].1 {
            DbValue::Int(n) => n,
            ref other => panic!("expected Int count, got {other:?}"),
        }
    }

    #[test]
    fn begin_commit_persists_changes() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (mut conn, path) = open_with_items("ffi_tx_commit.db");
        assert!(!conn.in_transaction());

        conn.begin().expect("begin should start a transaction");
        assert!(conn.in_transaction(), "in_tx must be set after begin");

        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Str("kept".to_string())],
        )
        .unwrap();

        conn.commit().expect("commit should succeed");
        assert!(!conn.in_transaction(), "in_tx must clear after commit");

        // The inserted row is durable after commit.
        assert_eq!(row_count(&conn), 1, "committed row persists (R8.3)");

        cleanup(&path);
    }

    #[test]
    fn begin_rollback_restores_prior_state_exactly() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (mut conn, path) = open_with_items("ffi_tx_rollback.db");

        // Establish a baseline row, outside any transaction.
        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Str("base".to_string())],
        )
        .unwrap();
        let before = row_count(&conn);
        assert_eq!(before, 1);

        conn.begin().expect("begin should succeed");
        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(2), DbValue::Str("temp".to_string())],
        )
        .unwrap();
        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(3), DbValue::Str("temp2".to_string())],
        )
        .unwrap();
        // Inside the transaction the new rows are visible.
        assert_eq!(row_count(&conn), 3);

        conn.rollback().expect("rollback should succeed");
        assert!(!conn.in_transaction(), "in_tx must clear after rollback");

        // State returns exactly to before the transaction (R8.4).
        assert_eq!(row_count(&conn), before, "rollback restores prior state");
        let rows = conn
            .query("SELECT id, name FROM items;", &[])
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].1, DbValue::Int(1));
        assert_eq!(rows[0][1].1, DbValue::Str("base".to_string()));

        cleanup(&path);
    }

    #[test]
    fn begin_while_active_is_e0509_and_keeps_transaction() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (mut conn, path) = open_with_items("ffi_tx_double_begin.db");

        conn.begin().expect("first begin should succeed");
        let err = conn.begin().expect_err("second begin must fail");
        assert_eq!(err.code, "E0509", "got: {err}");
        // The original transaction is untouched and still active.
        assert!(conn.in_transaction(), "in_tx stays true after rejected begin");

        // Cleanly close out the still-open transaction.
        conn.rollback().expect("rollback should succeed");
        cleanup(&path);
    }

    #[test]
    fn commit_or_rollback_without_active_tx_is_e0510() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (mut conn, path) = open_with_items("ffi_tx_no_active.db");
        assert!(!conn.in_transaction());

        let c = conn.commit().expect_err("commit with no tx must fail");
        assert_eq!(c.code, "E0510", "got: {c}");

        let r = conn.rollback().expect_err("rollback with no tx must fail");
        assert_eq!(r.code, "E0510", "got: {r}");

        // Neither call started a transaction.
        assert!(!conn.in_transaction());

        cleanup(&path);
    }

    #[test]
    fn constraint_violation_on_exec_is_e0505_and_leaves_state_unchanged() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (conn, path) = open_with_items("ffi_tx_constraint.db");

        // UNIQUE/PRIMARY KEY violation: insert id=1 twice.
        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Str("first".to_string())],
        )
        .unwrap();
        let before = row_count(&conn);

        let err = conn
            .exec(
                "INSERT INTO items (id, name) VALUES (?, ?);",
                &[DbValue::Int(1), DbValue::Str("dup".to_string())],
            )
            .expect_err("duplicate primary key must violate a constraint");
        assert_eq!(err.code, "E0505", "PK violation → E0505; got: {err}");

        // The failed command left the table exactly as before (statement
        // atomicity, R8.6): no extra row, original value intact.
        assert_eq!(row_count(&conn), before, "constraint failure changes nothing");
        let rows = conn
            .query("SELECT name FROM items WHERE id = ?;", &[DbValue::Int(1)])
            .unwrap();
        assert_eq!(rows[0][0].1, DbValue::Str("first".to_string()));

        cleanup(&path);
    }

    #[test]
    fn not_null_constraint_on_exec_is_e0505() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let path = tmp_path("ffi_tx_notnull.db");
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).unwrap();
        conn.exec(
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
            &[],
        )
        .unwrap();

        let err = conn
            .exec(
                "INSERT INTO people (id, name) VALUES (?, ?);",
                &[DbValue::Int(1), DbValue::Null],
            )
            .expect_err("NOT NULL violation must fail");
        assert_eq!(err.code, "E0505", "NOT NULL → E0505; got: {err}");

        // Table is unchanged by the failed command.
        let rows = conn.query("SELECT COUNT(*) FROM people;", &[]).unwrap();
        assert_eq!(rows[0][0].1, DbValue::Int(0));

        cleanup(&path);
    }

    #[test]
    fn constraint_violation_inside_tx_auto_rolls_back_and_clears_tx() {
        if !is_available() {
            eprintln!("skip: libsqlite3 tidak tersedia pada host ini");
            return;
        }
        let (mut conn, path) = open_with_items("ffi_tx_auto_rollback.db");

        // Baseline row committed outside any transaction.
        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Str("base".to_string())],
        )
        .unwrap();
        let before = row_count(&conn);
        assert_eq!(before, 1);

        conn.begin().expect("begin should start a transaction");
        assert!(conn.in_transaction());

        // A valid write inside the transaction (visible until rollback).
        conn.exec(
            "INSERT INTO items (id, name) VALUES (?, ?);",
            &[DbValue::Int(2), DbValue::Str("pending".to_string())],
        )
        .unwrap();
        assert_eq!(row_count(&conn), 2, "pending row visible within the tx");

        // A constraint-violating write (duplicate PK) → E0505 and triggers an
        // automatic ROLLBACK of the whole transaction (R6.7/R8.6).
        let err = conn
            .exec(
                "INSERT INTO items (id, name) VALUES (?, ?);",
                &[DbValue::Int(1), DbValue::Str("dup".to_string())],
            )
            .expect_err("duplicate primary key must violate a constraint");
        assert_eq!(err.code, "E0505", "constraint → E0505; got: {err}");

        // The transaction was rolled back: the active-tx flag is cleared and the
        // database is back at its pre-transaction state — both the failed
        // command *and* the earlier `pending` row are gone.
        assert!(
            !conn.in_transaction(),
            "auto-rollback must clear in_tx (R8.6)"
        );
        assert_eq!(
            row_count(&conn),
            before,
            "auto-rollback restores the pre-transaction state"
        );
        let rows = conn.query("SELECT id, name FROM items;", &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].1, DbValue::Int(1));
        assert_eq!(rows[0][1].1, DbValue::Str("base".to_string()));

        // With no transaction active anymore, commit/rollback now report E0510.
        assert_eq!(
            conn.commit().expect_err("commit with no active tx").code,
            "E0510"
        );

        cleanup(&path);
    }
}

// ============================================================================
// Property-Based Tests (P2/P3/P4/P5/P10) untuk backend SQLite.
//
// Memetakan Correctness Properties dari design.md ke pengujian terhadap
// `libsqlite3` nyata via FFI. Konsisten dengan harness std-only di
// `support::pbt` (tanpa crate eksternal): RNG seedable, generator/shrinker
// minimal, minimum 100 kasus per property, seed dicetak saat gagal.
//
// Setiap test memakai strategi availability-skip: bila `libsqlite3` tak tersedia
// di host, test di-skip dengan pesan jelas (bukan gagal) agar CI tetap hijau.
// Seluruh database uji ditulis ke `.tmp_tests/` (gitignored) dan dibersihkan
// (termasuk sidecar `-wal`/`-shm`) pada akhir tiap test.
// ============================================================================
#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};
    use std::cell::RefCell;
    use std::cmp::Ordering;
    use std::path::{Path, PathBuf};

    /// Hapus database uji beserta sidecar WAL/SHM-nya.
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        for ext in ["-wal", "-shm"] {
            let mut side = path.as_os_str().to_os_string();
            side.push(ext);
            let _ = std::fs::remove_file(PathBuf::from(side));
        }
    }

    // ---- Generator bersama --------------------------------------------------

    /// Generator string UTF-8 yang menyaring byte NUL (U+0000).
    ///
    /// Jalur baca TEXT pada lapisan FFI ini bersifat NUL-terminated
    /// (`column_text` dibaca sebagai C string), sehingga ruang masukan TEXT yang
    /// didukung tidak memuat NUL tertanam. Menyaring NUL membatasi generator ke
    /// ruang masukan yang valid bagi antarmuka C string (R8.3), bukan menutupi
    /// bug: nilai TEXT lain (termasuk string kosong dan multi-byte) tetap diuji
    /// secara byte-identik.
    fn text_no_nul(max: usize) -> Gen<String> {
        let base = pbt::string(max);
        Gen::new(
            move |rng, size| base.generate(rng, size).chars().filter(|c| *c != '\0').collect(),
            |s: &String| {
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![String::new()]
                }
            },
        )
    }

    /// Generator satu nilai kolom: i64 (termasuk tepi), f64 (termasuk
    /// NaN/±inf/-0.0/subnormal), TEXT UTF-8 (termasuk kosong), dan NULL.
    fn db_value_gen() -> Gen<DbValue> {
        let ints = pbt::i64_any();
        let floats = pbt::f64_any();
        let texts = text_no_nul(48);
        Gen::new(
            move |rng, size| match rng.below(4) {
                0 => DbValue::Int(ints.generate(rng, size)),
                1 => DbValue::Float(floats.generate(rng, size)),
                2 => DbValue::Str(texts.generate(rng, size)),
                _ => DbValue::Null,
            },
            |_| Vec::new(),
        )
    }

    /// Generator nilai moneter eksak. Tepi: nol (skala 0 dan skala besar),
    /// negatif, skala besar, dan nol di ujung (trailing zeros).
    ///
    /// Mantissa dibatasi ke rentang 64-bit bertanda agar string desimalnya
    /// selalu dapat di-parse ulang tanpa luber (round-trip pasti eksak).
    fn decimal_gen() -> Gen<Decimal> {
        Gen::new(
            |rng, _size| match rng.below(8) {
                0 => Decimal::new(0, 0),                       // nol murni
                1 => Decimal::new(0, rng.below(18) as u32 + 1), // nol berskala: "0.000"
                2 => Decimal::parse("19.99").unwrap(),         // sen klasik
                3 => Decimal::parse("-250.7500").unwrap(),     // negatif + trailing zeros
                4 => {
                    // Trailing zeros: kelipatan 100 pada skala 2 (mis. "15.00").
                    let m = rng.range_i64(-100_000, 100_000) as i128 * 100;
                    Decimal::new(m, 2)
                }
                _ => {
                    let mantissa = rng.next_u64() as i64 as i128; // rentang i64 penuh
                    let scale = rng.below(19) as u32; // 0..=18
                    Decimal::new(mantissa, scale)
                }
            },
            |_| Vec::new(),
        )
    }

    /// Nilai moneter realistis untuk skenario buku besar (mantissa kecil, skala
    /// ≤4) sehingga aritmetika decimal pada penjumlahan saldo tidak pernah luber.
    fn money_decimal(rng: &mut Rng) -> Decimal {
        let mantissa = rng.range_i64(-1_000_000, 1_000_000) as i128;
        let scale = rng.below(5) as u32; // 0..=4
        Decimal::new(mantissa, scale)
    }

    // ========================================================================
    // Property 2 (task 3.7): Round-trip tipe SQLite <-> Ran.
    // ========================================================================

    // Feature: enterprise-runtime-capabilities, Property 2: Round-trip tipe SQLite <-> Ran
    // Validates: Requirements 8.1, 8.2, 8.3, 8.4
    #[test]
    fn prop_db_value_roundtrip_preserves_type_and_value() {
        if pbt::skip_if_unavailable("libsqlite3", pbt::SQLITE_LIBS) {
            return;
        }
        let path = pbt::unique_tmp_path("prop_p2_roundtrip", "db");
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).expect("open temp db");
        // Kolom `v` dideklarasikan tanpa tipe (afinitas NONE) sehingga storage
        // class tiap nilai yang diikat (INTEGER/REAL/TEXT/NULL) dipertahankan
        // apa adanya tanpa koersi afinitas; pembacaan kembali memetakan ke tipe
        // Ran yang sama.
        conn.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, v);", &[])
            .expect("create table");

        let gen = db_value_gen();
        pbt::for_all("prop_db_value_roundtrip", &gen, |written: &DbValue| {
            if conn.exec("DELETE FROM t;", &[]).is_err() {
                return false;
            }
            if conn
                .exec(
                    "INSERT INTO t (id, v) VALUES (?, ?);",
                    &[DbValue::Int(1), written.clone()],
                )
                .is_err()
            {
                return false;
            }
            let rows = match conn.query("SELECT v FROM t WHERE id = ?;", &[DbValue::Int(1)]) {
                Ok(r) => r,
                Err(_) => return false,
            };
            if rows.len() != 1 {
                return false;
            }
            let got = &rows[0][0].1;
            match written {
                // INTEGER bitwise-equal i64, TEXT byte-identik, NULL -> Null:
                // kesetaraan struktural `DbValue` sudah eksak untuk ketiganya.
                DbValue::Int(_) | DbValue::Str(_) | DbValue::Null => got == written,
                // Keterbatasan SQLite yang terdokumentasi: mengikat double NaN
                // disimpan sebagai NULL (tidak ada storage REAL untuk NaN),
                // sehingga NaN yang ditulis terbaca kembali sebagai void.
                DbValue::Float(f) if f.is_nan() => matches!(got, DbValue::Null),
                // REAL: bit-pattern dipertahankan (mencakup ±inf, -0.0, subnormal).
                DbValue::Float(f) => match got {
                    DbValue::Float(g) => g.to_bits() == f.to_bits(),
                    _ => false,
                },
                DbValue::Decimal(_) => false, // tidak dihasilkan generator ini
            }
        });

        drop(conn);
        cleanup(&path);
    }

    // ========================================================================
    // Property 3 (task 3.8): Eksaktnes desimal nilai moneter.
    // ========================================================================

    // Feature: enterprise-runtime-capabilities, Property 3: Eksaktnes desimal nilai moneter
    // Validates: Requirements 8.5
    #[test]
    fn prop_money_decimal_exact_roundtrip() {
        if pbt::skip_if_unavailable("libsqlite3", pbt::SQLITE_LIBS) {
            return;
        }
        let path = pbt::unique_tmp_path("prop_p3_money", "db");
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).expect("open temp db");
        // Uang disimpan sebagai TEXT desimal eksak (konvensi R7.5/R8.5).
        conn.exec("CREATE TABLE ledger (id INTEGER PRIMARY KEY, amount TEXT);", &[])
            .expect("create table");

        let gen = decimal_gen();
        pbt::for_all("prop_money_decimal_exact_roundtrip", &gen, |d: &Decimal| {
            if conn.exec("DELETE FROM ledger;", &[]).is_err() {
                return false;
            }
            // Ditulis sebagai Decimal (diikat sebagai TEXT eksak, tanpa float).
            if conn
                .exec(
                    "INSERT INTO ledger (id, amount) VALUES (?, ?);",
                    &[DbValue::Int(1), DbValue::Decimal(*d)],
                )
                .is_err()
            {
                return false;
            }
            // Dibaca via query_money -> direkonstruksi sebagai Decimal.
            let rows = match conn.query_money(
                "SELECT amount FROM ledger WHERE id = ?;",
                &[DbValue::Int(1)],
                &["amount"],
            ) {
                Ok(r) => r,
                Err(_) => return false,
            };
            if rows.len() != 1 {
                return false;
            }
            // Oracle: kesetaraan `Decimal` pada nilai + skala (string desimal
            // eksak), bukan via float. `DbValue::Decimal` membandingkan melalui
            // `Decimal::to_string`, sehingga digit dan skala (trailing zeros)
            // harus sama persis.
            rows[0][0].1 == DbValue::Decimal(*d)
        });

        drop(conn);
        cleanup(&path);
    }

    // ========================================================================
    // Property 4 (task 3.9): Invariant saldo transaksi (zero-sum).
    // ========================================================================

    /// Skenario buku besar: N akun + urutan transfer seimbang (debit satu akun,
    /// kredit akun lain dengan jumlah sama). `from != to` dijamin oleh generator.
    #[derive(Clone, Debug)]
    struct LedgerScenario {
        balances: Vec<Decimal>,
        transfers: Vec<(usize, usize, Decimal)>,
    }

    fn ledger_scenario_gen() -> Gen<LedgerScenario> {
        Gen::new(
            |rng, _size| {
                let n = 2 + rng.below(5) as usize; // 2..=6 akun
                let balances: Vec<Decimal> = (0..n).map(|_| money_decimal(rng)).collect();
                let m = rng.below(8) as usize; // 0..=7 transfer
                let mut transfers = Vec::with_capacity(m);
                for _ in 0..m {
                    let from = rng.below(n as u64) as usize;
                    // Pastikan tujuan berbeda dari sumber (transfer benar-benar
                    // memindahkan nilai antar dua akun, bukan ke diri sendiri).
                    let step = 1 + rng.below((n - 1) as u64) as usize;
                    let to = (from + step) % n;
                    transfers.push((from, to, money_decimal(rng)));
                }
                LedgerScenario { balances, transfers }
            },
            |_| Vec::new(),
        )
    }

    /// Total saldo seluruh akun (aritmetika decimal eksak; bukan float).
    fn sum_balances(conn: &SqliteConn) -> Option<Decimal> {
        let rows = conn
            .query_money("SELECT balance FROM accounts ORDER BY id;", &[], &["balance"])
            .ok()?;
        let mut acc = Decimal::zero();
        for row in &rows {
            match row[0].1 {
                DbValue::Decimal(d) => acc = acc.add(&d).ok()?,
                _ => return None,
            }
        }
        Some(acc)
    }

    /// Baca saldo satu akun sebagai Decimal eksak.
    fn read_balance(conn: &SqliteConn, id: i64) -> Option<Decimal> {
        let rows = conn
            .query_money(
                "SELECT balance FROM accounts WHERE id = ?;",
                &[DbValue::Int(id)],
                &["balance"],
            )
            .ok()?;
        match rows.first().map(|r| r[0].1.clone()) {
            Some(DbValue::Decimal(d)) => Some(d),
            _ => None,
        }
    }

    // Feature: enterprise-runtime-capabilities, Property 4: Invariant saldo transaksi (zero-sum)
    // Validates: Requirements 9.7
    #[test]
    fn prop_balanced_transfers_preserve_total_balance() {
        if pbt::skip_if_unavailable("libsqlite3", pbt::SQLITE_LIBS) {
            return;
        }
        let path = pbt::unique_tmp_path("prop_p4_zerosum", "db");
        cleanup(&path);
        let conn = RefCell::new(SqliteConn::open(path.to_str().unwrap()).expect("open temp db"));

        let gen = ledger_scenario_gen();
        pbt::for_all(
            "prop_balanced_transfers_preserve_total_balance",
            &gen,
            |sc: &LedgerScenario| {
                let mut c = conn.borrow_mut();
                // State awal segar tiap kasus.
                if c.exec("DROP TABLE IF EXISTS accounts;", &[]).is_err() {
                    return false;
                }
                if c
                    .exec(
                        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, balance TEXT NOT NULL);",
                        &[],
                    )
                    .is_err()
                {
                    return false;
                }
                for (i, b) in sc.balances.iter().enumerate() {
                    if c
                        .exec(
                            "INSERT INTO accounts (id, balance) VALUES (?, ?);",
                            &[DbValue::Int(i as i64), DbValue::Decimal(*b)],
                        )
                        .is_err()
                    {
                        return false;
                    }
                }

                let before = match sum_balances(&c) {
                    Some(s) => s,
                    None => return false,
                };

                // Tiap transfer dijalankan sebagai transaksi nyata yang di-commit.
                for (from, to, amount) in &sc.transfers {
                    if c.begin().is_err() {
                        return false;
                    }
                    let from_bal = match read_balance(&c, *from as i64) {
                        Some(b) => b,
                        None => {
                            let _ = c.rollback();
                            return false;
                        }
                    };
                    let to_bal = match read_balance(&c, *to as i64) {
                        Some(b) => b,
                        None => {
                            let _ = c.rollback();
                            return false;
                        }
                    };
                    let new_from = match from_bal.sub(amount) {
                        Ok(v) => v,
                        Err(_) => {
                            let _ = c.rollback();
                            return false;
                        }
                    };
                    let new_to = match to_bal.add(amount) {
                        Ok(v) => v,
                        Err(_) => {
                            let _ = c.rollback();
                            return false;
                        }
                    };
                    if c
                        .exec(
                            "UPDATE accounts SET balance = ? WHERE id = ?;",
                            &[DbValue::Decimal(new_from), DbValue::Int(*from as i64)],
                        )
                        .is_err()
                    {
                        let _ = c.rollback();
                        return false;
                    }
                    if c
                        .exec(
                            "UPDATE accounts SET balance = ? WHERE id = ?;",
                            &[DbValue::Decimal(new_to), DbValue::Int(*to as i64)],
                        )
                        .is_err()
                    {
                        let _ = c.rollback();
                        return false;
                    }
                    if c.commit().is_err() {
                        return false;
                    }
                }

                let after = match sum_balances(&c) {
                    Some(s) => s,
                    None => return false,
                };

                // Oracle: total saldo setelah == total saldo sebelum, persis,
                // dengan aritmetika decimal eksak (selisih nol).
                before.cmp(&after) == Ordering::Equal
            },
        );

        cleanup(&path);
    }

    // ========================================================================
    // Property 5 (task 3.10): Rollback memulihkan state sebelumnya secara persis.
    // ========================================================================

    /// Satu operasi tulis dalam urutan transaksi. Sebagian sengaja melanggar
    /// constraint (PK duplikat / NOT NULL) untuk memicu auto-rollback (R6.7/R8.6).
    #[derive(Clone, Debug)]
    enum WriteOp {
        /// INSERT id baru (di luar rentang snapshot) — umumnya valid.
        InsertNew(i64, String),
        /// INSERT id yang sudah ada di snapshot — melanggar PRIMARY KEY.
        InsertDupPk(i64, String),
        /// INSERT dengan nama NULL — melanggar NOT NULL.
        InsertNullName(i64),
        /// UPDATE nama sebuah id (ada/tidak ada).
        Update(i64, String),
        /// DELETE sebuah id (ada/tidak ada).
        Delete(i64),
    }

    #[derive(Clone, Debug)]
    struct RollbackScenario {
        /// Baris awal (id unik 0..k, nama non-NULL) yang di-commit sebelum transaksi.
        snapshot: Vec<(i64, String)>,
        /// Urutan tulis di dalam transaksi.
        ops: Vec<WriteOp>,
    }

    fn rollback_scenario_gen() -> Gen<RollbackScenario> {
        let names = text_no_nul(16);
        Gen::new(
            move |rng, size| {
                let k = 1 + rng.below(5) as usize; // 1..=5 baris snapshot
                let snapshot: Vec<(i64, String)> = (0..k)
                    .map(|i| (i as i64, names.generate(rng, size)))
                    .collect();
                let m = rng.below(8) as usize; // 0..=7 operasi
                let mut ops = Vec::with_capacity(m);
                for _ in 0..m {
                    let op = match rng.below(6) {
                        0 => WriteOp::InsertNew(k as i64 + rng.range_i64(0, 5), names.generate(rng, size)),
                        1 => WriteOp::InsertDupPk(rng.below(k as u64) as i64, names.generate(rng, size)),
                        2 => WriteOp::InsertNullName(rng.range_i64(0, (k + 5) as i64)),
                        3 => WriteOp::Update(rng.range_i64(0, (k + 5) as i64), names.generate(rng, size)),
                        4 => WriteOp::Delete(rng.below(k as u64) as i64),
                        _ => WriteOp::Delete(rng.range_i64(0, (k + 5) as i64)),
                    };
                    ops.push(op);
                }
                RollbackScenario { snapshot, ops }
            },
            |_| Vec::new(),
        )
    }

    /// Snapshot baris tabel `t` (id, name) terurut menurut id.
    fn snapshot_rows(conn: &SqliteConn) -> Option<Vec<(i64, String)>> {
        let rows = conn.query("SELECT id, name FROM t ORDER BY id;", &[]).ok()?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = match row[0].1 {
                DbValue::Int(n) => n,
                _ => return None,
            };
            let name = match &row[1].1 {
                DbValue::Str(s) => s.clone(),
                _ => return None,
            };
            out.push((id, name));
        }
        Some(out)
    }

    // Feature: enterprise-runtime-capabilities, Property 5: Rollback memulihkan state sebelumnya secara persis
    // Validates: Requirements 7.7, 9.4, 9.6
    #[test]
    fn prop_rollback_restores_exact_prior_state() {
        if pbt::skip_if_unavailable("libsqlite3", pbt::SQLITE_LIBS) {
            return;
        }
        let path = pbt::unique_tmp_path("prop_p5_rollback", "db");
        cleanup(&path);
        let conn = RefCell::new(SqliteConn::open(path.to_str().unwrap()).expect("open temp db"));

        let gen = rollback_scenario_gen();
        pbt::for_all(
            "prop_rollback_restores_exact_prior_state",
            &gen,
            |sc: &RollbackScenario| {
                let mut c = conn.borrow_mut();
                if c.exec("DROP TABLE IF EXISTS t;", &[]).is_err() {
                    return false;
                }
                if c
                    .exec("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL);", &[])
                    .is_err()
                {
                    return false;
                }
                for (id, name) in &sc.snapshot {
                    if c
                        .exec(
                            "INSERT INTO t (id, name) VALUES (?, ?);",
                            &[DbValue::Int(*id), DbValue::Str(name.clone())],
                        )
                        .is_err()
                    {
                        return false;
                    }
                }
                let baseline = match snapshot_rows(&c) {
                    Some(b) => b,
                    None => return false,
                };

                if c.begin().is_err() {
                    return false;
                }
                for op in &sc.ops {
                    // Bila constraint sudah memicu auto-rollback, transaksi
                    // tidak aktif lagi; berhenti agar tulisan berikutnya tidak
                    // berjalan di luar transaksi dan mengubah state ter-commit.
                    if !c.in_transaction() {
                        break;
                    }
                    let res = match op {
                        WriteOp::InsertNew(id, nm) | WriteOp::InsertDupPk(id, nm) => c.exec(
                            "INSERT INTO t (id, name) VALUES (?, ?);",
                            &[DbValue::Int(*id), DbValue::Str(nm.clone())],
                        ),
                        WriteOp::InsertNullName(id) => c.exec(
                            "INSERT INTO t (id, name) VALUES (?, ?);",
                            &[DbValue::Int(*id), DbValue::Null],
                        ),
                        WriteOp::Update(id, nm) => c.exec(
                            "UPDATE t SET name = ? WHERE id = ?;",
                            &[DbValue::Str(nm.clone()), DbValue::Int(*id)],
                        ),
                        WriteOp::Delete(id) => {
                            c.exec("DELETE FROM t WHERE id = ?;", &[DbValue::Int(*id)])
                        }
                    };
                    // Error constraint (E0505) sah: ia memicu auto-rollback dan
                    // membersihkan flag transaksi; ditangani oleh guard di atas.
                    let _ = res;
                }

                // Bila transaksi selamat (tak ada pelanggaran), rollback eksplisit.
                if c.in_transaction() && c.rollback().is_err() {
                    return false;
                }

                // Oracle: tidak ada transaksi aktif, dan state identik dengan
                // snapshot pra-transaksi (baik via rollback eksplisit maupun
                // auto-rollback akibat constraint).
                if c.in_transaction() {
                    return false;
                }
                match snapshot_rows(&c) {
                    Some(after) => after == baseline,
                    None => false,
                }
            },
        );

        cleanup(&path);
    }

    // ========================================================================
    // Property 10 (task 3.11): Integritas parameter binding (anti-injeksi).
    // ========================================================================

    /// Generator nilai string adversarial (payload injeksi klasik + unicode)
    /// dicampur string acak. Menyaring NUL (lihat `text_no_nul`).
    fn adversarial_string_gen() -> Gen<String> {
        const ADV: &[&str] = &[
            "' OR 1=1 --",
            "'; DROP TABLE canary;--",
            "\"; DROP TABLE target; --",
            "1); DELETE FROM canary; --",
            "' UNION SELECT secret FROM canary --",
            "robert'); DROP TABLE target;--",
            "admin'--",
            "\\'; DROP TABLE canary; --",
            "ω≈ç√∫˜µ≤≥÷",
            "你好'); DROP TABLE canary;--",
            "𝔘𝔫𝔦𝔠𝔬𝔡𝔢",
            "",
        ];
        let texts = text_no_nul(48);
        Gen::new(
            move |rng, size| {
                if rng.below(3) == 0 {
                    texts.generate(rng, size)
                } else {
                    (*rng.choose(ADV)).to_string()
                }
            },
            |_| Vec::new(),
        )
    }

    // Feature: enterprise-runtime-capabilities, Property 10: Integritas parameter binding (anti-injeksi)
    // Validates: Requirements 7.4
    #[test]
    fn prop_parameter_binding_resists_injection() {
        if pbt::skip_if_unavailable("libsqlite3", pbt::SQLITE_LIBS) {
            return;
        }
        let path = pbt::unique_tmp_path("prop_p10_injection", "db");
        cleanup(&path);
        let conn = SqliteConn::open(path.to_str().unwrap()).expect("open temp db");
        conn.exec("CREATE TABLE target (id INTEGER PRIMARY KEY, payload TEXT);", &[])
            .expect("create target");
        // Tabel kenari: bila SQL injeksi sempat dieksekusi (mis. DROP TABLE),
        // tabel/baris ini akan hilang atau berubah. Karena parameter selalu
        // diikat sebagai data, ia tetap utuh.
        conn.exec(
            "CREATE TABLE canary (id INTEGER PRIMARY KEY, secret TEXT NOT NULL);",
            &[],
        )
        .expect("create canary");
        conn.exec(
            "INSERT INTO canary (id, secret) VALUES (?, ?);",
            &[DbValue::Int(1), DbValue::Str("do-not-touch".to_string())],
        )
        .expect("seed canary");

        let gen = adversarial_string_gen();
        pbt::for_all("prop_parameter_binding_resists_injection", &gen, |s: &String| {
            if conn.exec("DELETE FROM target;", &[]).is_err() {
                return false;
            }
            // Payload diikat via placeholder — selalu data, tak pernah teks SQL.
            if conn
                .exec(
                    "INSERT INTO target (id, payload) VALUES (?, ?);",
                    &[DbValue::Int(1), DbValue::Str(s.clone())],
                )
                .is_err()
            {
                return false;
            }
            // Oracle bagian 1: nilai terbaca == nilai tertulis, persis.
            let rows = match conn.query("SELECT payload FROM target WHERE id = ?;", &[DbValue::Int(1)]) {
                Ok(r) => r,
                Err(_) => return false,
            };
            if rows.len() != 1 || rows[0][0].1 != DbValue::Str(s.clone()) {
                return false;
            }
            // Tepat satu baris (tak ada tulisan tambahan dari SQL terinjeksi).
            let cnt = match conn.query("SELECT COUNT(*) FROM target;", &[]) {
                Ok(r) => r,
                Err(_) => return false,
            };
            if cnt[0][0].1 != DbValue::Int(1) {
                return false;
            }
            // Oracle bagian 2: skema/tabel lain tak berubah — tabel kenari dan
            // barisnya masih ada dan bernilai sama.
            let canary = match conn.query("SELECT secret FROM canary WHERE id = ?;", &[DbValue::Int(1)]) {
                Ok(r) => r,
                Err(_) => return false,
            };
            canary.len() == 1 && canary[0][0].1 == DbValue::Str("do-not-touch".to_string())
        });

        drop(conn);
        cleanup(&path);
    }
}
