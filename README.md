# Ran

**A self-hosted programming language for internal systems, business tooling, and
services that handle real money.**

> **Internal / personal use.** Ran is built and maintained for private, in-house
> use. It is not a general-purpose public product, makes no stability promises to
> outside users, and is shaped entirely around the needs of the projects that use
> it. Use it inside your own organization at your own discretion.

Ran runs `.ran` files with a tree-walking interpreter. `ran build` packages a
program into a single standalone native binary with the source embedded. No
external runtime and no separate compiler toolchain are required to build.

```fish
# Run directly
ran hello.ran

# Compile to a standalone binary
ran build hello.ran -o hello
./hello
```

> Status: v0.3.10, under active development. This README and `docs/` describe what
> works today; partial or planned features are labelled. See `docs/16-roadmap.md`.

---

## Why it exists

- **Exact money math.** A first-class `decimal` type with explicit rounding —
  `0.1 + 0.2` is exactly `0.3`. Never use floats for currency.
- **Strict, friendly diagnostics.** Undefined names, arity, type, and syntax
  errors are caught before the program runs, each with `file:line:col` and a fix
  hint.
- **Batteries included.** HTTP server and client (with TLS), JSON, filesystem,
  time, math, strings, logging, environment/config — all built in.
- **Single-binary delivery.** `ran build` emits one self-contained executable.

---

## Quick tour

```ran
import "std::decimal" as decimal
import "std::log" as log

struct Account { owner: str, balance: decimal }

impl Account {
    fn open(owner) -> Account {
        return Account { owner: owner, balance: dec("0.00") }
    }
    fn deposit(self, amount) -> Account {
        return Account { owner: self.owner, balance: self.balance + amount }
    }
}

fn main() {
    log.info("starting")
    let a = Account.open("Risqi").deposit(dec("19.99"))
    echo "$a.owner has $a.balance"
}
```

## Commands

| Command | Description |
|---------|-------------|
| `ran <file.ran>` | Run a file directly |
| `ran run [file]` | Run the project (auto-detects the entry) |
| `ran build [file] -o <out>` | Compile to a standalone binary |
| `ran test [file]` | Run all `test_*` functions |
| `ran init [name]` | Scaffold a new project (`src/` layout) |
| `ran repl` | Interactive REPL |
| `ran version` / `ran help` | Version / usage |

Entry resolution: explicit file → `ran.toml` `entry` → `src/main.ran` /
`main.ran` → the first `.ran` file defining `fn main(`. `ran.toml` is optional.

## Standard library

Imported with a mandatory `std::` prefix and alias:

```ran
import "std::http" as http
import "std::json" as json
```

Modules: `http` (server + TLS client), `web` (native HTML/CSS/asset serving),
`db` (SQLite), `concurrency` (threads, channels, wait groups, shared state),
`decimal`, `crypto`, `env`, `json`, `fs`, `os`, `time`, `math`, `str`, `rand`,
`log`, `html`. Full reference in [`docs/stdlib/`](docs/stdlib/README.md).

Local modules import by path (no prefix), including parent paths:

```ran
import "./lib/users" as users
import "../shared/money.ran" as money
```

## Install

```fish
git clone https://github.com/ranlang-org/ran
cd ran
cargo build --release
cp target/release/ran ~/.local/bin/
```

The release build links the system TLS library; ensure `libssl`/`libcrypto`
development files are present. Once built, the `ran` binary is self-contained.

## Compilation & source protection

`ran build` strips the runtime, compresses and obfuscates the source, and embeds
it. **This is obfuscation, not encryption** — the key ships in the toolchain, so
it resists casual inspection only. Never hardcode secrets; read them at runtime
with the `env` module. See `docs/14-security.md`.

## Security posture

- HTTP **client** verifies certificates and hostnames (TLS via system OpenSSL).
- HTTP **server** is unauthenticated and has no inbound TLS yet — put auth and
  TLS termination in front of it. Request line/headers/body are size-capped.
- `rand` is not a CSPRNG; `html.render` does not auto-escape.
- See `docs/21-dependency-policy.md` for the zero-third-party-crate policy.

## Project structure

```
ran/
├── src/
│   ├── main.rs        # CLI entry point
│   ├── frontend/      # lexer, parser, AST
│   ├── semantics/     # analyzer + type system
│   ├── backend/       # codegen; vm/ exists but is NOT wired in
│   ├── runtime/       # tree-walking interpreter + stdlib dispatch
│   ├── stdlib/        # net (HTTP/web server + client), concurrency, db (SQLite), hardware
│   └── support/       # diagnostics, crypto, decimal, tls, module loader
├── examples/          # example programs
├── docs/              # documentation
└── tests/             # end-to-end tests
```

## Status

Working: variables, functions + recursion, control flow, **`return` through
loops**, **`break`/`continue`**, **`match`-arm `return`**, **closures/lambdas**,
checked int math (overflow/div-by-zero are faults, never silent), **exact `decimal`
math**, **bounds-safe indexing**, strings/arrays/maps, **structs + methods +
constructors**, **`enum` + `match`**, **traits + `impl Trait for Type`**, the HTTP
server, **HTTPS client (TLS)**, **native web serving** (`web` module: HTML/CSS/assets,
SPA fallback, cache validators, build hook), the **`db` module (SQLite)** with
parameterized queries and transactions, **real concurrency** (`spawn` + join with
fault delivery, channels, wait groups, synchronized shared state), **enforced
ownership/borrow checking** (`--ownership=warn|strict`, `&mut` write-back), the full
stdlib above, local + parent imports, `std::` stdlib imports, `ran test`,
**resource-aware builds** (`--mem-limit`), and auto-managed `ran.toml`.

**Crash-hardened runtime:** a recursion-depth guard (`E1007`, `--max-depth=<N>`, runs
on a large dedicated stack so it fires before any OS stack overflow), checked
arithmetic (`E1010`/`E1011`), bounds-safe indexing (`E1012`), poisoned-mutex recovery
(`E0511`), a memory watchdog + in-loop guard (`E1006`), and recoverable runtime faults
that unwind to a catch boundary (a faulting `spawn`ed thread surfaces an error value;
a faulting HTTP handler returns 500 and the server stays up) — no library code calls
`process::exit`.

**Execution engine:** the register/stack **bytecode VM** (`backend/vm/`) is the
default engine (type-specialized opcodes, bounded execution `E1008`/`E1009`), with an
automatic, safe fallback to the tree-walking interpreter for constructs it does not yet
support.

Partial / planned: **native AOT machine-code generation** (`ran build` still embeds the
interpreter today — a true native backend is designed; see `docs/16-roadmap.md`),
deref-assignment (`*p = ...`), inbound server TLS, a CSPRNG and
password-hashing KDF, a package manager, and the Ran-in-Ran compiler + bootstrap. See
`docs/16-roadmap.md`.

## License

MIT — for internal use.
