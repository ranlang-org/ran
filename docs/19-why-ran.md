# Why Ran — Feature Highlights

Ran is an internal-use language built around a few strong ideas. This page
summarizes what makes it worth reaching for on in-house projects.

## The pitch

Write small services and tools with minimal ceremony, get a strict compiler that
catches mistakes before anything runs, compute money exactly, talk to the
network over TLS, and ship the whole thing as one self-contained native binary.

## Standout features

### 1. Low-ceremony declarations
Variables need no boilerplate:

```ran
name="Ran"
port=8080
ready=true
echo "Listening on port $port"
```

### 2. Exact money math
A first-class `decimal` type with explicit rounding — no floating-point drift:

```ran
import "std::decimal" as decimal
let total = dec("19.99") * dec("3")          # 59.97 exactly
let tax = decimal.round(total * dec("0.11"), 2, "half_up")
```

### 3. A strict compiler that fails fast
Mistakes are caught before the program runs, all at once, with codes and exact
`file:line:col` locations:

```
error[E0001]: undefined variable: `clear`
  --> app.ran:4:5
  |
4 |     clear
  |     ^ not found in this scope
  = help: declare it first: `let clear = ...`
```

See [12 - Error Handling](12-error-handling.md) and
[stdlib/errors.md](stdlib/errors.md).

### 4. Records and methods
Model your domain with structs, methods, and constructors:

```ran
struct Account { owner: str, balance: decimal }
impl Account {
    fn open(owner) -> Account { return Account { owner: owner, balance: dec("0.00") } }
    fn deposit(self, amount) -> Account {
        return Account { owner: self.owner, balance: self.balance + amount }
    }
}
```

### 5. A built-in web server and TLS client
Routing is a function and a string; outbound HTTPS verifies certificates:

```ran
import "std::http" as http
fn home() { return "<h1>Hi</h1>" }
fn main() {
    http.get("/", "home")
    http.server(8080)
}
```

Path params (`:id`), query strings, cookies, CORS, and static files work out of
the box. See [17 - Building Websites](17-building-websites.md).

### 6. One self-contained binary
`ran build` produces a single stripped native executable with the source
embedded (compressed and obfuscated). No runtime or compiler needs to be
installed on the target. See [14 - Security](14-security.md).

### 7. Configuration and logging built in
`env` for typed config and `.env` loading; `log` for leveled structured logs.

### 8. Concurrency without ceremony

```ran
spawn { echo "working in the background" }
```

### 9. Several comment styles

```ran
# hash line comment
// line comment
/* block /* nested */ comment */
; a line starting with ';' is also a comment
```

## Where Ran fits

Ran targets a specific sweet spot: internal services, business tooling, CLI
utilities, and money-handling logic that you want to ship as one fast binary.
It is not a general-purpose public product.

## Honest limitations (today)

Ran is young (v0.2.4). Not yet available:

- closures / lambdas (`fn(x) { ... }` as a value) — planned
- real ownership enforcement (syntax parses but is cosmetic)
- short-circuit `&&` / `||` (they work but always evaluate both sides)
- channels, a package manager, and the bytecode VM (all planned)

The full, accurate status is in [16 - Roadmap](16-roadmap.md).
