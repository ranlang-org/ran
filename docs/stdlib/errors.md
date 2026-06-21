# Error Code Reference

Ran reports problems in a precise, structured format: a severity, an error code,
the exact `file:line:column`, the offending source line with an underline, and a
`help:` line telling you how to fix it.

```
error[E0001]: undefined variable: `naem`
  --> app.ran:4:16
  |
4 |     echo "hi $naem"
  |                ^ not found in this scope
  = help: declare it first: `let naem = ...` or `naem="..."`
```

Errors are grouped by phase. **Compile-time** errors (E0xxx) stop the program
before it runs. **Runtime** errors (E1xxx) abort during execution with a
non-zero exit code.

## Compile-time: semantic (E000x)

| Code | Meaning | How to fix |
|------|---------|-----------|
| `E0001` | Undefined variable | Declare it (`let x = ...` or `x="..."`) before use, or check spelling. |
| `E0002` | Undefined function | Define it with `fn`, or import the module that provides it. |
| `E0003` | Wrong number of arguments | Call the function with the exact arity it declares. |
| `E0004` | Type mismatch vs annotation | Fix the value or change the type annotation. |
| `E0005` | Stdlib import missing alias | Write `import "std::http" as http`. |

## Compile-time: syntax / parser (E01xx)

| Code | Meaning | How to fix |
|------|---------|-----------|
| `E0100` | Expected a specific token, found another | Insert the missing token shown in the message (`{`, `)`, `,`, ...). |
| `E0101` | Unexpected token where a value was expected | Provide a value: literal, variable, `(...)`, `[...]`, or a call. |
| `E0102` | Expected a statement | Start the line with a keyword (`fn`, `let`, `if`, ...), an assignment, or an expression. |

The parser collects **all** syntax errors it can find, prints them, and then
aborts with `aborting due to N errors emitted`. A program with any compile-time
error never reaches the runtime.

## Runtime (E1xxx)

| Code | Meaning | How to fix |
|------|---------|-----------|
| `E1001` | Integer overflow (`+`, `-`, `*` exceeded 64-bit range) | Use smaller values, or use `decimal` for large/exact values; integers are `i64`. |
| `E1002` | Division or modulo by zero (int or decimal) | Guard the divisor: `if d != 0 { a / d }`. |
| `E1003` | Decimal overflow (exceeded 128-bit precision) | Reduce scale or operand magnitude. |
| `E1004` | Invalid decimal construction (`dec("abc")`) | Pass a valid numeric string, int, or float. |
| `E1005` | Required env var missing (`env.require`) | Set the variable, or load it from a `.env` file. |
| `E1006` | Out of memory: free RAM fell below the runtime safety floor inside a loop | Reduce memory held across the loop (avoid building very large arrays); the process stops itself before the OS OOM-killer would. |

Runtime errors exit with code `70` (`EX_SOFTWARE`). Compile-time errors exit
with code `1`.

## Faults inside an HTTP handler

When a runtime fault (any `E1xxx`) happens **inside an HTTP request handler**,
it does **not** terminate the server. The request returns `500 Internal Server
Error`, the fault is logged to stderr, and the server keeps serving other
requests. Outside a handler (top-level scripts), a fault prints the diagnostic
and exits with code `70` as above.

> `exit(...)`, `os.exit(...)`, and `log.fatal(...)` are explicit process
> terminations and are **not** caught — use them only for intentional shutdown.

## Exit codes summary

| Exit code | Cause |
|-----------|-------|
| `0` | Success |
| `1` | Compile-time error, or `fatal`/`assert` failure |
| `70` | Runtime fault (E1xxx) |
| custom | Whatever you pass to `exit(n)` / `os.exit(n)` |
