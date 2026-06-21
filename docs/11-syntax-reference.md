# Syntax Reference

A compact reference for Ran's syntax. Each entry notes its status where relevant. For
deeper explanations, follow the links to the relevant chapter.

## File structure

```ran
#!/usr/bin/env ran          # optional shebang

# Top-level: config variables and function definitions

name="value"                # bash-style config
let version = "0.2.1"       # immutable binding

fn helper() {
    # ...
}

fn main() {
    # program starts here
}
```

Top-level statements run before `main()`. Execution begins at `main()`.

## Comments

Ran supports several comment styles:

```ran
# bash-style line comment
// C++ style line comment
echo "code"   // inline comment

/* C-style block comment
   spanning multiple lines.
   /* block comments can be nested */
   and the outer comment continues here. */

; a line whose first non-whitespace char is ';' is a comment
   ; leading whitespace before ';' is fine too
echo "x" ; echo "y"   # but ';' between statements is a separator, not a comment
```

The `#`, `//`, and `;`-leading forms are line comments (they run to end of line).
Block comments `/* ... */` can span multiple lines and **nest**. The `#!` shebang on
line 1 is ignored, so `#!/usr/bin/env ran` works.

## Statement separators

Newlines separate statements, and `;` is an **optional** separator too:

```ran
echo "a"; echo "b"    # runs both
echo "a";             # a trailing ';' is fine
```

Careful: a `;` at the **start** of a line (after optional whitespace) makes the whole
line a comment, so `;echo "x"` and `; echo "x"` both do nothing. Use `;` only between
statements, never to begin a line you want to run.

## Variables

```ran
name="Alice"          # bash-style, mutable, type inferred (no spaces around =)
let x = 42            # let binding (spaces required)
let mut count = 0     # mutable binding
count = count + 1     # reassign a mutable binding
```

See [02 - Variables & Types](02-variables-types.md).

## Type annotations

```ran
let name: str = "Ran"
let port: int = 8080
let price: decimal = dec("19.99")
```

Types: `int`, `float`, `decimal` (exact money), `str`, `bool`, arrays, maps,
and user `struct` types.

## String interpolation

```ran
echo "Hello $name"          # plain variable
echo "Port: ${port}"        # explicit boundary
echo "Owner: $account.owner" # dotted field paths on objects/maps
```

Interpolation substitutes **variable names and dotted field paths** (e.g.
`$user.name`, `$order.total`). Arbitrary expressions and calls are **not**
evaluated inline - compute them into a variable first:

```ran
import "std::time" as time

let now = time.now()
echo "Now: $now"
```

Escape sequences: `echo` prints `\n`, `\t`, `\r` **literally** by default; use
`echo -e "..."` to interpret them. Quote escapes `\"`, `\'`, `\\` always work.

## Functions

```ran
fn add(a: int, b: int) -> int {
    return a + b
}

fn greet(name: str) {
    echo "Hello, $name!"
}
```

- Parameters are written `name: type`, positional.
- `-> type` declares a return type (optional).
- `return` sends a value back.
- No closures, no default arguments. Argument count is enforced.

See [03 - Functions](03-functions.md).

## Control flow

```ran
# If / else
if x > 10 {
    echo "big"
} else {
    echo "small"
}

# For (over arrays only)
for item in [1, 2, 3] {
    echo "$item"
}

# While
let mut n = 3
while n > 0 {
    echo "$n"
    n = n - 1
}
```

Conditions need no parentheses; blocks always need braces. See
[04 - Control Flow](04-control-flow.md).

## Concurrency

```ran
import "std::time" as time

spawn {
    echo "background work"
}
time.sleep(100)
```

`spawn { }` runs a block on its own OS thread. Use the `concurrency` module to
join threads, pass values over channels, coordinate with wait groups, and share
state safely. See [06 - Concurrency](06-concurrency.md).

## Operators

### Arithmetic
```
+   -   *   /   %
```
Integers support all of these (checked: overflow aborts with `error[E1001]`,
never silent wraparound). Floats support `+ - * /`. Int and float can be mixed
(the int is promoted to float, so `3.5 + 2` is `5.5`). Integer or decimal `/`
and `%` **by zero abort** with `error[E1002]` (they do not yield `0`).

`decimal` values use exact base-10 arithmetic for money; `+ - *` are exact and
`/` rounds half-up at a sensible scale (use `decimal.div(a,b,scale,mode)` for
explicit control). See [stdlib/decimal.md](stdlib/decimal.md).

### Comparison
```
==   !=   <   <=   >   >=
```
Work on integers, floats, and strings (strings compare lexicographically). `==` and
`!=` also work on bools.

### Logical
```
!        logical not
&&  ||   logical and / or (operate on truthiness; NOT short-circuit - both sides evaluate)
```

### Assignment
```
=
```

### References (ownership) - cosmetic today
```
&        immutable borrow (parses; no-op at runtime)
&mut     mutable borrow   (parses; does NOT mutate the caller)
*        dereference      (parses; no-op at runtime)
```

See [05 - Ownership](05-ownership.md).

### Member access
```
.        method call / module function / field access (objects & maps)
->       return type annotation
```

## Structs, methods & OOP

```ran
struct Account { owner: str, balance: decimal }

impl Account {
    fn open(owner) -> Account {            # associated fn / constructor
        return Account { owner: owner, balance: dec("0.00") }
    }
    fn deposit(self, amount) -> Account {  # instance method (self receiver)
        return Account { owner: self.owner, balance: self.balance + amount }
    }
}

let a = Account.open("Risqi")
let b = a.deposit(dec("100.00"))
echo b.balance        # field access
echo "owner=$b.owner" # dotted interpolation
```

Objects use **value semantics** (methods return updated copies). See
[stdlib/oop.md](stdlib/oop.md).

## Literals

```ran
# Integers
42
-7

# Floats
3.14
-0.5

# Strings
"hello"
"with $interpolation"
"escapes: \n \t \\"

# Booleans
true
false

# Arrays
[1, 2, 3]
["a", "b", "c"]
[]
```

## Collections

```ran
# Arrays
let nums = [1, 2, 3]
push(nums, 4)
echo nums.len()
echo nums[0]

# Maps
let m = map()
set(m, "key", "value")
echo get(m, "key")
echo m["key"]
echo m.keys()
```

## Built-in functions

```
echo    print    println
len     typeof
str     int      float    dec
push    map      set      get
range   keys     values
abs     assert   bool
exit
```

See [10 - Standard Library](10-stdlib.md).

## Standard modules

```
http    time    fs    json    os    math    html    str    rand    log    decimal    env    crypto
```

Import each with a **mandatory alias**, then call methods on the alias:

```ran
import "std::http" as http
import "std::fs" as fs

http.get("/", "home")
let text = fs.read("file.txt")
```

Using a stdlib module without importing it is `error[E0001]`; importing one without an
alias is `error[E0005]`. The names `net`, `crypto`, `sync`, `fmt`, `hardware`, `io`,
and `regex` are **not** stdlib modules; importing one reports `module 'name' not
found`.

## Imports

```ran
import "std::http" as http     # stdlib: `std::` prefix + alias are MANDATORY
import "../shared/money.ran" as money   # local file (relative/parent path)
import "utils" as utils                 # bare name: searched in ., ./lib, ./modules
```

An import has two parts:

```
import "std::http" as http
#       └ which ──┘  └name┘
```

- The **string** says *which* module: `std::<name>` for a standard-library
  module, or a file path (`./x`, `../x.ran`, or a bare name) for local code.
- **`as <name>`** binds a **local alias** — the handle you call it by
  (`http.get(...)`). Choose any name; rename to avoid clashes
  (`import "std::str" as text`). The alias is required so every call site shows
  where a function comes from.

See [13 - Modules & Imports](13-modules-imports.md) and the
[CRUD tutorial](24-realtime-crud-tutorial.md).

## Not yet usable (parses or reserved, but not functional)

```
chan    <-                                 # not implemented
async   await                              # not implemented
trait                                      # parses; no trait dispatch yet
```

`struct`, `impl`, methods, associated functions, **`enum`**, and **`match`** are
functional now. See [16 - Roadmap](16-roadmap.md) for full status.

## Match & enums

```ran
enum Status { Active, Inactive, Pending }

fn label(s) -> str {
    return match s {
        Status.Active => "active"        # enum variant pattern
        Status.Inactive => "inactive"
        _ => "other"                     # wildcard
    }
}

fn grade(n: int) -> str {
    return match n {
        100 => "perfect"                 # literal pattern
        other => "scored " + other       # binding pattern (matches all, binds)
    }
}
```

Patterns: literals, enum variants (`Enum.Variant`), a bare identifier (binds the
subject), and `_` (wildcard). Arm bodies are a single statement or a `{ }` block.
Note: `return` inside a match arm does not yet propagate out of the function.

## Semicolons and blocks

- Newlines separate statements; `;` is an optional separator (`echo "a"; echo "b"`).
- A trailing `;` is allowed (`echo "a";`).
- A **leading** `;` makes the whole line a comment (`;echo "x"` does nothing).
- Curly braces `{ }` delimit every block. Single-line bodies without braces are not
  allowed.

```ran
# Correct
if x > 0 {
    echo "positive"
}

# Also fine - ';' as a separator
echo "a"; echo "b"

# Not allowed
# if x > 0 echo "positive"
```

## Naming conventions

| Kind | Convention | Example |
|------|------------|---------|
| Variables | `snake_case` | `user_name` |
| Functions | `snake_case` | `read_config` |
| Constants/config | `UPPER_SNAKE` or lowercase | `MAX_RETRIES`, `port` |
| Files | `snake_case.ran` | `web_server.ran` |

Next: [Error Handling](12-error-handling.md).
