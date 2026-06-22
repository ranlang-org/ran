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
| `parser.ran` | B2 — tokens → AST | ✅ runs on the interpreter & VM; passes `--ownership=strict` |
| `checker.ran` | B3 — analysis + ownership | ✅ runs on the interpreter & VM; passes `--ownership=strict`; reports E0001/E0002/E0003/E0008 (+ E0100 syntax) over the AST |
| `codegen.ran` | B4 — AST → bytecode/native | ⬜ next |

Run the lexer proof-of-concept:

```fish
ran bootstrap/lexer.ran
# tokenizes `fn add(a, b) { return a + b }` and a second expression
```

Run the parser proof-of-concept:

```fish
ran bootstrap/parser.ran
# lexes + parses several sample programs and prints an indented,
# S-expression-ish AST dump plus a node count for each. Output is identical
# on the VM (default), the interpreter (`--interp`), and under
# `--ownership=strict`.
```

`parser.ran` is a recursive-descent parser for a subset of Ran (function
declarations; `let`/`return`/`if`/`else`/`while`/`echo`/assignment/expression
statements; and the full expression precedence ladder `|| && == != < <= > >=
+ - * / %` with unary `- !`, calls, and parenthesized groups). It produces an
AST of tagged maps and reports syntax errors as `Error` nodes (carrying an
`E####` code, message, and the offending token) instead of crashing the host.

Run the checker proof-of-concept:

```fish
ran bootstrap/checker.ran
# lexes + parses several sample programs and runs the semantic analysis pass,
# printing host-style diagnostics (or "ok (no errors)") for each. Output is
# identical on the VM (default), the interpreter (`--interp`), and under
# `--ownership=strict`.
```

`checker.ran` is a semantic analyzer over the parser's AST, mirroring the host
analyzer (`../src/semantics/analyzer.rs`). It collects top-level function
declarations (name + arity) and reports, collecting **all** diagnostics at once:

- **E0100** — syntax errors (any `Error` node the parser produced), surfaced first.
- **E0008** — duplicate definition (a function declared more than once).
- **E0002** — undefined function (a `Call` to a name that is neither a declared
  function, a built-in, nor an in-scope variable). The built-in set matches the
  interpreter's (`echo`, `print`, `println`, `len`, `typeof`, `str`, `int`,
  `float`, `push`, `map`, `set`, `get`, `exit`, `range`, `keys`, `values`,
  `abs`, `assert`, `bool`, `dec`).
- **E0003** — wrong argument count for a call to a known user function.
- **E0001** — undefined variable (a referenced `Ident` not in scope). Scope is
  lexical: function params + `let` bindings, with `if`/`while` bodies getting
  their own block scope (bindings do not leak out). Function bodies see globals
  plus their own params. Assignment follows the interpreter's `var_set`
  (`../src/runtime/frame.rs`): assigning to an unknown name **defines** it
  (define-on-assign), so an `Assign` target never raises E0001.

Diagnostics are printed in the project's `error[E####]: message` + `= help:`
style. AST nodes do not carry line/col yet, so each diagnostic names the
offending identifier; precise `file:line:col` spans arrive once the parser
threads token positions (a later task).

> The lexer is **duplicated** inside `parser.ran` on purpose, and both the lexer
> and parser are **duplicated** inside `checker.ran` on purpose: each bootstrap
> file has exactly one `fn main()`, so `import "./lexer"` / `import "./parser"`
> would merge multiple `main`s (an `E0008` duplicate definition). Wiring the
> stages together as real modules is task 15.4 (`ranc.ran`); until then each
> file stays self-contained and runnable on its own.

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
