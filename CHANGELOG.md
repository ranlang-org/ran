# Changelog

All notable changes to **Ran** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and the project follows the
deliberate version scheme described in [`docs/20-changelog.md`](docs/20-changelog.md)
(1.0.0 is reserved for the fully self-hosted release).

For the complete, historical, feature-by-feature log see
[`docs/20-changelog.md`](docs/20-changelog.md). This file summarizes releases and
the current in-progress work.

## [Unreleased] — Memory-safe self-hosting

Work tracked under the `memory-safe-self-hosting` effort, organized in phases.
**Phases A, B, and C are complete and verified** (full test suite green: 372 tests
— 331 unit + 41 integration/golden, plus property-based tests P1–P20). **Phase D
(native AOT codegen) is designed and not yet started.**

### Added — Phase A: memory safety & crash hardening (highest priority)

- **Recursion-depth guard (`E1007`).** The runtime now tracks per-thread Ran
  call depth and raises a *catchable* fault before the OS stack overflows. This
  closes the most serious crash source: previously, deep/unbounded recursion
  caused an uncatchable `SIGSEGV`. Execution now runs on a dedicated 1 GiB-stack
  thread so the guard always fires first. Configurable via `--max-depth=<N>`
  (default 10000; invalid values fall back without aborting).
- **Checked integer arithmetic.** Overflow on `+ - * /  %` raises `E1010`
  (instead of silently wrapping — the release profile disables overflow checks),
  and integer division/modulo by zero raises `E1011`.
- **Bounds-safe indexing (`E1012`).** Out-of-range or negative array/string
  indices raise a fault carrying the index and length instead of panicking; string
  indexing is Unicode-scalar (char-boundary) safe.
- **Poisoned-mutex recovery (`E0511`).** A centralized `lock_or_fault` helper maps
  a poisoned `Mutex` to a recoverable fault; all risky `.lock().expect(...)` sites
  in `stdlib/db.rs` and `stdlib/concurrency.rs` were converted. A failed thread no
  longer cascades into a process-wide crash.
- **No `process::exit` in library code.** Audited every exit site; the library
  `assert` failure now raises `E1013` (recoverable) instead of exiting. Exit is
  confined to whitelisted boundaries (top-level runner, compile-error boundary,
  user-requested `exit`/`os.exit`, and the memory watchdog). A property test
  statically enforces this invariant.
- **Recoverable fault delivery.** A `RuntimeFault` raised inside a `spawn`ed thread
  is delivered to its joiner as an inspectable error value (not a crash); a faulting
  HTTP request handler returns `500` and the server keeps serving, without leaking
  internal fault/stack details to the client.
- **Memory watchdog & loop guard (`E1006`)** formalized across all execution paths
  (interpreter, standalone binary, HTTP server, `--vm`), installed idempotently.
- **Bounded bytecode VM (`E1008` step budget, `E1009` value-stack cap).**
- `fault_to_value` exposes faults to Ran code as `{ error, code, message }` for
  `try`/recover style handling.

### Added — Phase B: language core for writing a compiler

- **Closures / lambdas** (`fn(x) { ... }`) as first-class values that capture their
  defining scope; storable, passable, and returnable.
- **`break` / `continue`** in `for`/`while` loops, propagating correctly out of
  nested blocks.
- **`return` from inside a `match` arm** now unwinds to the enclosing function.
- **Traits:** `trait` declarations (with optional default method bodies) and
  `impl Trait for Type`, with dispatch selected by the receiver value's type.
- The ownership/borrow checker accepts all the new constructs under
  `--ownership=strict` without false positives.

### Changed — Phase C: bytecode VM is now the execution engine

- The register/stack **bytecode VM** (`backend/vm/`) is now **wired in as the
  default engine**, with automatic, safe fallback to the tree-walking interpreter
  for any construct it does not yet support (it never runs a program incorrectly).
- **Type-specialized opcodes** (e.g. integer add) use analyzer type information to
  avoid generic dispatch on hot paths.
- Bounded execution (`E1008`/`E1009`) and an output-buffer model keep the VM safe and
  keep fallback output correct.

### Fixed

- Stale-incremental build artifacts (from a project relocation) and two pre-existing
  flaky tests (depth property tests racing on the process-global call-depth limit).
- Integer overflow / divide-by-zero integration tests updated to the new `E1010` /
  `E1011` codes.

### Designed (not yet implemented) — Phase D: native AOT codegen

A real ahead-of-time native backend: lower checked programs to C, link against a
precompiled `libran_rt` runtime/stdlib library, and produce true ELF machine code
with no embedded interpreter and no `.ran` source in the artifact. Stdlib is linked
(Go/Rust model), not re-emitted per program; hot numeric code is unboxed to native
`int64`/`double` for near-C speed. Unsupported constructs are a hard build error
(`E06xx`), never a silent interpreter fallback. See the design in
`docs/16-roadmap.md` (Performance roadmap, Stage 4). Phases E–G (compiler stdlib,
Ran-in-Ran compiler, bootstrap fixed point) follow.

## [0.2.1] — bytecode target, faster runtime, build dumps

- Indexed call-stack frames + shared globals + `Arc` function bodies; lazy
  `for x in range(n)`; runtime memory guard (`E1006`).
- Bytecode VM target as a build artifact (disassembly dump); microkernel-style
  `runtime/` split. See [`docs/20-changelog.md`](docs/20-changelog.md) for detail.
