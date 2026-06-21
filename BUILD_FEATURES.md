# Ran v0.1.2 - Build Features & Status

This document records what is actually implemented, what is partial, and what is
built-but-not-wired or planned. It is meant to be honest about the current state of
the source tree. For a user-facing roadmap, see `docs/16-roadmap.md`.

## Pipeline (what actually runs)

```
.ran source
    |
    v
+---------+    +--------+    +----------+    +----------------+    +------------------------+
|  Lexer  |--->| Parser |--->|   AST    |--->| Analyzer       |--->| Tree-walking           |
|         |    |        |    |          |    | (strict checks)|    | interpreter (runtime)  |
+---------+    +--------+    +----------+    +----------------+    +------------------------+
                                                                              |
                                                          ran build           v
                                                  +----------------------------------+
                                                  | codegen: compress + encrypt      |
                                                  | source, append to stripped ran   |
                                                  | binary -> standalone executable  |
                                                  +----------------------------------+
```

Every execution path (`ran run`, `ran <file>`, and compiled binaries) uses the
tree-walking interpreter in `src/runtime/`. The codegen step embeds the source; it
does not compile to machine code or bytecode.

---

## Implemented (works today)

### Core language
- [x] Lexer and recursive-descent parser
- [x] Bash-style variables (`name="value"`) and `let` / `let mut` with inference
- [x] Optional type annotations on `let` (`let x: int = 5`), enforced when present (E0004)
- [x] Reassignment of mutable bindings (`c = c + 1`)
- [x] Functions with positional params, return types, and recursion
- [x] Argument-count enforcement (E0003)
- [x] `if` / `else` (including nested), `for x in [array]`, `for i in range(n)`, `while`
- [x] Integer arithmetic (`+ - * / %`) and all integer comparisons
  (div/mod by zero yields 0)
- [x] Float arithmetic `+ - * /` and all float comparisons; mixed int/float arithmetic
  (int promoted to float)
- [x] Comparisons on ints, floats, and strings (lexicographic); `==` / `!=` on bools
- [x] Logical `!`, `&&`, and `||` (operate on truthiness; not short-circuit)
- [x] String concatenation with `+` (including str+int / int+str)
- [x] String interpolation of plain variable names: `$name`, `${name}`
- [x] Arrays and maps, with index access `array[int]` / `map["key"]`
- [x] Field access `obj.field` when the variable holds a map
- [x] Comments: `#`, `//`, nested `/* */`, and `;`-leading lines
- [x] Optional `;` statement separator (`echo "a"; echo "b"`)
- [x] `echo` prints escapes literally; `echo -e` interprets `\n` `\t` `\r`

### Built-in functions
- [x] `echo`, `print`, `println`, `len`, `typeof`, `str`, `int`, `float`
- [x] `push`, `map`, `set`, `get`, `exit`
- [x] `range`, `keys`, `values`, `abs`, `assert`

### Methods
- [x] String: `len`, `to_upper`, `to_lower`, `trim`, `contains`, `starts_with`,
  `ends_with`, `replace`, `split`, `chars`, `repeat`, `slice`
- [x] Array: `len`, `first`, `last`, `contains`, `join`, `reverse`, `slice`
- [x] Map: `keys`, `values`, `has`, `len`

### Standard library modules (runtime-wired)
Each stdlib module must be imported with a mandatory alias before use
(`import "http" as http`, `import "fs" as fs`, etc.). Methods are called on the alias.
- [x] `http`: `server`/`listen`, `get`, `post`
- [x] `time`: `sleep`, `now`, `now_ms`
- [x] `fs`: `read`, `write`, `exists`, `readdir`, `append`, `remove`, `mkdir`,
  `is_file`, `is_dir`
- [x] `json`: `encode` (full); `decode` (full - objects -> map, arrays -> array,
  nested, scalars; null -> void)
- [x] `os`: `args`, `env`, `exit`, `cwd`, `platform`, `arch`, `setenv`
- [x] `math`: `abs`, `max`, `min` (int and float), `sqrt`, `pow`, `floor`, `ceil`,
  `round`, `sin`, `cos`, `tan`, `log`, `log10`, `pi`, `e` (floor/ceil/round return int)
- [x] `html`: `render` PARTIAL (variable interpolation only)
- [x] `str`: `from`, `upper`, `lower`, `trim`, `len`, `contains`, `replace`, `split`,
  `join`
- [x] `rand`: `int`, `float`, `bool` (time-seeded xorshift; NOT cryptographic)

### Concurrency
- [x] `spawn { }` runs a real OS thread (fire-and-forget; the environment is cloned)
- [x] `time.sleep(ms)` to coordinate

### Networking (`src/stdlib/net.rs`, FastServer)
- [x] Thread-per-connection handling (the "workers: 8" log line is cosmetic)
- [x] HTTP/1.1 keep-alive (30s timeout, up to 100 requests/connection), TCP_NODELAY
- [x] 64KB read buffer, 10MB max body
- [x] Routing with path params (`/users/:id`), exact segment match
- [x] Request data injected into handlers as variables (`req_method`, `req_path`,
  `req_body`, `query_<name>`, `param_<name>`)
- [x] Static files from `public/` with MIME detection and `..` traversal blocking
- [x] CORS by default; query-string percent-decoding; cookie parsing

### Modules / imports (`src/support/modules.rs`)
- [x] `import "./file"` merges a local file's declarations into one flat global
  namespace (no module prefix); cycle-guarded
- [x] Search paths: `.`, `./lib`, `./modules`
- [x] Stdlib imports require a mandatory alias (`import "http" as http`); the alias is
  the name you call methods on. Importing a stdlib module without an alias is E0005;
  using a stdlib module without importing it is E0001.

### Compilation & crypto (`src/backend/codegen.rs`, `src/support/crypto/`)
- [x] `ran build` produces a stripped, standalone native binary, zero runtime deps
- [x] LZ77 compression (window 4096, min match 4, max 255)
- [x] SHA-256 CTR-mode stream cipher; iterated SHA-256 KDF (100,000 rounds)
- [x] Payload layout `[ciphertext][nonce:16][size:u64 LE]["RANENCv3"]`;
  nonce = first 16 bytes of `sha256(source)`
- [x] SHA-256 is a real FIPS 180-4 implementation, pure Rust, no dependencies

### Tooling
- [x] `ran <file.ran>`, `ran run [file]`, `ran build`, `ran init`, `ran repl`,
  `ran version`, `ran help`
- [x] `--port N` flag calls a user-defined `fn port(p: int)` as the entry point
  instead of `main()`; errors if no `fn port` is defined

---

## Partial (works with limits)

- `html.render` only performs `$var` interpolation; it is not a full template engine.
- `ran repl` does not persist variables or functions between lines.

> `&&` / `||` work but are not short-circuit (both sides always evaluate).

---

## Built but NOT wired in

### Bytecode VM (`src/backend/vm/`)
- [x] VM data structures, opcodes, compiler, and executor exist in the tree
- [ ] NOT connected to the pipeline. Programs do not run on the VM. There is no flag
  to enable it. Treat the VM as experimental scaffolding only.

### Ownership / borrow checker (`src/semantics/`)
- [x] Ownership states and type-inference scaffolding exist
- [ ] NOT enforced. `&`, `&mut`, and `*` parse but are no-ops at runtime. `&mut` does
  not mutate caller state. Ownership is cosmetic / a design goal.

### Hardware (`src/stdlib/hardware.rs`)
- [x] GPIO / MMIO / serial / syscall code exists as a library
- [ ] NOT exposed to Ran. `import "hardware" as hardware` reports
  `module 'hardware' not found`; the Rust code is unreachable from `.ran` programs.

### HTTP middleware
- [x] A middleware type exists in `net.rs`
- [ ] NOT executed. Do not document middleware as usable.

### Removed module names
- `net`, `crypto`, `sync`, `fmt`, `hardware`, `io`, and `regex` are no longer accepted
  as stdlib imports. Importing one reports `module 'name' not found` at runtime.

---

## Planned (not implemented)

- structs / enums / impl / traits as runtime values (they parse but are not wired in;
  struct-literal expressions and `match` are not parsed)
- closures / lambdas (not parsed)
- channels (`chan`, `<-`)
- real ownership / borrow enforcement
- short-circuit evaluation for `&&` / `||` (they currently evaluate both sides)
- remote packages (paths with `/` print "remote packages not yet supported")
- reading `ran.toml` (it is scaffolding only; nothing parses it yet)
- wiring the bytecode VM as the engine
- cross-compilation

---

## How to Contribute

```bash
# Build
cargo build --release

# Run tests
cargo test

# Run an example
./target/release/ran examples/hello.ran

# Compile to a binary
./target/release/ran build examples/hello.ran -o hello
```
