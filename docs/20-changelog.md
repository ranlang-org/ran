# Changelog

## 0.3.4 — Native string memory safety (no leaks, refcount-clean)

Backward-compatible; native output is byte-for-byte unchanged. The native AOT path used
to allocate unboxed `const char*` strings (concat, `$`-interpolation, value formatting,
and `str`/`json`/`os`/`fs` results) and never free them — an unbounded leak in
long-running programs. Native string handling is now fully memory-safe, the prerequisite
for the native HTTP server. Summary in the root [`CHANGELOG.md`](../CHANGELOG.md).

- **Per-thread autorelease pool:** every fresh heap string registers in a
  `_Thread_local` pool drained at each statement boundary; transient strings are
  reclaimed at once (a long loop stays at a few MB instead of leaking per iteration).
- **Owned variable strings:** strings bound to a variable or returned from a function
  are copied to an owned, non-pooled buffer (`ran_str_dup`) and freed at scope/function
  exit, on reassignment, and on `break`/`continue`/`return`; literals and borrows are
  never pooled or freed (mirrors interpreter clone-on-bind + the `RanValue` discipline).
- **Runtime leak fixes:** `log.*`, `os.hostname`, `os.args`, `fs.read` free their string
  builders; `json.encode` of decimals and the interpolation path no longer leak (the
  latter also fixes a latent use-after-free).
- Verified: byte-for-byte parity (strings, interpolation, arrays, structs, maps, JSON,
  decimal, SQLite `db`); ASan + UBSan + LSan clean; flat RSS across a 3,000,000-iteration
  string loop; 407 tests green.

## 0.3.3 — Self-hosting: a Ran semantic checker written in Ran

Milestone: **the Ran compiler's semantic checker is now written in Ran.** Joining the
Ran-written lexer (`bootstrap/lexer.ran`) and parser (`bootstrap/parser.ran`), a new
**semantic checker in Ran** (`bootstrap/checker.ran`) walks the parsed AST and reports
the core diagnostics — so `ranc` now spans lexer + parser + checker, all written in Ran
and running today on the `ran` binary (interpreter, VM, and `--ownership=strict`).
Backward compatible. Summary in the root [`CHANGELOG.md`](../CHANGELOG.md).

- **`bootstrap/checker.ran`:** pure Ran, self-contained (bundles its own lex + parse),
  one `main`. Walks the AST and reports the core semantic diagnostics with `E####`
  codes: `E0001` (undefined variable), `E0002` (undefined function), `E0003` (wrong
  argument count), `E0008` (duplicate function — e.g. two `main`s), and `E0100`
  (assignment to an immutable `let` binding). Tracks scopes (params + `let`/`let mut`)
  and a function signature table (arity + mutability); parse/check errors are located
  (no host crash). Runs identically on VM + interpreter; passes `--ownership=strict`.

## 0.3.2 — Short-circuit `&&` / `||`

Backward-compatible. `&&` and `||` now **short-circuit** (right operand evaluated only
when needed) consistently across the interpreter, VM, and native engines. Result values
unchanged; only conditional evaluation is new — removes a footgun (`i < n && arr[i]`
no longer reads `arr[n]` at the boundary). Verified: 361 unit + 46 integration green.
Summary in the root [`CHANGELOG.md`](../CHANGELOG.md).

## 0.3.1 — Self-hosting begins: a Ran parser written in Ran

Milestone: **part of the Ran compiler is now written in Ran.** Alongside the existing
Ran-written lexer, a recursive-descent **parser written in Ran**
(`bootstrap/parser.ran`) turns tokens into an AST and runs today on the `ran` binary
(VM, interpreter, and `--ownership=strict`). Headline step toward self-hosting.
Backward compatible. Summary in the root [`CHANGELOG.md`](../CHANGELOG.md).

- **`bootstrap/parser.ran`:** pure Ran, self-contained, one `main`. Recursive-descent
  parser for a real subset (fn decls, `let`/`let mut`, `return`, `if`/`else`, `while`,
  `echo`, assignment, expr statements) with full expression precedence, calls, and
  parentheses. AST nodes are tagged maps; parse errors are located `Error` nodes (no
  crash). Runs on VM + interpreter identically; passes `--ownership=strict`.
- **Fixed `bootstrap/lexer.ran`:** EOF-safe scan loops (Ran's `&&` is not short-circuit,
  which could trip `E1012` at end of input). Both compiler-in-Ran components now run
  clean on the current binary.

## 0.2.5 — Native SQLite (`db`) (D4b-3a)

Backward-compatible. `ran build --native` now bridges the `db` (embedded SQLite)
module via direct `libsqlite3` FFI in the C runtime — database programs build
native. Verified: 395 tests green; default `ran build` unchanged. Summary in the
root [`CHANGELOG.md`](../CHANGELOG.md).

- **Native `db`:** `connect/close/query/exec/begin/commit/rollback` matching the
  interpreter — parameterized queries, rows as maps, base type mapping, and exact
  decimal money as TEXT. Error parity `E0501`–`E0510` (handleable `E0505` constraint
  with in-transaction auto-rollback). Links `-lsqlite3` only when `db` is imported.
  Golden connect→exec→query→commit/rollback flow byte-for-byte equal to the
  interpreter; ASan/UBSan clean.
- **Still `E0606`:** `http`, `web`, `concurrency`, `crypto`, `decimal` module-form,
  `os.meminfo`.

## 0.2.4 — Native map type, JSON decode & env (D4b-1)

Backward-compatible. The native AOT runtime gained a reference-counted **map/dict
type**, unlocking `json.decode`/`parse`/`get` and the `env` module in `ran build
--native`. Verified: 392 tests green; default `ran build` unchanged. Summary in the
root [`CHANGELOG.md`](../CHANGELOG.md).

- **Native map/dict (`RAN_MAP`):** refcounted, string-keyed (ASan/UBSan clean).
  Lowering for `map()`, `m["key"]`, `set`/`get`/`keys`/`values`, `len`. Per-key
  access is byte-for-byte equal to the interpreter; whole-map display order is
  insertion order (documented value-preserving divergence).
- **Native `json` decode:** `json.decode`/`parse`/`get("a.b.0")`/`valid` — a faithful
  port of the interpreter's parser (objects→map, arrays→array, numbers/bool/string
  with `\uXXXX`+surrogates/null).
- **Native `env`:** `get/get_or/require/int/float/bool/decimal/has/set/unset/all` +
  dotenv `load/load_override/load_default` (`env.require` missing → `E1005`).
- **Still `E0606`:** `http`, `db`, `web`, `concurrency`, `crypto`, `decimal`
  module-form, `os.meminfo`. Never a silent fallback.

## 0.2.3 — Native string interpolation + stdlib bridge (D3 / D4a)

Backward-compatible. `ran build --native` gained **general string interpolation**
and a **standard-library bridge**, so real programs (with `import`s and module
calls) now compile to native machine code. The default `ran build` is unchanged.
Verified: 391 tests green. Summary also in the root [`CHANGELOG.md`](../CHANGELOG.md).

- **Native string interpolation (D3):** interpolated string literals (`"x=$x"`,
  `"${total}"`, dotted `"$acc.owner"`) work anywhere in native code, not just in
  `echo` — byte-for-byte identical to the interpreter (incl. unknown-name-left-literal).
- **Native stdlib bridge (D4a):** `import` + method calls compile to native for
  `time`, `log`, `math`, `str`, `os`, `fs`, `rand`, `json` (encode/stringify/pretty),
  implemented in the C runtime (`libran_rt`, libc/libm only). Deterministic functions
  are byte-for-byte equal to the interpreter; nondeterministic ones (time/rand/log
  timestamp/pid) match format/shape/type. Variadic `log.*` matches the interpreter's
  line format. A real program with a big int-sum loop + `time.now_ms()` + `log.info`
  now builds native and runs the hot loop as native `int64`.
- **Still `E0606` (D4b):** `http`, `db`, `web`, `concurrency`, `crypto`, `env`, `html`,
  `decimal` module-form; and `json.decode/parse/get`, `os.meminfo` (need a native map
  type). Never a silent fallback.

## 0.2.2 — Memory-safe self-hosting (Phases A–C) + native AOT codegen (D1–D2)

Tracked under the `memory-safe-self-hosting` effort. **Phases A, B, and C are
complete**, and **native AOT codegen iterations D1–D2 shipped** — `ran build
--native` now emits real native ELF binaries for a growing subset. Backward
compatible (native is additive; default `ran build` unchanged). Verified: 382
tests green. A condensed summary also lives in the root
[`CHANGELOG.md`](../CHANGELOG.md).

### Phase A — memory safety & crash hardening (first-class priority)

- **Recursion-depth guard (`E1007`).** Per-thread Ran call-depth tracking raises a
  *catchable* fault before the OS stack overflows. This fixes the most serious
  crash source: deep/unbounded recursion used to cause an uncatchable `SIGSEGV`.
  Execution runs on a dedicated **1 GiB-stack thread** so the guard fires first.
  New flag `--max-depth=<N>` (default 10000); invalid values warn and fall back to
  the default without aborting.
- **Checked integer arithmetic.** Overflow on `+ - * / %` raises `E1010` (no more
  silent wrap), integer divide/modulo by zero raises `E1011`. (Decimal keeps its
  own `E1002`/`E1003`.)
- **Bounds-safe indexing (`E1012`).** Out-of-range / negative array & string
  indices raise a fault carrying index + length; string indexing is char-boundary
  (Unicode-scalar) safe; no host panic.
- **Poisoned-mutex recovery (`E0511`).** New `lock_or_fault` helper replaces all
  risky `.lock().expect(...)` in `stdlib/db.rs` / `stdlib/concurrency.rs`; a
  panicked thread no longer cascades into a process crash.
- **No `process::exit` in library code.** Full audit; the library `assert` failure
  now raises `E1013` (recoverable). Exit is confined to whitelisted boundaries; a
  property test statically enforces the invariant.
- **Recoverable fault delivery.** A fault in a `spawn`ed thread is delivered to the
  joiner as an inspectable error value; a faulting HTTP handler returns `500` and
  the server keeps serving (no internal detail leaked to the client).
- **Memory watchdog & loop guard (`E1006`)** formalized across all execution paths
  and installed idempotently.
- **Bounded VM faults:** `E1008` (step budget), `E1009` (value-stack cap).
- `fault_to_value` surfaces faults to Ran code as `{ error, code, message }`.
- 16 property-based tests (recursion never SIGSEGVs, depth restored on
  fault-unwind, checked arithmetic never wraps, index bounds-safe, poisoned-mutex
  recovery, exit-free library audit, diagnostic consistency, build-budget
  invariants, bounded VM termination, VM fallback, etc.).

### Phase B — language core for writing a compiler

- **Closures / lambdas** (`fn(x) { ... }`) — first-class values that capture their
  defining scope; storable, passable, returnable.
- **`break` / `continue`** in loops, propagating through nested blocks.
- **`return` inside a `match` arm** now unwinds to the enclosing function.
- **Traits:** `trait` declarations (with optional default method bodies) and
  `impl Trait for Type`; dispatch by the receiver value's type.
- Ownership/borrow checker accepts all new constructs under `--ownership=strict`
  with no false positives (verified by a property test).

### Phase C — bytecode VM wired in as the engine

- The `backend/vm/` bytecode VM is now the **default execution engine**, with
  automatic, safe **fallback to the interpreter** for unsupported constructs (never
  runs a program incorrectly).
- **Type-specialized opcodes** (e.g. int add) use analyzer type info to skip
  generic dispatch on hot paths.
- Bounded execution + output buffering keep the VM safe and fallback output correct.
- A regression gate keeps the full suite green before the VM became the default.

### Phase D — native AOT codegen (D1–D2 shipped)

`ran build --native` (alias `--aot`) emits a real native ELF binary: lower the
checked program to C → link a precompiled C runtime (`libran_rt`) → invoke the
system `cc`. **No embedded interpreter, no `.ran` source** in the artifact;
output is **byte-for-byte identical to the interpreter** and runs under `env -i`.
Default `ran build` is unchanged (still embed-source) so nothing regresses.

- **D1** (scalar core): functions+recursion, `if`/`while`/`for range`,
  `break`/`continue`, checked int arithmetic (`E1010`/`E1011`), bool+comparisons,
  strings+concat, `echo` interpolation; proven numeric values unboxed to native
  `int64`/`bool`.
- **D2** (data-type layer): tagged, reference-counted `RanValue` (no leak /
  double-free) backing **exact `decimal` money math**, `float`, **arrays** +
  bounds-checked indexing (`E1012`) + `len`, **structs** (literal + field access),
  and **`match`**.
- No fake native / no silent fallback: out-of-subset constructs are a hard
  `E0606` build error; atomic temp→rename means no partial artifact. New build
  diagnostics `E0601`–`E0606`; the system C compiler is a documented build-time
  dependency-policy exception (no cargo crate added).
- **Remaining (D3+):** general string interpolation, closures, trait dispatch,
  `spawn`/channels, stdlib via `libran_rt`, `--link-static`, then making native
  the default. Phases E–G (compiler stdlib, Ran-in-Ran compiler, bootstrap fixed
  point) follow.

## 0.2.1 — bytecode target, faster runtime, build dumps

### Performance: indexed call-stack + Arc bodies
- New variable model (`runtime/frame.rs`): shared globals (no per-call clone) +
  indexed `Vec` local frames; FNV-hashed scopes. `Arc<Vec<Stmt>>` function
  bodies (no per-call AST deep-clone). Lazy `for x in range(n)` (constant memory
  — fixes OOM on huge loops). 1M-iter loop ~617→~275 ms; fib(30) ~5.1→~3.0 s.
- Runtime memory guard (`E1006`): loops abort cleanly before the OS OOM-killer.

### Bytecode VM target (experimental, build artifact)
- `ran build` now compiles the AST to bytecode and writes a disassembly to
  `target/<name>.bc.txt`. Execution still uses the interpreter; the bytecode is
  the stepping stone toward native code generation (see TODO bootstrap roadmap).

### Build UX: dumps, precise stages, timing
- `ran build` writes `target/<name>.{tokens,ast,check,bc}.txt` for debugging the
  language itself.
- Distinct, always-visible build stages (Lexing/Parsing/Resolving/Checking/
  Checked/Emitting/Finishing) with per-stage `[+t]` elapsed and a total time.
- Multi-file projects: `ran build` inlines local imports into the binary; built
  binaries verified to run under `env -i` (no `ran`, no PATH) and honour `--port`.

### Structure (microkernel-style split)
- `runtime/mod.rs` (~4.9k→~2.7k) split into `runtime/{json,module_dispatch,
  server,builtins,frame}.rs` + `helpers/{concurrency,db}.rs`.

## Unreleased (hardening pass)

### HTTP: sessions, cookies, response control + full web-app example (NEW)
- Handlers can now read cookies (`$cookie_<name>`) and control the response:
  `http.set_status`, `http.set_header`, `http.set_cookie`/`clear_cookie`,
  `http.redirect`.
- New example `examples/webapp_full/` — login + **stateless HMAC-signed
  sessions** + role-aware dashboard + per-user task CRUD, JSON-backed. Verified
  live: 401 when unauthenticated, 401 on bad login, cookie set on login, 200
  dashboard when authed, 302→/login when not, owner-scoped task CRUD.
- `docs/stdlib/http.md` updated (cookies + response-control table).

### Stdlib: `crypto` module (NEW)
- Exposed `std::crypto`: `sha256`/`sha256_hex`, `hmac_sha256` (RFC 2104),
  `hex`, `base64`/`base64_decode` — built on the in-tree FIPS 180-4 SHA-256
  (previously internal-only). Verified against RFC 4231 / known vectors.
- Documented honestly (`docs/stdlib/crypto.md`): not a password hasher, not a
  CSPRNG, not an encryption API — those remain out until done correctly.

### HTTP: full CRUD verbs + worked example (NEW)
- Added `http.put`, `http.patch`, `http.delete` route registration (GET/POST
  already existed), so REST CRUD is first-class.
- New runnable example [`examples/crud/`](../examples/crud/): a JSON-backed
  realtime notes board (list/create/update/delete) with a polling browser UI.
- New tutorial `docs/24-realtime-crud-tutorial.md` explaining routing, injected
  request variables, JSON persistence, and — explicitly — what
  `import "std::http" as http` (and the `as` alias) means.

### Language: lexical scoping (NEW)
- The interpreter now uses a proper **scope stack**. Block-locals (`if`/`while`/
  `for` bodies) and the `for` loop variable no longer leak out of their block.
- Functions see **globals + their parameters only** — never the caller's locals
  (true lexical scope, not dynamic). `main` runs in its own frame.
- Assignment mutates the variable in its defining scope (so loop accumulation
  like `total = total + x` still works), while a fresh `let` stays block-local.
- Verified: accumulation, non-leakage of loop/block/if locals, and function
  isolation (65 tests).

### Reliability: recoverable runtime faults (NEW)
- Runtime faults (`E1001`–`E1005`, decimal errors) now **unwind** to a catch
  boundary instead of calling `process::exit`. Release profile switched to
  `panic = "unwind"`.
- **HTTP server fault isolation**: a fault inside a request handler returns
  `500` and is logged; the server keeps serving. Verified live (a div-by-zero
  handler returns 500 while other routes stay up).
- Top-level scripts still print the diagnostic and exit `70`; a faulting
  `test_*` is reported as a failure, not a crash. `exit`/`os.exit`/`log.fatal`
  remain intentional terminations.
- `docs/stdlib` examples fixed to the mandatory `std::` import form; fault→500
  behavior documented in `errors.md` and `http.md`.

### Language: `enum` + `match` (NEW)
- `enum` declarations and `match` expressions/statements now execute. Patterns:
  literals, enum variants (`Status.Active`), a binding identifier, and `_`.
  Arm bodies are a single statement or a `{ }` block. New `=>` token; new syntax
  error `E0103` already covered struct fields.
- New `TODO.md`: single source of truth for progress toward self-hosting
  (milestones 0–6), with the critical items flagged (error model, lexical
  scoping, `runtime/` split).

### Concurrency, CPU & documentation audit
- HTTP server worker pool now uses a **bounded queue** (`sync_channel`) for
  backpressure and **named worker threads** for observability.
- New `docs/23-runtime-audit.md`: concurrency/CPU audit, the critical
  per-request error-isolation finding (a handler fault currently can exit the
  whole server — error-model refactor required), and the planned `runtime/`
  file split + scoping/enum-match/VM roadmap.
- Documentation audit: README, introduction, why-ran, and interop rewritten as
  standalone **internal-use** docs; comparative "like language X" framing and
  C++ references removed from the prominent pages.

### Imports & module system
- Standard-library imports now **require** the `std::` prefix:
  `import "std::http" as http` (bare `import "std::http"` → new error `E0006`;
  unknown `std::x` → `E0007`).
- Relative imports support **parent paths and explicit `.ran`**:
  `import "../shared/money.ran" as money`.
- All examples, the `ran init` template, and tests updated to `std::`.

### Security hardening
- HTTP server: request line, header lines, header count, and total header bytes
  are now capped (anti-DoS); body was already capped.
- HTTP client: response size capped at 64 MB.
- `ran build` invokes `strip --` so an output name starting with `-` can't be
  read as a flag.

### Documentation
- README rewritten as a standalone, **internal-use** document; version/manifest
  strings no longer reference other languages.
- New `docs/22-ecosystem-and-packages.md`: module system status + a git-native
  package-manager and ecosystem-security recommendation.
- Money/config docs reworded to avoid naming other languages directly.

### Stdlib: `env` module (NEW) + robust JSON
- New `env` module: typed config getters (`get`/`get_or`/`require`/`int`/
  `float`/`bool`/**`decimal`** for money), `has`/`set`/`unset`/`all`, and
  dotenv-style `.env` loading (`load`/`load_override`/`load_default`). New error
  `E1005` for `env.require` on a missing variable. Guide: `docs/stdlib/env.md`.
- JSON hardened: full string escapes incl. `\uXXXX` + surrogate pairs on decode;
  control-char escaping on encode (always-valid output); bounds-safe parsing (no
  panics). New `json.valid`, `json.get(value,"a.b.0")`, `json.parse`/`stringify`
  aliases.

### Tooling: flexible entry & manifest (Go/Cargo-like)
- `ran run`/`build`/`test` resolve the entry via: explicit file → `ran.toml`
  `entry` (any path/name) → `src/main.ran`/`main.ran` → first `*.ran` defining
  `fn main(`. The entry no longer must be `src/main.ran` or named `main`.
- `ran.toml` is optional; `ran build` only creates it when a **project** build
  uses dependencies. A one-off `ran build file.ran` never litters a manifest.
- Build output uses discrete, always-visible stage lines (no carriage-return
  trickery) with timing — visible even when compilation is near-instant.

### Secure connections: TLS / HTTPS (NEW)
- HTTPS client via **system OpenSSL** (FFI in `src/support/tls.rs`, linked by
  `build.rs`). Full certificate-chain **and** hostname verification — invalid or
  expired certs are rejected, never connected insecurely.
- `http.fetch`/`post_to`/`request` now accept `https://` URLs transparently.
- Cargo `[dependencies]` stays empty; OpenSSL is a system library linked via FFI
  (recorded in `docs/21-dependency-policy.md`).
- Verified live: `https://example.com` → 200; `https://expired.badssl.com` →
  rejected at handshake.

### Audit & docs
- Code audit: clippy clean of correctness/safety errors (26 style warnings
  only); no panics/unwraps in the interpreter hot path; checked money/int math.
- Syntax reference (`docs/11`) synced with the implementation: decimal type,
  checked-arithmetic errors (E1001/E1002), working structs/impl, dotted
  interpolation, updated module/builtin lists.

### Language: OOP / records (NEW)
- Struct literals `Name { field: value }`, instance methods via `impl` (with
  `self`), and associated functions / constructors (`Type.new(...)`).
- Field access on objects, dotted interpolation (`"$user.name"`),
  `typeof(obj)` returns the struct name. Value semantics (immutable updates).
- `else if` chains; struct-literal disambiguation in `if/while/for` headers.
- New syntax error `E0103` (malformed struct-literal field). Guide:
  `docs/stdlib/oop.md`.

### Tooling: package-manager feel (NEW)
- `ran init` scaffolds a `src/` layout (`src/main.ran`, `src/lib/`, `public/`).
- `ran build`/`run`/`test` auto-detect the entry (`src/main.ran`).
- `ran build` shows cargo-style staged progress with timings and **auto-fills
  `ran.toml` `[dependencies]`** from the `import`s in your code.
- `ran test` runs all `test_*` functions and reports pass/fail.

### Database (design only)
- `docs/stdlib/database.md`: full design + API for a std-only PostgreSQL `db`
  module (Option A), with acceptance criteria. Not yet implemented — gated on
  live-server integration tests because it handles money.
- Runnable money demos: `examples/banking.ran`, `examples/ecommerce.ran`
  (`docs/stdlib/bank-and-ecommerce.md`).

### Money & business math (NEW — `decimal`)
- New exact base-10 fixed-point `decimal` type (i128-backed, ~38 digits) for
  money. `+`/`-`/`*` are exact; division and `round` take an explicit rounding
  mode (`half_up`, `half_even`/banker's, `down`, `up`, `floor`, `ceiling`).
- First-class value type: `dec("19.99")` builtin, full operator + comparison
  support, mixed int/decimal promotion, `typeof` → `"decimal"`,
  JSON-number serialization.
- `decimal` module: `new`/`parse`/`from`, `add`/`sub`/`mul`/`div`/`round`/`cmp`/
  `abs`/`neg`/`is_zero`; plus value methods (`.div`, `.round`, `.to_str`, ...).
- New errors: `E1003` (decimal overflow), `E1004` (invalid decimal).
- Guide: `docs/stdlib/decimal.md`. Verified `0.1 + 0.2 == 0.3` exactly.

### Policy & roadmap (NEW)
- `docs/21-dependency-policy.md`: formal zero-third-party-crate policy and the
  bar for exceptions (TLS, CSPRNG).
- `docs/16-roadmap.md`: staged performance plan (interpreter wins → bytecode VM
  → specialization → optional native codegen), with honest expectations.

### Correctness (runtime)
- `return` now propagates correctly out of `for`/`while` loops and nested blocks
  (previously a `return` inside a loop did not exit the function).
- Integer arithmetic is checked: overflow on `+`/`-`/`*` aborts with `E1001`
  instead of silently wrapping.
- Division/modulo by zero aborts with `E1002` instead of silently yielding `0`.
- `spawn`ed tasks are now joined before the program exits.

### Diagnostics
- Parser collects syntax errors and aborts before running, with codes
  `E0100`/`E0101`/`E0102`, precise `file:line:col`, and fix hints.
- New runtime error codes `E1001` (overflow) and `E1002` (divide-by-zero).
  Full reference: `docs/stdlib/errors.md`.

### Security
- HTTP static file serving: percent-decoded path + `..`/NUL rejection +
  canonicalized containment check (no traversal, no symlink escape).
- CORS now reflects a single permitted `Access-Control-Allow-Origin` instead of
  emitting an invalid comma-joined list.
- `url_decode` is UTF-8 correct (decodes to bytes, not Latin-1 chars).
- Server uses a bounded worker pool (`RAN_WORKERS`) instead of unbounded
  thread-per-connection. Bind host is configurable via `RAN_HOST`.
- `ran build` source embedding relabeled honestly as **obfuscation**, not
  encryption.

### Standard library
- New `log` module: `debug`/`info`/`warn`/`error`/`fatal` (stderr, ISO-8601).
- New `http` client: `fetch`, `post_to`, `request` (plaintext http:// only).
- Extended `str` (`starts_with`, `ends_with`, `index_of`, `repeat`, `reverse`,
  `trim_start`/`trim_end`, `pad_left`/`pad_right`, `to_int`/`to_float`).
- Extended `os` (`getpid`, `hostname`, `env_or`), `time` (`now_iso`),
  `json` (`pretty`), `fs` (`size`, `copy`, `rename`).

### Docs & tests
- New `docs/stdlib/` reference covering every module, builtins, error codes,
  and an enterprise/large-project guide.
- New end-to-end test suite in `tests/integration.rs` (10 tests) plus existing
  31 unit tests.

### Known gaps (next up)
- Struct literals in expressions (`User { ... }`) and deref-assignment
  (`*p = ...`) are not yet parsed/executed.
- `match`, channels (`<-`), closures, and `impl` methods parse but do not
  execute.
- No TLS/HTTPS client; `rand` is not a CSPRNG; HTML is not auto-escaped.
- Execution is tree-walking; the bytecode VM exists but is not wired in.

## Versioning policy

Ran uses a deliberate, slow version scheme:

- **0.1.x** - the current series. Active early development. The language, runtime,
  and standard library are written in Rust and evolve quickly. Breaking changes can
  still happen between 0.1.x updates while the design settles.
- **0.x** (future) - larger milestones on the road to self-hosting.
- **1.0.0** - reserved for the **fully self-hosted release**: the Ran compiler and
  toolchain rewritten in Ran itself, no longer depending on the Rust implementation.
  Reaching 1.0.0 is the headline goal, so the project will stay in 0.x until that is
  real and stable.

In short: 1.0.0 means "Ran compiles Ran." Everything before then is 0.x.

## Current: 0.1.2

### Language

- Variables: bash-style (`name="x"`) and `let` / `let mut`, with optional type
  annotations (`let x: int = 5`) that are checked (error E0004 on mismatch).
- Types: int, float, str, bool, arrays, maps.
- Float comparisons (`<`, `<=`, `>`, `>=`, `==`, `!=`) and mixed int/float arithmetic
  (the int is promoted to float).
- Logical `&&` and `||` (operate on truthiness; note: not short-circuit - both sides
  are evaluated).
- String comparisons (lexicographic) and bool `==` / `!=`.
- Functions with typed parameters, return types, and recursion. Argument count is
  checked (error E0003).
- Control flow: `if` / `else`, `for x in array`, `for i in range(n)`, `while`.
- String interpolation: `$name` and `${name}`.
- Three comment styles: `#`, `//`, and nestable `/* */`. A line starting with `;` is
  also a comment.
- `;` is an optional statement separator.
- `echo` prints literally; `echo -e` interprets `\n`, `\t`, `\r`.
- `spawn { }` runs a block on a thread (fire-and-forget).

### Built-in functions

`echo`, `print`, `println`, `len`, `typeof`, `str`, `int`, `float`, `bool`, `push`,
`map`, `set`, `get`, `range`, `keys`, `values`, `abs`, `assert`, `exit`.

### Standard library (each needs `import "x" as x`)

- **math**: `abs`, `max`, `min` (int and float), `sqrt`, `pow`, `floor`, `ceil`,
  `round`, `sin`, `cos`, `tan`, `log`, `log10`, `pi()`, `e()`.
- **json**: `encode(value)`, `decode(string)` - full parsing of objects (-> map),
  arrays, nested structures, numbers, bools, strings (`null` -> void).
- **str**: `from`, `upper`, `lower`, `trim`, `len`, `contains`, `replace`, `split`,
  `join`.
- **rand**: `int(lo, hi)`, `float()`, `bool()` (time-seeded xorshift; not cryptographic).
- **fs**: `read`, `write`, `exists`, `readdir`, `append`, `remove`, `mkdir`, `is_file`,
  `is_dir`.
- **time**: `sleep(ms)`, `now()` (seconds), `now_ms()` (milliseconds).
- **os**: `args`, `env`, `exit`, `cwd`, `platform`, `arch`, `setenv`.
- **http**: `get(path, handler)`, `post(path, handler)`, `server(port)` / `listen(port)`.
- **html**: `render(template)` (variable interpolation only).

### Imports and modules

- stdlib modules require a mandatory alias: `import "std::http" as http`.
- Local files merge into the program: `import "./mathlib"`.
- The names `net`, `crypto`, `sync`, `fmt`, `hardware`, `io`, `regex` are **not**
  importable (importing them errors). `hardware` exists only as library-only Rust code.

### Web and CLI

- The `--port N` flag calls a user-defined `fn port(p: int)` instead of `main()`, so
  the port comes from exactly one place (no "double port").
- HTTP server: routing, path params (`:id`), query params, cookies, CORS, static files
  from `public/`, keep-alive, request-variable interpolation into responses.

### Tooling

- `ran <file>`, `ran run`, `ran build` (encrypted + stripped standalone binary),
  `ran init`, `ran repl`, `ran version`, `ran help`.
- Strict analyzer: undefined variable (E0001), undefined function (E0002), wrong
  argument count (E0003), type mismatch (E0004), stdlib import without alias (E0005).
  Errors show `file:line:col` with a source underline.

## Toward 1.0.0

The remaining big pieces before a self-hosted 1.0.0:

- structs / enums / `match` / closures at runtime
- real ownership / borrow enforcement
- short-circuit `&&` / `||`
- channels and a richer concurrency model
- a package manager (Git-native) and an FFI layer
- activating the bytecode VM as the execution engine
- ultimately, rewriting the compiler in Ran

See [16 - Roadmap](16-roadmap.md) for the live status list.
