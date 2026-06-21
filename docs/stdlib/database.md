# Database — `db` (SQLite)

The `db` module is an embedded SQL database backed by SQLite. It gives Ran a
durable, transactional store with parameterized queries and exact `decimal`
money mapping — no separate database server to run.

```ran
import "std::db" as db
```

> **System library.** `db` links the system SQLite library (`libsqlite3`) through
> FFI, the same approach the HTTPS client uses for TLS. The project ships zero
> Cargo crates; see [../21-dependency-policy.md](../21-dependency-policy.md). If
> `libsqlite3` is unavailable at runtime, `db.connect` returns a handleable error
> value (code `E0501`) instead of crashing, so programs can degrade gracefully.

## Quick start

```ran
import "std::db" as db

fn main() {
    let conn = db.connect("app.sqlite")     # opens or creates the file

    db.exec(conn, "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, name TEXT, qty INTEGER)", [])

    # Parameters are bound, never interpolated — safe against injection.
    db.exec(conn, "INSERT INTO items (name, qty) VALUES (?, ?)", ["apple", 3])
    db.exec(conn, "INSERT INTO items (name, qty) VALUES (?, ?)", ["banana", 7])

    let rows = db.query(conn, "SELECT name, qty FROM items WHERE qty > ? ORDER BY id", [5])
    for row in rows {
        let name = row["name"]
        let qty = row["qty"]
        echo "$name: $qty"
    }

    db.close(conn)
}
```

## Functions

| Call | Description |
|------|-------------|
| `db.connect(path)` | Open or create the SQLite file; returns an integer handle. On failure returns a handleable error value. |
| `db.query(conn, sql, params)` | Run a `SELECT`; returns an array of row maps (column → value). Zero rows returns an empty array. |
| `db.exec(conn, sql, params)` | Run `INSERT`/`UPDATE`/`DELETE`/DDL; returns the affected-row count (≥ 0). |
| `db.begin(conn)` | Begin a transaction. |
| `db.commit(conn)` | Commit the active transaction. |
| `db.rollback(conn)` | Roll back the active transaction. |
| `db.close(conn)` | Close the connection and release its resources. |

`params` is always an array. Use `?` placeholders in the SQL; each `?` consumes
one element of `params` in order. Values are bound by the driver and never
interpolated into the SQL string.

## Type mapping

Values read from a result set map to Ran types as follows:

| SQLite | Ran |
|--------|-----|
| `INTEGER` | `int` |
| `REAL` | `float` |
| `TEXT` | `str` (including the empty string) |
| `NULL` | `void` |

`BLOB` and other unsupported column types raise `E0507`.

### Money columns → `decimal`

Store money as exact decimal text and read it back with the `decimal` module so
no precision is lost:

```ran
import "std::db" as db
import "std::decimal" as decimal

fn main() {
    let conn = db.connect("ledger.sqlite")
    db.exec(conn, "CREATE TABLE IF NOT EXISTS accounts (id INTEGER PRIMARY KEY, balance TEXT)", [])

    # Write a decimal as exact text.
    db.exec(conn, "INSERT INTO accounts (balance) VALUES (?)", [dec("1250.75")])

    # Read the column and parse it back to an exact decimal.
    let rows = db.query(conn, "SELECT balance FROM accounts WHERE id = ?", [1])
    let balance = decimal.parse(rows[0]["balance"])
    echo "balance = $balance"

    db.close(conn)
}
```

Keep money columns as `TEXT` holding exact decimal strings — never `REAL` — so
values round-trip without floating-point error. A value that cannot be parsed as
a decimal raises `E0508`.

## Transactions

Wrap related writes in a transaction so they commit together or not at all:

```ran
fn transfer(conn, from_id, to_id, amount) {
    db.begin(conn)
    db.exec(conn, "UPDATE accounts SET balance = ? WHERE id = ?", [debited, from_id])
    db.exec(conn, "UPDATE accounts SET balance = ? WHERE id = ?", [credited, to_id])
    db.commit(conn)
}
```

If a statement violates a constraint, the transaction is rolled back
automatically (`E0505`) to the state before the failing command, so no partial
write survives. The zero-sum invariant — total balance before equals total
balance after a balanced transfer — holds because the money math is exact and
the writes are atomic.

## Diagnostics

| Code  | Meaning |
|-------|---------|
| E0501 | cannot open the database (library unavailable, permission denied) |
| E0502 | the file is not a valid SQLite database |
| E0503 | invalid or already-closed connection handle |
| E0504 | invalid SQL |
| E0505 | constraint violation — transaction rolled back |
| E0506 | bound-parameter count does not match the `?` placeholders |
| E0507 | unsupported column type (e.g. `BLOB`) |
| E0508 | money column could not be parsed as an exact decimal |
| E0509 | `begin` called while a transaction is already active |
| E0510 | `commit`/`rollback` called with no active transaction |

Errors are returned as handleable values (a map with `error: true` and a `code`),
so a program can inspect and recover rather than crash.

## Looking ahead

The embedded SQLite backend covers single-node, file-based storage. A networked
database client (server-based SQL over the wire, connection pooling) remains a
roadmap item; see [../16-roadmap.md](../16-roadmap.md). For pure business-logic
modelling without any database, `decimal` plus `fs`/`json` still works — see
[bank-and-ecommerce.md](bank-and-ecommerce.md).
