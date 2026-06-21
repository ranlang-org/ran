# Ran — Road to Bootstrap & Self-Hosting

The single source of truth, organized around **one headline goal**:

> **Ran compiles Ran, and a built program runs anywhere with no `ran` runtime,
> no interpreter, and no source files on the target machine.**

Self-hosting = the Ran compiler and toolchain are written **in Ran**. A built
artifact must be a real, standalone program — not "interpreter + embedded
source". Reaching this is what takes the project to **1.0.0**; until then we stay
in `0.x`.

Legend: `[x]` done · `[~]` partial · `[ ]` not started

Last verified: **372 tests passing** (331 unit incl. property tests P1–P20 for
memory-safe-self-hosting + P1–P11 for enterprise-runtime-capabilities; 2 db-golden;
2 fault-boundary; 26 integration; 3 memory-guard; 8 ownership-golden). Debug +
release build green. **Phases A (crash hardening), B (language core), and C (VM as
the default engine) are complete.**

---

## Where we are today (honest baseline)

- The compiler is written in **Rust** (this is the *stage-0 / host* compiler —
  normal: rustc started in OCaml, Go in C).
- Pipeline: **lexer → parser → AST → analyzer (incl. ownership/borrow) →
  tree-walking interpreter** (`runtime/`).
- `ran build` does **not** generate machine code. It compresses + obfuscates the
  `.ran` source and appends it to a copy of the `ran` runtime; the binary
  decodes and **interprets** it at runtime. So a built binary already runs with
  no separate `ran` install — but it **carries the interpreter** rather than
  being native code.
- A **bytecode VM in `backend/vm/` is now wired in as the default engine** (Phase C),
  with type-specialized opcodes and a safe interpreter fallback for unsupported
  constructs. Tree-walking remains the fallback, not the primary path.

So the remaining gap between "today" and "self-hosted, runtime-free native":
1. **No native code target yet** — the VM is the engine, but `ran build` still emits a
   binary that *carries the interpreter* + obfuscated source rather than native
   machine code. Closing this is **Phase D** (native AOT codegen — designed in
   `docs/16-roadmap.md`, not yet implemented).
2. **The compiler is in Rust**, not Ran (Phases E–G).

---

## Phase 0 — Language core must be solid (prereq for everything)

- [x] Lexer, parser (collects errors, aborts before running: E0100–E0103)
- [x] Functions, recursion, `if`/`else`, `for`/`while`, `return` through loops
- [x] Structs, struct literals, methods/constructors, `enum` + `match`
- [x] Imports: `std::` + local/parent-path; merged into one program
- [x] **Ownership/borrow enforcement** (E0210/E0212/E0214/E0215/E0613), `&mut`
      write-back, `--ownership=warn|strict`
- [x] Lexical block scoping; indexed call-stack frames (no per-call env clone)
- [x] **Closures / lambdas** (`fn(x){...}` as a value) — first-class, capture scope
- [x] **`break` / `continue`** in loops
- [x] **`match`-arm `return`** propagation to the enclosing function
- [x] Trait declarations + dispatch (`trait` + `impl Trait for Type`, default bodies)

## Phase 1 — Result-based error model

- [x] Runtime faults **unwind** (not `process::exit`); server catches per request
      (returns 500, no internal leak), top level prints + exits 70; a faulting
      `spawn`ed thread delivers an inspectable error value to its joiner. Memory
      guard + watchdog (`E1006`), **recursion guard (`E1007`)**, checked arithmetic
      (`E1010`/`E1011`), bounds-safe indexing (`E1012`), poisoned-mutex recovery
      (`E0511`), and `assert`→`E1013` all recoverable.
- [x] **No library code calls `process::exit`** (audited; confined to whitelisted
      boundaries; enforced by a property test). `fault_to_value` exposes faults to
      Ran code as `{ error, code, message }`.
- [~] A first-class **`try`/recover language construct** for user code (the
      value-level error data exists; the surface syntax is still pending).

## Phase 2 — A real execution target (the big one)

This is what makes "compile" mean compile, and unblocks runtime-free binaries.

- [x] **Wired the bytecode VM** (`backend/vm/`) into `ran run` as the **default**
      engine, with a safe automatic fallback to the interpreter for unsupported
      constructs (never runs a program incorrectly). Bounded (`E1008`/`E1009`).
- [x] **Type-specialized opcodes** using analyzer type info (int-add vs generic).
- [~] **Native codegen / AOT**: **designed** (emit C → link precompiled `libran_rt`
      → system `cc`; unbox proven numeric types for near-C speed; unsupported = hard
      build error, not a fallback). See `docs/16-roadmap.md` Stage 4. **Not yet
      implemented** (Phase D, iterations D1–D5).
- [ ] **Static linking** option (`--link-static`) so binaries that use TLS/SQLite
      need no system `.so` on the target (full portability).

Interpreter wins already landed (Stage 1):
- [x] Indexed `Vec` frames + shared globals (no per-call clone); FNV-hashed scopes
- [x] `Arc` function bodies (no per-call AST deep-clone)
- [x] Lazy `for x in range(n)` (constant memory; fixes OOM on huge loops)
- Result so far: 1M-iter loop ~617→~275 ms; fib(30) ~5.1→~3.0 s (release).

## Phase 3 — A written language specification

- [~] Working spec drafted: `docs/25-language-spec.md` (lexical structure,
      grammar, types, scoping, ownership, modules, concurrency, error model,
      execution model). Refine as the language stabilizes; it is the contract the
      self-hosted compiler must match.

## Phase 4 — Stdlib sufficient to write a compiler in Ran

- [x] `fs`, `str`, `os`, `json`, `math`, arrays/maps, `decimal`
- [ ] Richer string/byte ops (slicing, char codes, byte buffers) for a lexer
- [ ] A data-structure layer in Ran (the start of the "rewrite stdlib in Ran" goal)
- [ ] File I/O ergonomics sufficient for reading source trees + writing output

## Phase 5 — Write the Ran compiler in Ran

- [~] `bootstrap/lexer.ran` — source → tokens. **Working** on the interpreter
      today; passes `--ownership=strict`; pure Ran (no stdlib). (Phase B1)
- [ ] `parser.ran` — tokens → AST
- [ ] `checker.ran` — analyzer + ownership (mirrors the spec from Phase 3)
- [ ] `codegen.ran` — AST → the Phase-2 target (bytecode first, then native)
- [ ] CLI in Ran wiring the above (`ranc`)

## Phase 6 — Bootstrap (1.0.0)

- [ ] **Stage A:** Ran-in-Rust runs Ran-in-Ran to compile a test program.
- [ ] **Stage B:** Ran-in-Ran compiles **itself** (produces `ranc'`).
- [ ] **Stage C (fixed point):** `ranc'` compiles Ran-in-Ran again → `ranc''`;
      assert `ranc' == ranc''` byte-for-byte. Self-hosting achieved.
- [ ] **Stage D:** retire the Rust implementation as the default toolchain.

---

## Build / toolchain quality (supporting the above)

- [x] Concise build with a **live spinner** + elapsed/total compile time; full
      per-stage log and artifact dumps gated behind **`--debug`**.
- [x] **Build dumps** to `debug/<name>.{tokens,ast,check,bc}.txt` (`--debug`) for
      debugging the language itself.
- [x] Standalone binary verified to run under `env -i` (no PATH, no `ran`).
- [x] Resource-aware build (memory budget, `--mem-limit`) + clear E07xx diagnostics.
- [x] Duplicate-definition detection (`E0008`) across the merged program.
- [x] Bytecode VM is the **default** engine: bounded (step budget + stack cap +
      panic guard — cannot loop/leak) and safely falls back to the interpreter for
      any unsupported construct.
- [x] VM executes full programs with `fn main()` for the supported subset
      (CALL/RET args, INDEX/FIELD/STRUCT/ARRAY/CONCAT, control flow, type-specialized
      int ops). MODCALL/METHOD/SPAWN/INTERP/closures/traits/`match` still route to the
      interpreter fallback (correctness over coverage).
- [~] **Native code generation** (emit C → link `libran_rt` → system `cc`) so a built
      binary is real machine code with no embedded interpreter — **designed** (Phase D,
      `docs/16-roadmap.md`), not yet implemented.
- [ ] `--emit <items>` / `--no-dump` to control artifact emission.
- [ ] Package manager (later): git-native, hash-locked, taking the best of Go,
      Cargo, npm, RubyGems, Composer. **Not now.**

## Code health / structure

- [x] Split `runtime/mod.rs` (~4.9k → ~2.7k) into `runtime/{json,module_dispatch,
      server,builtins,frame}.rs` + `helpers/{concurrency,db}.rs` (microkernel-style)
- [x] Property-based test harness (std-only), P1–P11
- [ ] Continue fine-grained splits: `module_dispatch` per stdlib module; secondary
      large files (`stdlib/net.rs`, `support/sqlite_ffi.rs`, `semantics/types.rs`)
- [ ] Harden a flaky integration test (`syntax_error_aborts_before_running`) that
      shares a temp path under parallel runs (passes on re-run; isolate the path)

---

## Recommended order

**Phase 2 (wire the VM) → Phase 1 (Result errors) → Phase 3 (spec) → Phase 0
gaps (closures) → Phase 4/5 (compiler in Ran) → Phase 6 (bootstrap).**

Each step keeps the full test suite green before becoming the default.
