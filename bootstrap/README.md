# Bootstrap — the self-hosted Ran compiler

This directory holds the **Ran compiler written in Ran** (`ranc`), being built
incrementally toward self-hosting. It runs on the current `ran` interpreter
(the Rust "stage-0" host) today.

> Self-hosting goal: `ranc` compiles Ran — including itself — so the Rust
> implementation is needed only to bootstrap once on a new machine.

## What works now

| File | Phase | Status |
|------|-------|--------|
| `lexer.ran` | B1 — source → tokens | ✅ runs on the interpreter; passes `--ownership=strict` |
| `parser.ran` | B2 — tokens → AST | ⬜ next |
| `checker.ran` | B3 — analysis + ownership | ⬜ |
| `codegen.ran` | B4 — AST → bytecode/native | ⬜ |

Run the lexer proof-of-concept:

```fish
ran bootstrap/lexer.ran
# tokenizes `fn add(a, b) { return a + b }` and a second expression
```

It is written in **pure Ran** (no `std::` imports) using only core features:
`.chars()`, array indexing, `push`, and lexicographic character comparison.

## The bootstrap plan (summary)

See `../TODO.md` and `../docs/25-language-spec.md` for the full roadmap. Order:

1. **Fase A (in Rust, the host):** finish the bytecode VM (enter `fn main()` +
   remaining opcodes), add a `Result`-based error model, close language gaps
   (closures, `break`/`continue`, `match` return), grow the stdlib (file I/O,
   bytes, strings) enough to write a compiler.
2. **Fase B (here, in Ran):** `lexer.ran` → `parser.ran` → `checker.ran` →
   `codegen.ran`, written against the stable language subset and the spec.
3. **Fase C (bootstrap, fixed point):**
   - Stage 0: host runs `ranc` to compile a test program.
   - Stage 1: `ranc` compiles **itself** → `ranc₁`.
   - Stage 2: run `ranc₁`, compile `ranc` again → `ranc₂`.
   - **Self-hosted** when `ranc₁ == ranc₂` byte-for-byte.
   - Stage 3: retire the Rust toolchain as the default.

We target the **bytecode VM first** (easier than native); native code generation
comes after self-hosting.

## Why a separate, growing component set

Each file is added only when the language + stdlib can express it cleanly and a
golden test pins its output. `lexer.ran` is the first such milestone: a real,
running piece of the future self-hosted compiler.
