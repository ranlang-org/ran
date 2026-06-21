# Ran Architecture

The compiler and runtime are organized into five clean layers. Source flows
top-to-bottom: text becomes tokens, tokens become an AST, the AST is analyzed,
then either interpreted or compiled to a standalone binary.

```
                   +-------------------+
   source.ran ---> |     frontend      |  text -> tokens -> AST
                   +-------------------+
                            |
                            v
                   +-------------------+
                   |     semantics     |  name resolution, types, ownership
                   +-------------------+
                            |
              +-------------+-------------+
              v                           v
     +-----------------+        +-------------------+
     |     runtime     |        |      backend      |
     |  (interpreter)  |        |  (vm + codegen)   |
     +-----------------+        +-------------------+
              |                           |
              +-------------+-------------+
                            v
                   +-------------------+
                   |      stdlib       |  http, hardware, concurrency
                   +-------------------+

   support/ (diagnostics, crypto, modules) is used by all layers.
```

## Layers

### frontend/ - Source to AST
- `token.rs` - token definitions and source spans
- `lexer.rs` - tokenizer (bash-style `$var`, `echo`, `#` comments)
- `parser.rs` - recursive-descent parser producing a spanned AST
- `ast.rs` - AST node definitions (`Stmt`, `Statement`, `Expression`, `Span`)

### semantics/ - Analysis
- `analyzer.rs` - strict checks: undefined variables/functions (E0001/E0002),
  arity (E0003), type mismatch (E0004). Reports line:column diagnostics.
- `types.rs` - type system and ownership/borrow tracking

### backend/ - Compilation & VM
- `codegen.rs` - produces standalone binaries today (compress -> obfuscate ->
  strip -> embed). A native AOT backend (`backend/aot/`, emit C -> link
  `libran_rt` -> system `cc`) is designed for Phase D; see `docs/16-roadmap.md`.
- `vm/` - register/stack bytecode VM, **now the default execution engine** (with a
  safe fallback to the interpreter for unsupported constructs)
  - `opcodes.rs`, `chunk.rs`, `value.rs`, `exec.rs`, `compiler.rs`
  - bounded execution: step budget (`E1008`) + value-stack cap (`E1009`)

### runtime/ - Interpretation
- `mod.rs` - tree-walking interpreter with the full standard library,
  used by `ran run` and by compiled binaries

### stdlib/ - Built-in Libraries
- `net.rs` - FastServer HTTP (multi-threaded, keep-alive, routing, CORS)
- `hardware.rs` - GPIO, MMIO, serial, syscall
- `concurrency.rs` - channels, spawn, WaitGroup

### support/ - Cross-cutting Concerns
- `diagnostics.rs` - Rust-grade error reporting (codes, spans, help text)
- `crypto/` - SHA-256, stream cipher, LZ compression (source protection)
- `modules.rs` - module resolution and import loading (Go-style)

## Data Flow

1. `frontend::lexer::tokenize(source)` -> `Vec<Token>`
2. `frontend::parser::parse(tokens)` -> `Program` (spanned AST)
3. `support::modules::load_program(file)` -> merges imported modules
4. `semantics::analyzer::analyze_with_file(...)` -> `CheckedProgram` (or aborts)
5a. `ran run`: compiles to bytecode and runs on the **VM** by default
    (`backend::vm`), falling back to `runtime::execute(checked)` (the tree-walking
    interpreter) for any construct the VM does not yet support. Execution runs on a
    large dedicated stack so the recursion guard (`E1007`) fires before an OS stack
    overflow.
5b. `ran build`: `backend::codegen::compile_standalone(source, out)` (native AOT is
    Phase D).

## Compiled Binary Format

```
[ stripped ran runtime ][ compressed+encrypted source ][ nonce:16 ][ size:u64 ][ "RANENCv3" ]
```

The runtime detects the `RANENCv3` trailer at startup, decrypts and
decompresses the embedded source, and runs it. See `docs/14-security.md`.
