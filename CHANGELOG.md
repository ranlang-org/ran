# Changelog

All notable changes to **Ran** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/), and the project follows the
deliberate version scheme described in [`docs/20-changelog.md`](docs/20-changelog.md)
(1.0.0 is reserved for the fully self-hosted release).

For the complete, historical, feature-by-feature log see
[`docs/20-changelog.md`](docs/20-changelog.md). This file summarizes releases and
the current in-progress work.

## [Unreleased]

Next: native AOT iterations D3+ (general string interpolation, closures, trait
dispatch, then `spawn`/channels and stdlib via `libran_rt`), `--link-static`,
making native the default once the subset matures, and Phases E–G (compiler
stdlib, the Ran-in-Ran compiler `ranc`, and the bootstrap fixed point that
defines 1.0.0).

## [Unreleased]

Next: native AOT D4b — a native map/dict value type (unlocking `json.decode`/`parse`,
`os.meminfo`) and the heavier stdlib modules (`http`, `db`, `web`, `concurrency`,
`crypto`, `env`); then closures/trait dispatch native, `--link-static`, and making
native the default once the subset matures. Phases E–G (compiler stdlib, the
Ran-in-Ran compiler `ranc`, and the bootstrap fixed point that defines 1.0.0) follow.

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
