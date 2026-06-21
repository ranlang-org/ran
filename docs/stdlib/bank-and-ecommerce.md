# Worked Examples: Banking & E-commerce

Two runnable examples show enterprise money handling with today's features
(`struct` + `impl` + `decimal` + `json`). No database is required — the money
logic is identical whether prices come from a map or, later, from `db`.

Run them:

```fish
ran examples/banking.ran
ran examples/ecommerce.ran
```

## Banking: exact, safe transfers

`examples/banking.ran` models accounts as structs and transfers funds with:

- **Exact balances** via `decimal` (no float drift).
- **Overdraft protection**: a transfer is rejected if the source can't cover it.
- **Value semantics**: balances are recomputed and only the successful result is
  used — a failed transfer leaves both accounts untouched.

```
Transfer 250.75 Alice -> Bob:
  Alice (ACC-1): 749.25
  Bob (ACC-2): 300.75
Overdraft attempt 10000 Bob -> Alice:
ERROR ... transfer rejected: insufficient funds for Bob
```

When the `db` module lands, wrap the two legs in `db.begin/commit/rollback` so
the debit and credit are a single atomic transaction (see
[database.md](database.md)).

## E-commerce: price from "DB" + tax %

`examples/ecommerce.ran` computes a cart total where each price is fetched by
SKU (simulating a database lookup), applies 11% tax with explicit half-up
rounding, and emits a JSON API response with **exact decimal numbers**:

```
subtotal: 233.97
tax(11%): 25.74
total:    259.71
JSON: {"total":259.71,"subtotal":233.97,"tax":25.74}
```

Key points for real systems:

- Prices and balances are `decimal`, never `float`.
- Tax is rounded once, explicitly: `decimal.round(subtotal * rate, 2, "half_up")`.
- `json.encode` emits decimals as exact JSON numbers, safe for downstream
  consumers.

## Migrating to a real database

The only thing that changes is persistence:

```ran
# today (in-memory / map)
fn price_of(sku) -> decimal { if sku == "BOOK" { return dec("19.99") } ... }

# later (db module)
fn price_of(conn, sku) -> decimal {
    let rows = db.query(conn, "SELECT price FROM products WHERE sku = $1", [sku])
    return rows[0]["price"]    # NUMERIC -> decimal, exact
}
```

The tax and total math stays byte-for-byte the same.
