# Ran Language Specification (working draft)

This is the working specification of the Ran language as implemented today. It
exists so a **second, self-hosted implementation** (`bootstrap/`) can be written
to match the stage-0 (host) implementation exactly. It describes only what is
implemented; planned features are listed at the end.

Status: draft for v0.3.6. Where the prose and the implementation disagree, the
implementation (and its test suite) is authoritative until this document is
corrected.

---

## 1. Source & lexical structure

A program is UTF-8 text. The lexer (`frontend/lexer.rs`; reference reimplementation
in `bootstrap/lexer.ran`) turns it into tokens.

### 1.1 Whitespace & line terminators
Spaces, tabs, carriage returns, and newlines separate tokens. Newlines and `;`
both terminate statements; a trailing `;` is allowed.

### 1.2 Comments
- Line: `# ...`, `// ...`, or a line whose first non-space char is `;`.
- Block: `/* ... */`, **nestable**.
- A `#!` shebang on line 1 is ignored.

### 1.3 Identifiers
`[A-Za-z_][A-Za-z0-9_]*`.

### 1.4 Keywords
`fn let mut if else return while for in import as struct enum impl match spawn
true false`. (`break`, `continue`, `trait`, `async`, `await`, `chan` are reserved
but not yet functional — see §12.)

### 1.5 Literals
- Integer: `[0-9]+` (64-bit signed, `i64`).
- Float: `[0-9]+ '.' [0-9]+` (and `e`/`E` exponents), `f64`.
- String: `"..."` with escapes `\" \\ \n \t \r`. In `echo`, `\n`/`\t`/`\r` print
  literally unless `echo -e` is used; quote/backslash escapes always apply.
- Boolean: `true`, `false`.
- Decimal (money): produced by `dec("…")`, not a lexical literal.

### 1.6 Operators & punctuation
Two-char: `== != <= >= && || -> => ::`.
Single-char: `+ - * / % < > = ! & * . , ; : ( ) [ ] { }`.

---

## 2. Grammar (EBNF-style overview)

```
program     = { statement } ;
statement   = var_decl | fn_decl | struct_decl | enum_decl | impl_block
            | if_stmt | for_stmt | while_stmt | spawn_stmt | import_stmt
            | echo_stmt | return_stmt | expr_stmt ;

var_decl    = ( "let" [ "mut" ] ident [ ":" type ] "=" expr )
            | ( ident "=" value )            (* bash-style, untyped *) ;
fn_decl     = [ "pub" ] "fn" ident "(" [ params ] ")" [ "->" type ] block ;
params      = param { "," param } ;
param       = [ "mut" ] ident [ ":" type ] ;
struct_decl = [ "pub" ] "struct" ident "{" { ident ":" type "," } "}" ;
enum_decl   = [ "pub" ] "enum" ident "{" ident { "," ident } "}" ;
impl_block  = "impl" ident "{" { fn_decl } "}" ;
if_stmt     = "if" expr block [ "else" ( if_stmt | block ) ] ;
for_stmt    = "for" ident "in" expr block ;
while_stmt  = "while" expr block ;
spawn_stmt  = "spawn" block ;
import_stmt = "import" string [ "as" ident ] ;
echo_stmt   = "echo" [ "-e" ] expr ;
return_stmt = "return" [ expr ] ;
block       = "{" { statement } "}" ;

expr        = assignment ;
assignment  = lvalue "=" expr | logic_or ;
logic_or    = logic_and { "||" logic_and } ;
logic_and   = equality { "&&" equality } ;
equality    = comparison { ("==" | "!=") comparison } ;
comparison  = term { ("<" | "<=" | ">" | ">=") term } ;
term        = factor { ("+" | "-") factor } ;
factor      = unary { ("*" | "/" | "%") unary } ;
unary       = ("!" | "-" | "&" | "&mut" | "*") unary | postfix ;
postfix     = primary { call | index | field | method } ;
call        = "(" [ args ] ")" ;
index       = "[" expr "]" ;
field       = "." ident ;
method      = "." ident "(" [ args ] ")" ;
primary     = int | float | string | bool | ident | array | struct_init
            | match_expr | "(" expr ")" ;
array       = "[" [ expr { "," expr } ] "]" ;
struct_init = ident "{" { ident ":" expr "," } "}" ;
match_expr  = "match" expr "{" { pattern "=>" ( expr | block ) } "}" ;
pattern     = literal | ident "." ident | ident | "_" ;
type        = ident | "&" type | "&mut" type | "[" type "]" ;
```

Notes: `&&`/`||` **short-circuit** — the right operand is evaluated only when it
can change the result (`&&` skips the right side when the left is falsy; `||`
skips it when the left is truthy). A single expression must fit on one line (no
implicit line continuation).

---

## 3. Types

`int` (i64), `float` (f64), `decimal` (exact base-10), `str`, `bool`, arrays
`[T]`, maps (string-keyed), and user `struct`/`enum`. Type annotations on `let`
and parameters are checked when present (E0004). Money must use `decimal`, never
`float`.

Integer arithmetic is **checked**: overflow → `E1001`, divide/modulo by zero →
`E1002`. `decimal` is exact (`E1003`/`E1004` on overflow/parse).

---

## 4. Scoping & evaluation

- Lexical block scoping: `if`/`for`/`while`/`match` arms and function bodies push
  a scope; block-locals do not leak out.
- A function sees **globals + its own parameters/locals**, never the caller's
  locals.
- Assignment (`x = e`) updates the nearest existing binding (enabling
  accumulation like `total = total + i`); `let x = e` declares in the current
  scope.
- Top-level statements run before `main()`. Execution begins at `fn main()`.
- Runtime memory: values are released exactly once when their owning scope ends
  (no garbage collector). A loop guard (`E1006`) aborts before OS OOM.

---

## 5. Ownership & borrowing

Scalars (`int`, `float`, `bool`, `void`) are copied. Everything else (`str`,
arrays, maps, structs) has a single owner and is **moved** on assignment or
by-value argument passing. Enforced in two modes (`--ownership=warn|strict`):

| Code | Rule |
|------|------|
| E0210 | use after move |
| E0212 | conflicting borrows (`&mut` excludes other borrows) |
| E0214 | dangling reference (borrow outlives referent) |
| E0215 | move while borrowed |
| E0613 | unsynchronized shared write captured by `spawn` |

`&T` is a shared borrow (many at once); `&mut T` is an exclusive borrow and
**writes back to the caller's lvalue** after the call. `*p = …`
(deref-assignment) is not yet parsed.

---

## 6. Functions & calls

Positional parameters; argument count enforced (E0003). `return` propagates
through loops/blocks. Methods on structs use a `self` receiver; associated
functions (constructors) are `Type.method(...)`. Objects have value semantics
(methods return updated copies). `&mut` parameters write back.

---

## 7. Modules & imports

- Stdlib: `import "std::name" as alias` (alias mandatory, E0005; unknown name
  E0007; bare name without `std::` is E0006). Built-in modules: `http web db
  concurrency decimal crypto env log time fs json os math html str rand`.
- Local: `import "./file"` / `import "../file.ran"` / bare name searched in `.`,
  `./lib`, `./modules`. Local imports merge declarations into one flat namespace
  (no alias). Duplicate function definitions across the merged program are an
  error (E0008).

---

## 8. Concurrency

`spawn { }` runs a block on an OS thread. The `concurrency` module provides
thread join, channels (bounded + rendezvous), wait groups, and synchronized
shared state. Errors are returned as handleable values (E0610–E0614). Writing a
captured variable from `spawn` without synchronization is E0613.

---

## 9. Error model

- **Compile-time** errors (E00xx/E01xx parse, E02xx ownership) abort before the
  program runs; each carries a code, `file:line:col`, a source underline, and a
  help hint.
- **Runtime** faults (E1xxx) unwind to a catch boundary: the top level prints and
  exits 70; the HTTP server returns 500 per request and keeps serving. A fully
  `Result`-threaded model (user `try`/recover) is planned (§12).

---

## 10. Execution model

The reference engine is a tree-walking interpreter over the checked AST.
`ran build` embeds the (merged) source into a copy of the runtime; the binary
decodes and interprets it (so it runs with no `ran` install). An experimental
bytecode VM (`--vm`) executes a subset and falls back to the interpreter. Native
code generation is planned.

---

## 11. Standard library surface

See `docs/stdlib/`. Money (`decimal`), HTTP server + TLS client (`http`), native
web serving (`web`), SQLite (`db`), concurrency, crypto hashing, env/config,
logging, JSON, fs, os, time, math, str, rand, html.

---

## 12. Not yet specified / implemented

Closures, `break`/`continue`, `match`-arm `return` propagation, traits,
deref-assignment, a `Result`-threaded error model,
inbound server TLS, a package manager, and native code generation. These are on
the bootstrap roadmap (`TODO.md`); the self-hosted compiler targets the stable
subset above first.
