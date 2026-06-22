# Changelog

All notable changes to **Ran** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and the project follows the
deliberate version scheme described in [`docs/20-changelog.md`](docs/20-changelog.md)
(1.0.0 is reserved for the fully self-hosted release).

For the complete, historical, feature-by-feature log see
[`docs/20-changelog.md`](docs/20-changelog.md). This file summarizes releases and
the current in-progress work.

## [Unreleased]

Next (native track): the native HTTP **server** — blocked on a runtime request-context
mechanism (the interpreter injects `req_*`/`query_*`/`param_*`/`cookie_*` as runtime
globals that zero-arg handlers read by name; native codegen needs an equivalent
context store before handlers can compile), then `concurrency` (`spawn`/channels via
pthreads). `crypto` stays a thin FFI bridge to OpenSSL (no hand-rolled crypto).
Self-hosting track: `bootstrap/codegen.ran` → the `ranc` CLI → the Stage A→D bootstrap
fixed point that defines 1.0.0. Plus `--link-static` and making native the default.

## [0.3.5] — Native HTTP client (http + https/TLS)

Backward-compatible. `ran build --native` can now build HTTP **client** programs: the
`http` module's `fetch`/`post_to`/`request` are bridged into the C runtime, with
`http://` over libc sockets and `https://` over the system OpenSSL (certificate +
hostname verification against the default trust store) — the same transports the
interpreter uses. Programs that never import `http` link nothing extra.

### Added — native `http` client

- `http.fetch(url)`, `http.post_to(url, body)`, `http.request(method, url, body)` lower
  to `ran_mod_http_*` calls returning the response **Map** `{status:int, body:str,
  ok:bool, error:str}` in the same insertion order as the interpreter's
  `http_client_call`. `ok` is `200..300`; a malformed URL or transport failure yields
  `status:0` with a populated `error`.
- `https://` uses OpenSSL (`-lssl -lcrypto`, linked only when `http` is imported) with
  SNI, peer-certificate verification, and hostname checking; `http://` uses a
  bounded-timeout socket connect. Response read is capped at 64 MB, matching the
  interpreter.
- Verified: byte-for-byte parity with the interpreter for GET/POST/request and the
  invalid-URL error against a local server, and a real `https://` fetch (identical
  status + body); ASan + UBSan + LSan clean. Network/OS error *text* is
  environment-dependent (shape-matched, like `time`/`rand`).
- **Still `E0606` natively:** the http **server** side (`http.get`/`post`/`server`/
  `listen`/`set_header`/`set_status`/`set_cookie`/`redirect`) and `web.serve`.

## [0.3.4] — Native string memory safety (no leaks, refcount-clean)

Backward-compatible; native output is byte-for-byte unchanged. The native AOT path
previously handled unboxed `const char*` strings (concat, `$`-interpolation, value
formatting, and the `str`/`json`/`os`/`fs` stdlib results) without ever freeing them —
a leak that grew without bound in long-running programs (e.g. a server's per-request
work). This release makes native string handling fully memory-safe, which is the
prerequisite for the upcoming native HTTP server.

### Fixed — heap-string lifetime in native binaries

- **Per-thread autorelease pool.** Every freshly allocated heap string registers in a
  `_Thread_local` pool; generated code drains the pool at each statement boundary, so
  transient strings are reclaimed immediately. A long loop now holds steady at a few
  MB instead of leaking one (or several) allocations per iteration.
- **Owned variable strings.** A string bound to a `let`/assignment, or returned from a
  function, is copied to an owned, non-pooled buffer (`ran_str_dup`) and freed at scope
  / function exit, on reassignment, and on `break`/`continue`/`return`. String literals
  and borrowed reads are never pooled or freed. Mirrors the interpreter's clone-on-bind
  semantics and the existing `RanValue` refcount discipline.
- **Runtime leak fixes.** `log.*`, `os.hostname`, `os.args`, and `fs.read` freed their
  internal string builders; `json.encode` of decimals and the string-valued
  interpolation path (`ran_interp_path`) no longer leak (the latter also fixes a latent
  use-after-free that returned a pointer into a just-released value).
- Verified: byte-for-byte parity with the interpreter across strings, interpolation,
  arrays, structs, maps, JSON, decimal money, and SQLite `db`; ASan + UBSan + LSan clean
  on each; peak RSS flat across a 3,000,000-iteration string loop; all 407 tests green.

## [0.3.3] — Self-hosting: a Ran semantic checker written in Ran

Milestone release: **the Ran compiler's semantic checker is now written in Ran.**
Joining the Ran-written lexer (`bootstrap/lexer.ran`) and parser
(`bootstrap/parser.ran`), a new **semantic checker in Ran** (`bootstrap/checker.ran`)
walks the parsed AST and reports the core diagnostics — so `ranc` now spans
lexer + parser + checker, all written in Ran and running today on the `ran` binary
(interpreter, VM, and under `--ownership=strict`). Backward compatible.

### Added — `bootstrap/checker.ran` (the semantic checker, in Ran)

- Pure Ran (no `std::` imports), self-contained (bundles its own lex + parse), one
  `main`. Walks the AST and reports the core semantic diagnostics with `E####` codes:
  - `E0001` — use of an undefined variable
  - `E0002` — call to an undefined function
  - `E0003` — wrong number of arguments to a call
  - `E0008` — duplicate function definition (e.g. two `main`s)
  - `E0100` — assignment to an immutable (`let`, not `let mut`) binding
- Scope tracking (function parameters + `let`/`let mut`), a function signature table
  (arity + mutability), and located diagnostics (no host crash on bad input). Runs
  identically on the VM and the interpreter and passes `--ownership=strict`.

## [0.3.2] — Short-circuit `&&` / `||`

Backward-compatible. `&&` and `||` now **short-circuit** — the right operand is
evaluated only when needed — consistently across all three engines (interpreter, VM,
and native). Result values are unchanged; only the conditional evaluation is new. This
removes a long-standing footgun (e.g. `i < n && arr[i] != x` no longer reads `arr[n]`
at the boundary) and the bootstrap compiler sources no longer need to work around it.
Verified: 361 unit + 46 integration/golden tests green.

- **Interpreter:** `&&`/`||` evaluate the left operand, then the right only if the
  result is not already determined (using the existing truthiness rule).
- **Native (AOT):** fixed a real bug — value-typed right operands (e.g. `a[i]`) were
  evaluated before the C `&&`; right-operand prep is now guarded, with correct refcount
  release. Native now short-circuits for all operand types.
- **VM:** `&&`/`||` route to the (now-correct) interpreter; the prior VM jump-based
  short-circuit was buggy (it dropped the operand) and is disabled pending a proper
  peek-jump opcode.
- Docs updated across the board (roadmap, control-flow, why-ran, syntax reference,
  language spec) to state `&&`/`||` short-circuit.

## [0.3.1] — Self-hosting begins: a Ran parser written in Ran

Milestone release: **part of the Ran compiler is now written in Ran.** Alongside the
existing Ran-written lexer, a recursive-descent **parser written in Ran**
(`bootstrap/parser.ran`) turns tokens into an AST and runs today on the `ran` binary
(interpreter, VM, and under `--ownership=strict`). This is the headline step toward
self-hosting (the Rust implementation compiling Ran → Ran compiling Ran). Backward
compatible; verified 395 tests green plus the bootstrap components running clean.

### Added — `bootstrap/parser.ran` (the parser, in Ran)

- Pure Ran (no `std::` imports), self-contained (bundles its own `lex`), one `main`.
- Recursive-descent parser for a real subset: function declarations, `let`/`let mut`,
  `return`, `if`/`else`, `while`, `echo`, assignment, expression statements; full
  expression **precedence** (`||`, `&&`, comparisons, `+ -`, `* / %`, unary `- !`),
  calls, and parenthesized expressions. AST nodes are tagged maps; parse errors become
  located `Error` nodes (no host crash). Runs identically on the VM and the interpreter
  and passes `--ownership=strict`.

### Fixed

- `bootstrap/lexer.ran` could crash at end-of-input (`E1012`) because Ran's `&&` is not
  short-circuit; its scan loops are now EOF-safe (using `break`). Both Ran compiler
  components run clean on the current binary.

## [0.2.5] — Native SQLite (`db`) (D4b-3a)

Backward-compatible. The native AOT path can now build database programs:
`ran build --native` bridges the `db` (embedded SQLite) module via direct
`libsqlite3` FFI in the C runtime — the same system library the interpreter uses.
Verified: 395 tests green; default `ran build` unchanged.

### Added — native `db` (SQLite)

- `db.connect/close/query/exec/begin/commit/rollback` compile to native, matching
  the interpreter's API and semantics: parameterized queries, rows as maps, base
  type mapping, and **exact decimal money stored as TEXT**. Error codes are at
  parity (`E0501`–`E0510`, including handleable `E0505` constraint with
  auto-rollback inside a transaction).
- The native binary links `-lsqlite3` only when the program imports `db`
  (`#ifdef`-gated runtime). Golden connect→exec→query→commit/rollback flow is
  byte-for-byte identical to the interpreter; ASan/UBSan clean.

### Still a hard `E0606` (deferred)

- `http`, `web`, `concurrency`, `crypto`, `decimal` module-form, `os.meminfo`.
  Never a silent fallback.

## [0.2.4] — Native map type, JSON decode & env (D4b-1)

Backward-compatible. The native AOT runtime gained a reference-counted **map/dict
type**, which unlocks `json.decode`/`parse`/`get` and the `env` module in native
builds — the foundational data layer the rest of the stdlib bridge builds on.
Verified: 392 tests green; default `ran build` unchanged.

### Added — native map/dict (`RanValue` RAN_MAP)

- Reference-counted, string-keyed map payload (no leak / double-free, verified with
  ASan/UBSan). Native lowering for `map()`, `m["key"]`, `set`/`get`/`keys`/`values`,
  and `len` on maps. Per-key access is byte-for-byte identical to the interpreter;
  whole-map *display order* is insertion order (the interpreter's is a hash order) —
  a documented, value-preserving divergence.

### Added — native `json` decode + `env`

- `json.decode`/`json.parse`/`json.get("a.b.0")`/`json.valid` — a byte-faithful port
  of the interpreter's JSON parser (objects→map, arrays→array, numbers, bool, string
  with `\uXXXX` + surrogate pairs, null).
- The `env` module: `get/get_or/require/int/float/bool/decimal/has/set/unset/all`
  plus dotenv `load/load_override/load_default`, matching the interpreter
  (`env.require` missing → `E1005`; `env.all` returns a map).

### Still a hard `E0606` (deferred)

- `http`, `db`, `web`, `concurrency`, `crypto`, `decimal` module-form; `os.meminfo`
  (needs a native sysinfo probe). Never a silent fallback.

## [0.2.3] — Native string interpolation + stdlib bridge (D3/D4a)

Backward-compatible. The native AOT path (`ran build --native`) gained general
string interpolation and a standard-library bridge, so real programs — with
`import`s and module calls — now compile to native machine code. Verified: 391
tests green; the default `ran build` is unchanged.

### Added — native string interpolation (D3)

- Interpolated string literals (`"x = $x"`, `"${order.total}"`, dotted paths like
  `"$acc.owner"`) now work **anywhere** in native code (let bindings, returns,
  arguments, concatenation), not just inside `echo` — byte-for-byte identical to
  the interpreter, including the "unknown `$name` left literal" rule.

### Added — native stdlib bridge (D4a)

- `import "std::<m>" as <m>` and module method calls now compile to native for the
  common modules, implemented in the C runtime (`libran_rt`, libc/libm only — the
  Rust runtime is not linked): **`time`, `log`, `math`, `str`, `os`, `fs`, `rand`,
  `json`** (encode/stringify/pretty). Variadic `log.*` matches the interpreter's
  line format.
- Deterministic functions (`math.*`, `str.*`, `os.platform/arch`, `fs.*`,
  `json.encode/stringify/pretty`) are byte-for-byte identical to the interpreter;
  nondeterministic ones (`time.*`, `rand.*`, `log` timestamps, `os.getpid/hostname`)
  match format/shape/type (documented divergences in `ran_rt.c`).
- A real program (e.g. a big integer-sum loop with `time.now_ms()` deltas and
  `log.info(...)`) now builds with `ran build --native` and runs the hot loop as
  native `int64` code.

### Still a hard `E0606` (deferred to D4b)

- Modules: `http`, `db`, `web`, `concurrency`, `crypto`, `env`, `html`, and the
  `decimal` module-form. Within bridged modules: `json.decode/parse/get/valid`,
  `os.meminfo` (need the native map type). Never a silent fallback.

## [0.2.2] — Memory-safe runtime, VM engine & native AOT codegen

A large, **backward-compatible** release. The runtime is substantially
crash-hardened, core language features for writing a compiler landed, the
bytecode VM became the default engine, and — the headline — `ran build --native`
now produces **real native machine code** for a growing subset (no embedded
interpreter, no `.ran` source in the artifact). The default `ran build` is
unchanged, so existing workflows keep working. Verified: 382 tests green.

### Added — native AOT codegen (Phase D, iterations D1–D2)

- **`ran build --native` (alias `--aot`) emits real native ELF binaries.**
  Pipeline: lower the checked program to C → link a precompiled C runtime
  (`libran_rt`) → invoke the system `cc`. The artifact carries **no embedded
  interpreter and no `.ran` source** (verified: no `RANENCv3` trailer). Output is
  **byte-for-byte identical to the interpreter** and runs under `env -i`.
- **D1 subset:** functions + recursion, `if`/`while`/`for range`,
  `break`/`continue`, checked integer arithmetic (`E1010`/`E1011`), booleans +
  comparisons, strings + concatenation, and `echo` interpolation — with proven
  numeric values unboxed to native `int64`/`bool` for near-C speed.
- **D2 subset (data-type layer):** a tagged, reference-counted `RanValue` model
  (no leak / no double-free) backing **exact `decimal` money math** (native
  result identical to the interpreter, e.g. price×qty + tax), `float`, **arrays**
  with bounds-checked indexing (`E1012`) + `len`, **structs** (literal + field
  access), and **`match`**.
- **No fake native / no silent fallback:** any construct outside the native
  subset is a hard build error (`E0606`) with `file:line:col` + help — never a
  silent interpreter fallback, never a partial artifact (atomic temp→rename).
- New build diagnostics `E0601`–`E0606`; the system C compiler is documented as a
  build-time-only dependency-policy exception (no cargo crate added).

### Added — Phase A: memory safety & crash hardening

- **Recursion-depth guard (`E1007`).** Per-thread call-depth tracking raises a
  *catchable* fault before the OS stack overflows (deep recursion used to cause
  an uncatchable `SIGSEGV`). Execution runs on a 1 GiB-stack thread; configurable
  via `--max-depth=<N>` (default 10000).
- **Checked integer arithmetic** (`E1010` overflow, `E1011` divide/modulo by
  zero) — no more silent wrap; **bounds-safe indexing** (`E1012`);
  **poisoned-mutex recovery** (`E0511`); `assert` failure is now recoverable
  (`E1013`).
- **No `process::exit` in library code** (audited; enforced by a property test).
  A faulting `spawn`ed thread delivers an inspectable error value to its joiner;
  a faulting HTTP handler returns `500` and the server keeps serving (no internal
  leak). Memory watchdog + loop guard (`E1006`) formalized across all paths;
  bounded VM faults (`E1008`/`E1009`).

### Added — Phase B: language core

- **Closures** (`fn(x) { ... }` capturing scope), **`break`/`continue`**,
  **`return` from a `match` arm**, and **traits** (`trait` + `impl Trait for
  Type`, default bodies, receiver-type dispatch). The ownership/borrow checker
  accepts all of them under `--ownership=strict` with no false positives.

### Changed — Phase C: bytecode VM is the default engine

- The `backend/vm/` bytecode VM is now the **default execution engine**, with
  type-specialized opcodes, bounded execution (`E1008`/`E1009`), and an
  automatic, safe fallback to the tree-walking interpreter for unsupported
  constructs (never runs a program incorrectly).

### Fixed

- Stale-incremental artifacts after a project relocation; two flaky depth tests
  racing on the process-global call-depth limit; integer overflow / divide tests
  updated to the new `E1010`/`E1011` codes.

### Compatibility

- Fully backward compatible: `--native` is additive and the default `ran build`,
  `ran run`, and all language semantics are unchanged. New diagnostics only fire
  on conditions that previously crashed or silently misbehaved.

## [0.2.1] — bytecode target, faster runtime, build dumps

- Indexed call-stack frames + shared globals + `Arc` function bodies; lazy
  `for x in range(n)`; runtime memory guard (`E1006`).
- Bytecode VM target as a build artifact (disassembly dump); microkernel-style
  `runtime/` split. See [`docs/20-changelog.md`](docs/20-changelog.md) for detail.
