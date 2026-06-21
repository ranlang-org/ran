# Roadmap & Feature Status

This page is the honest, single-source status list for Ran v0.2.4. It separates what
works today from what is partial and what is planned. When a doc elsewhere says "Status
note", this is the page it points back to.

> **Status update (memory-safe-self-hosting, Phases A–C complete).** Several items
> previously listed as partial/planned now work: **closures**, **`break`/`continue`**,
> **`return` from a `match` arm**, **traits + `impl Trait for Type`**, and the
> **bytecode VM is now the default engine** (with safe interpreter fallback). The
> runtime is also substantially crash-hardened (recursion guard `E1007`, checked
> arithmetic `E1010`/`E1011`, bounds-safe indexing `E1012`, poisoned-mutex recovery
> `E0511`, recoverable faults everywhere). See the top of
> [20 - Changelog](20-changelog.md) and the root `CHANGELOG.md`. The next big effort is
> **Phase D — native AOT codegen** (designed below, not yet started).

## Working now

These features work and are safe to rely on today.

- **Variables:** bash-style (`name="value"`, untyped), `let`, and `let mut`;
  reassignment of mutable bindings. Optional type annotations on `let`
  (`let x: int = 5`), enforced when present (E0004).
- **Types:** `int` (i64), `float` (f64), `str`, `bool`, arrays, maps; literal type
  inference.
- **Comments:** `#`, `//`, nested `/* */` block comments, and `;`-leading line
  comments.
- **Statement separators:** optional `;` between statements (`echo "a"; echo "b"`); a
  leading `;` makes the line a comment.
- **String interpolation:** plain variable names via `$name` and `${name}` (in `echo`,
  `print` / `println`, and `html.render`). Unknown names are left literal.
- **echo escapes:** `echo` prints `\n` `\t` `\r` literally; `echo -e` interprets them.
  Quote escapes (`\"`, `\'`, `\\`) always work.
- **Functions:** positional parameters, return types, and recursion. Argument count is
  enforced (E0003).
- **Control flow:** `if` / `else` (including nested), `for x in [array]`,
  `for i in range(n)`, and `while`.
- **Operators:** integer `+ - * / %`; float `+ - * /`; mixed int/float arithmetic
  (int promoted to float); all comparisons (`< <= > >= == !=`) on ints, floats, and
  strings (lexicographic), plus `==` / `!=` on bools; string concatenation with `+`
  (including str+int / int+str); logical `!`, `&&`, and `||` (not short-circuit - both
  sides evaluate).
- **Structs, enums, methods & traits:** `struct` definitions, struct-literal
  expressions, field access, `impl` methods (`self`) and associated functions /
  constructors, `enum` declarations, `match` (literal / variant / binding / wildcard
  patterns, including `return` from an arm), and **`trait` declarations + `impl Trait
  for Type`** with default method bodies and receiver-type dispatch.
- **Closures / lambdas:** `fn(x) { ... }` as a first-class value that captures its
  defining scope; can be stored, passed as an argument, and returned.
- **Loop control:** `break` and `continue` in `for`/`while` loops (propagate out of
  nested blocks).
- **Memory safety / crash hardening:** a recursion-depth guard (`E1007`, configurable
  with `--max-depth=<N>`, runs on a large dedicated stack so it fires before any OS
  stack overflow), checked integer arithmetic (`E1010` overflow, `E1011`
  divide/modulo-by-zero), bounds-safe array/string indexing (`E1012`), poisoned-mutex
  recovery (`E0511`), a background memory watchdog + in-loop guard (`E1006`), and
  recoverable runtime faults that unwind to a catch boundary (a faulting `spawn`ed
  thread surfaces an error value to its joiner; a faulting HTTP handler returns 500 and
  the server keeps serving) — no library code calls `process::exit`.
- **Execution engine:** the register/stack **bytecode VM** (`backend/vm/`) is the
  default engine, with type-specialized opcodes and bounded execution
  (`E1008`/`E1009`); it falls back automatically and safely to the tree-walking
  interpreter for any construct it does not yet support.
- **Concurrency:** `spawn { }` (real OS thread); thread join with result/error
  propagation; **channels** (bounded + rendezvous, `send`/`recv`/`close`); **wait
  groups** (`add`/`done`/`wait`); **synchronized shared state** (`shared` +
  `shared_get`/`shared_set`/`shared_add`). See the `concurrency` module in
  [10 - Standard Library](10-stdlib.md).
- **Ownership & borrowing (enforced):** real move tracking, borrow checking,
  dangling-reference and move-while-borrowed detection, and data-race detection on
  `spawn`, behind a `--ownership=warn|strict` mode (default `warn`). `&mut`
  parameters write back to the caller. See [05 - Ownership](05-ownership.md).
- **Native web serving:** the `web` module serves HTML/CSS and client-side assets
  from a directory, with SPA fallback, cache validators (ETag/Last-Modified/304),
  and a frontend build hook. See [17 - Building Websites](17-building-websites.md).
- **SQLite database:** the `db` module (embedded SQLite via the system library)
  with parameterized `query`/`exec`, transactions, and exact `decimal` money
  mapping. See [stdlib/database.md](stdlib/database.md).
- **Indexing & fields:** `array[int]`, `map["key"]`, and `obj.field` on maps and
  struct values.
- **Built-in functions:** `echo`, `print`, `println`, `len`, `typeof`, `str`, `int`,
  `float`, `push`, `map`, `set`, `get`, `range`, `keys`, `values`, `abs`, `assert`,
  `exit`.
- **Methods:** string, array, and map methods listed in
  [10 - Standard Library](10-stdlib.md).
- **Standard library:** `http`, `web`, `db`, `concurrency`, `decimal`, `env`,
  `crypto`, `log`, `time`, `fs`, `json`, `os`, `math`, `html`, `str`, and `rand`,
  each imported with a mandatory alias (`import "std::http" as http`). Full
  `json.decode` (objects -> map, arrays -> array, nested), `math` on ints and floats
  with `sqrt`/`pow`/trig/log/`floor`/`ceil`/`round`/`pi`/`e`, extended `fs`
  (`append`/`remove`/`mkdir`/`is_file`/`is_dir`), `time.now_ms`, and extended `os`
  (`cwd`/`platform`/`arch`/`setenv`).
- **HTTP server:** routing with path params, query params, cookies, CORS, static files
  from `public/`, keep-alive; thread-per-connection. The `--port N` flag calls a
  user-defined `fn port(p: int)` as the entry point instead of `main()`.
- **Imports:** stdlib imports require an alias (`import "std::http" as http`, E0005 without
  it); local imports `import "./file"` and bare-name imports via `.`, `./lib`,
  `./modules`, merged into a flat namespace.
- **Compilation:** `ran build` produces a standalone, stripped binary with the source
  compressed (LZ77) and encrypted (SHA-256 CTR, 100,000-round KDF, magic `RANENCv3`).

## Partial

These work, but with real limitations. Don't assume full behavior.

- **`html.render`** only does `$var` interpolation; it is not a full template engine,
  and it does not auto-escape values.
- **REPL** does not persist variables or functions between lines.
- **Short-circuit `&&` / `||`.** They evaluate correctly but always evaluate both
  sides (no short-circuiting).
- **Deref-assignment (`*p = ...`)** is not parsed yet (`E0102`). Update a value by
  returning it and rebinding, or pass a `&mut` parameter (which writes back).

## Not implemented / planned

In rough priority order. None of these work today.

- **Native AOT machine code (Phase D).** `ran build` still produces an
  interpreter-carrying binary (see Compilation, below); a true native backend is
  designed (Performance roadmap, Stage 4) but not yet implemented.
- **Inbound server TLS** (HTTPS termination inside the Ran server). The HTTP
  **client** already verifies certificates over TLS; the **server** does not yet
  terminate TLS — front it with a TLS-terminating proxy.
- **CSPRNG** and a **password-hashing KDF** — `rand` is not cryptographic and
  `crypto` provides fast hashes only.
- **Regex** and richer date/time (parsing, formatting, durations).
- **Package manager / remote packages.** Imports like `github.com/user/pkg` are not
  fetched; `ran.toml` `[dependencies]` is auto-managed from imports but remote
  resolution is future work.
- **The Ran-in-Ran compiler & bootstrap fixed point** (Phases E–G): a compiler stdlib
  rich enough for a lexer/parser, the compiler written in Ran (`ranc`), and the
  byte-for-byte self-compilation that defines 1.0.0.
- **Cross-compilation** to other targets.

## A note on the bytecode VM

The register/stack bytecode VM in `src/backend/vm/` is now **wired in as the default
execution engine**. It runs full `fn main()` programs for the constructs it supports
(arithmetic with type-specialized opcodes, control flow, user function calls, strings,
arrays/maps/index, structs/fields, etc.) and **falls back automatically to the
tree-walking interpreter** for anything it does not yet implement (methods, module
calls, `spawn`, closures, traits, `match`) — so a program is never executed
incorrectly. Execution is bounded (`E1008` step budget, `E1009` value-stack cap). This
removes the previous caveat: Ran does run on a VM today, for the supported subset, with
a correct interpreter fallback. What remains for "native" is Phase D below.

## See also

- [00 - Introduction](00-introduction.md) for the overview.
- [BUILD_FEATURES.md](../BUILD_FEATURES.md) for an implementation-oriented status list.
- Each chapter's "Status note" callouts for feature-specific detail.

---

## Performance roadmap (toward "fast as practical")

**Honest framing first.** A dynamically-typed language will not match hand-written
Rust on every workload, and we will not pretend otherwise. The realistic goal is
**large, predictable speedups** over today's tree-walking interpreter, with
performance that is more than adequate for business services (APIs, batch jobs,
money calculations). Where raw throughput is critical, the hot path can be written
in the Rust runtime and exposed as a stdlib function.

Current engine: **tree-walking interpreter** (`runtime/`). Simple and correct, but
it re-walks the AST and clones environment state per call.

Planned stages, each independently shippable and measured against a benchmark suite:

### Stage 1 — Interpreter wins (incremental, low risk) — DONE
- Stop cloning the whole variable map per function call; use a proper call stack
  with frames and lexical scopes. **Done** (`runtime/frame.rs`).
- Avoid cloning function bodies; share via `Arc`. **Done.**
- Lazy `for x in range(n)` (constant memory). **Done.**
- Result: 1M-iter loop ~617→~275 ms; fib(30) ~5.1→~3.0 s (release).

### Stage 2 — Bytecode VM (`backend/vm`) — DONE (default engine)
- Compile the checked AST to bytecode and execute it. **Done.**
- Wired into `ran run` and used **by default**, with a safe interpreter fallback
  for unsupported constructs, after passing the full suite + golden examples.
- Bounded execution (`E1008` step budget, `E1009` stack cap) so the VM can never
  loop forever or grow the stack without bound.

### Stage 3 — Static specialization — DONE (initial)
- Use analyzer type information to emit **type-specialized opcodes** (e.g. int-add
  vs generic add), removing runtime type dispatch on hot paths. **Done.**
- Inlining / constant-folding: future refinement.

### Stage 4 — Native AOT compilation (Phase D — designed, not started)

The path to "approaches C/Rust/Go speed". Goal: `ran build x.ran -o x` emits a real
ELF binary — **no embedded interpreter, no `.ran` source in the artifact**.

**Architecture (decided): emit C → link a precompiled runtime → system `cc`.**

- **Do not re-emit the stdlib per program.** Like Go and Rust, the standard library
  and value model are compiled once into a runtime library **`libran_rt`** (C ABI)
  and *linked* into each binary. Emitted code calls into it (`ran_http_*`,
  `ran_db_*`, string/array/map/decimal ops). I/O-bound stdlib latency dominates
  there anyway, so this is optimal — only the compute hot path needs codegen.
- **Lowering (`backend/aot/lower.rs`):** checked program → C. Functions become C
  functions; control flow maps to C `if`/`while`/`for`; `break`/`continue`/`return`
  map directly.
- **Near-C speed via unboxing.** Values are a tagged union `RanValue` with
  reference-counted heap payloads (string/array/map/object) for correctness and
  determinism (no GC initially). But variables the analyzer proves are
  `int`/`bool`/`float` are **unboxed to native `int64_t`/`double`** — no tag, no
  refcount, on the stack/in registers. Numeric loops emit as pure C `int64_t` loops
  → speed on par with `-O2` C.
- **Safety carried over.** Recursion guard, `__builtin_*_overflow`→`E1010`,
  divide-by-zero→`E1011`, index bounds→`E1012` are emitted inline.
- **Build pipeline.** Write `build/<name>.c` → `cc -O2 … -lran_rt [-lssl -lcrypto
  -lsqlite3] -o <name>`; **atomic** (temp file + `rename` on success, delete on
  failure — no partially-executable artifact, R10.6). `--link-static` links static
  archives for a fully self-contained binary.
- **No fake-native.** Constructs outside the supported subset are a **hard build
  error** (`E0606`), never a silent interpreter fallback — every emitted binary is
  100% native or the build fails with a clear diagnostic.
- **New diagnostics:** `E0601` (no `cc`), `E0602` (emit-C failed), `E0603`
  (compile failed, includes `cc` stderr), `E0604` (link failed), `E0605` (missing
  static lib for `--link-static`), `E0606` (construct not yet supported by codegen).
- **Dependency policy:** invoking the system C compiler is recorded as a documented
  exception in [21 - Dependency Policy](21-dependency-policy.md), the same class as
  the already-approved OpenSSL/SQLite FFI. No cargo crates are added.

Planned native iterations (D1→D5):
1. **D1:** `libran_rt` minimal + lowering for the core subset (functions, recursion,
   `if`/`while`/`for`, unboxed+checked int arithmetic, bool, strings + concat +
   `echo`, `return`). Verified: native ELF, output equals the interpreter, runs under
   `env -i`, no `.ran` source in the binary.
2. **D2:** arrays/maps/index, structs/fields, `match`, float/decimal.
3. **D3:** string interpolation, closures, trait dispatch.
4. **D4:** `spawn`/channels (pthreads), stdlib modules (http/db) via `libran_rt`.
5. **D5:** `--link-static`, escape-analysis-driven stack allocation, refcount
   elision where ownership proves uniqueness.

This is the only stage that approaches native speed, and it grows the supported
subset incrementally — the same way self-hosting compilers bootstrap.

### Non-negotiable across all stages
- Exact `decimal` semantics never change for the sake of speed.
- Every stage must pass the existing unit + integration suite and the
  `examples/` golden tests before it becomes the default.
- Overflow/divide-by-zero checks stay on; correctness over raw speed for money.
