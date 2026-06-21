# Error Handling

Ran's analyzer is strict, in the spirit of Rust. Before your program runs, it is
checked for a range of mistakes, and any problems are reported with an error code, a
message, a `file:line:col` location, a source-context underline, and often a help line.
If there are any errors, the program does not run.

## How errors are reported

When the checker finds problems, it reports **all of them at once** rather than
stopping at the first. At the end, it summarizes with a line like:

```
error: aborting due to 2 errors emitted
```

(The count is singular for a single error: `aborting due to 1 error emitted`.) This
lets you fix several issues in one pass. Each error has a code of the form
`error[E000N]`, a description, and a pointer at the offending code.

## The error codes

| Code | Name | Meaning |
|------|------|---------|
| `E0001` | Undefined variable or module | You used a variable or module that was never declared (or never imported). |
| `E0002` | Undefined function | You called a function that does not exist. |
| `E0003` | Wrong argument count | You called a function with the wrong number of arguments. |
| `E0004` | Type mismatch | An annotated `let` does not match the literal assigned to it. |
| `E0005` | Missing stdlib import alias | You imported a stdlib module without the mandatory `as alias`. |
| `E0008` | Duplicate definition | A function/type is defined more than once across the merged program. |

Ownership and borrow checking **are** enforced (behind `--ownership=warn|strict`,
default `warn`): use-after-move (`E0210`), conflicting borrows (`E0212`),
dangling references (`E0214`), move-while-borrowed (`E0215`), and unsynchronized
shared access across `spawn` (`E0613`). See [05 - Ownership](05-ownership.md).

### Runtime fault codes

These are raised while the program runs. They **unwind** to the nearest catch
boundary (the top-level runner prints the diagnostic and exits `70`; the HTTP
server turns a handler fault into a `500` and keeps serving) instead of crashing
the process. Inside Ran code they can be inspected as `{ error, code, message }`.

| Code | Meaning |
|------|---------|
| `E1002` / `E1003` | Decimal divide-by-zero / decimal overflow. |
| `E1004` | Invalid decimal value. |
| `E1005` | Required env var missing (`env.require`). |
| `E1006` | Out of memory: the watchdog / in-loop guard stopped the process before the OS OOM-killer (or raised a recoverable fault in a loop). |
| `E1007` | Recursion/call-depth limit exceeded (configurable with `--max-depth=<N>`); prevents an uncatchable stack overflow. |
| `E1008` / `E1009` | Bytecode VM step-budget / value-stack-cap exceeded (recoverable; falls back to the interpreter). |
| `E1010` | Integer overflow (`+ - * / %`) — never wraps silently. |
| `E1011` | Integer division or modulo by zero. |
| `E1012` | Array/string index out of bounds (message carries the index and length). |
| `E1013` | `assert` failed (recoverable, not a process exit). |
| `E0511` | A shared lock was poisoned by a failed thread and was recovered as a fault. |

### error[E0001] - undefined variable or module

You referenced a name that has not been declared (or misspelled one), or you used a
stdlib module without importing it.

```ran
fn main() {
    echo "$usrname"      # typo: variable is `username`
}
```

```
error[E0001]: undefined variable or module: `usrname`
```

The label reads `not found in this scope`. The same error fires if you call, say,
`http.get(...)` without first writing `import "std::http" as http`.

**Fix:** declare the variable, correct the spelling, or import the module with an alias.

```ran
fn main() {
    username="Alice"
    echo "$username"
}
```

### error[E0002] - undefined function

You called a function that does not exist.

```ran
fn main() {
    greet("World")       # no function named `greet`
}
```

```
error[E0002]: undefined function: `greet`
```

**Fix:** define the function, or correct the name.

```ran
fn greet(name: str) {
    echo "Hello, $name!"
}

fn main() {
    greet("World")
}
```

### error[E0003] - wrong argument count

You called a function with too few or too many arguments.

```ran
fn add(a: int, b: int) -> int {
    return a + b
}

fn main() {
    let x = add(1)       # add expects 2 arguments, got 1
}
```

```
error[E0003]: function `add` expects 2 arguments, but 1 was provided
  = help: call `add` with exactly 2 arguments
```

**Fix:** pass the right number of arguments.

```ran
fn main() {
    let x = add(1, 2)
}
```

### error[E0004] - type mismatch

An annotated `let` binding does not match the literal you assigned.

```ran
fn main() {
    let count: int = "hello"   # expected int, found str
}
```

```
error[E0004]: type mismatch: expected `int`, found `str`
```

The label reads `this is a `str``.

**Fix:** assign a value of the annotated type (or change the annotation).

```ran
fn main() {
    let count: int = 42
}
```

### error[E0005] - missing stdlib import alias

Stdlib modules must be imported with a mandatory alias. Importing one without `as
alias` triggers this error.

```ran
import "std::http"            # missing the alias

fn main() {
    http.server(8080)
}
```

```
error[E0005]: stdlib import `http` requires an alias
  = help: write: import "std::http" as http
```

**Fix:** add the alias, then call methods on it.

```ran
import "std::http" as http

fn main() {
    http.server(8080)
}
```

## Reading the output

A typical report includes:

- The **error code** and a one-line description.
- The **location** in your source (`file:line:col`) with the offending span underlined.
- Sometimes a **help** suggestion.

When several issues are found, they are printed in sequence and followed by the
`aborting due to N errors emitted` summary. Work through them top to bottom.

## Runtime conditions

The strict checks above happen before your program runs. Some conditions only appear at
runtime, and Ran reports those as it executes. For example, reading a file that does not
exist prints a message and yields no value:

```ran
import "std::fs" as fs

fn main() {
    let content = fs.read("missing.txt")
    # ran: fs.read error: No such file or directory
}
```

Guard against these by checking first:

```ran
import "std::fs" as fs

fn main() {
    if fs.exists("config.txt") {
        let content = fs.read("config.txt")
        echo $content
    } else {
        echo "config.txt not found, using defaults"
    }
}
```

Some other behaviors were previously silent but are now **checked faults**: integer
overflow raises `E1010` (never wraps), integer division/modulo by zero raises
`E1011`, and an out-of-range (or negative) array/string index raises `E1012` carrying
the index and length — none of these crash the host. `int("abc")` still yields `0`, so
validate input where it matters.

## Common mistakes checklist

- **Spaces in bash-style assignment.** `port = 8080` is wrong; write `port=8080`. (The
  `let` form needs spaces: `let port = 8080`.)
- **Module calls use `module.function`,** e.g. `time.sleep(100)`, not `sleep(100)` -
  and the module must be imported with an alias first (`import "std::time" as time`).
- **Mismatched argument counts** trigger `E0003`.
- **Annotated `let` with the wrong literal type** triggers `E0004`.
- **Missing braces.** Every `if`, `for`, `while`, `fn`, and `spawn` body needs `{ }`.

## Tips

- **Fix from the top.** Earlier errors sometimes cause later ones.
- **Read the code, not just the message.** The code tells you the category; the
  location tells you where to look.
- **Check before you act at runtime.** Use `fs.exists` before `fs.read`, and validate
  values you convert with `int` / `float`.

Next: [Modules & Imports](13-modules-imports.md).
