# Introduction to Ran

> **Internal / personal use.** Ran is built and maintained for private, in-house
> use — it is not a general-purpose public product and makes no stability
> promises to outside users.

**Ran** is a small, self-hosted programming language for internal systems and
business tooling. It pairs low-ceremony syntax with a strict pre-run checker,
exact decimal math for money, and a built-in HTTP server and TLS client. You
write a `.ran` file, run it with the `ran` interpreter, and when you are ready,
compile it into a single standalone binary with `ran build`.

```ran
name="World"

fn main() {
    echo "Hello, $name!"
}
```

```bash
ran hello.ran
# Hello, World!
```

> Status: Ran is v0.2.5 and under active development. These docs describe what works
> **today**. Anything partial or planned is labelled with a "Status" note. The full
> status list is in [16 - Roadmap](16-roadmap.md), and the feature summary is in the
> [20 - Changelog](20-changelog.md). Version 1.0.0 is reserved for the fully
> self-hosted release (the Ran compiler written in Ran).

## How Ran runs your code

Ran uses a **tree-walking interpreter** for everything: `ran run`, `ran <file>`, and
compiled binaries (which decode their embedded source and interpret it). A bytecode
VM exists in the source tree but is not wired into the pipeline and does not run your
programs. There is no engine-selection flag.

`ran build` packages your program into one native binary by embedding the source
(compressed and encrypted) into a stripped copy of the `ran` runtime. There is no
separate compiler toolchain to install.

## Philosophy

1. **Scripts should grow into programs.** Start with a few bash-style lines, then add
   functions, types, and structure.
2. **The checker is strict.** Ran reports undefined names, wrong argument counts, and
   simple type mismatches before the program runs, all at once, with `error[E000N]`
   codes.
3. **Batteries included.** HTTP server, file system, JSON, time, OS access, and math
   ship in the standard library.
4. **Ship one file.** `ran build` produces a single native binary with the source
   embedded (compressed and encrypted) so it is not trivially readable.
5. **Concurrency without ceremony.** Wrap work in `spawn { }` to run it on a thread.

## What works today (at a glance)

- Variables (bash-style and `let` / `let mut`), int and float values, strings, bools,
  arrays, and maps.
- Functions with recursion, `if`/`else`, `for` over arrays (and `for i in range(n)`),
  and `while`.
- Integer and float math and comparisons, mixed int/float arithmetic, string
  comparisons, and `&&` / `||` / `!`; string concatenation and methods.
- `spawn { }` threads.
- A built-in HTTP server with routing, path/query params, cookies, CORS, and static
  files.
- A standard library: `http`, `time`, `fs`, `json`, `os`, `math`, `html`, `str`, and
  `rand` (each imported with a mandatory alias, e.g. `import "std::time" as time`).
- Built-in helpers `range`, `keys`, `values`, `abs`, and `assert`.
- Local file imports (`import "./mathlib"`).

## What is partial or planned

- **Partial:** `html.render` (variable interpolation only).
- **Cosmetic today:** ownership / borrowing (`&`, `&mut`, `*` parse but do nothing).
- **Note:** `&&` / `||` work but are not short-circuit (both sides evaluate).
- **Planned:** structs / enums / `match`, closures, channels, the hardware bindings,
  HTTP middleware, remote packages, and activating the bytecode VM.

See [16 - Roadmap](16-roadmap.md) for the complete breakdown.

## When to use Ran

Ran is a good fit for small web services and APIs, CLI tools and automation, and
scripts you want to distribute as a single binary. It is less suited (today) for large
applications that need a rich third-party ecosystem, heavy numeric computing, or
platforms Ran does not target.

## A slightly bigger taste

```ran
# Functions, control flow, and the stdlib working together

import "std::time" as time

fn classify(n: int) -> str {
    if n < 0 {
        return "negative"
    } else {
        if n == 0 {
            return "zero"
        } else {
            return "positive"
        }
    }
}

fn main() {
    let numbers = [-3, 0, 7]
    for n in numbers {
        let label = classify(n)
        echo "$n is $label"
    }

    let now = time.now()
    echo "Time now: $now"
}
```

Note the last two lines: string interpolation only substitutes plain variable names,
so compute `time.now()` into a variable first, then interpolate `$now`. See
[02 - Variables & Types](02-variables-types.md) for the details.

## Documentation

| Doc | Topic |
|-----|-------|
| [00 - Introduction](00-introduction.md) | This page: what Ran is and how it runs |
| [01 - Getting Started](01-getting-started.md) | Install Ran and run your first program |
| [02 - Variables & Types](02-variables-types.md) | Values, types, and string interpolation |
| [03 - Functions](03-functions.md) | Define, call, and return from functions |
| [04 - Control Flow](04-control-flow.md) | `if`/`else`, `for`, `while` |
| [05 - Ownership](05-ownership.md) | Enforced ownership & borrow checking (`warn`/`strict`) |
| [06 - Concurrency](06-concurrency.md) | Threads, join, channels, wait groups, shared state |
| [07 - Networking](07-networking.md) | The built-in HTTP server |
| [08 - Hardware](08-hardware.md) | Hardware module status (library-only) |
| [09 - Compilation](09-compilation.md) | `ran build` and the binary format |
| [10 - Standard Library](10-stdlib.md) | Built-in functions, methods, and modules |
| [11 - Syntax Reference](11-syntax-reference.md) | Compact syntax cheat sheet |
| [12 - Error Handling](12-error-handling.md) | Reading and fixing checker errors |
| [13 - Modules & Imports](13-modules-imports.md) | Local file imports and layout |
| [14 - Security](14-security.md) | What source encryption does and does not do |
| [15 - CLI Reference](15-cli-reference.md) | Every `ran` command |
| [16 - Roadmap](16-roadmap.md) | Working / partial / planned status |
| [17 - Building Websites](17-building-websites.md) | Routing, params, --port, static files |
| [18 - Interop & Ecosystem](18-interop-and-ecosystem.md) | Native interop reality and the ecosystem plan |
| [19 - Why Ran](19-why-ran.md) | Feature highlights and design rationale |
| [20 - Changelog](20-changelog.md) | Versioning policy and current feature summary |
| [25 - Language Spec](25-language-spec.md) | Working language specification (for self-hosting) |

Welcome to Ran. Let's build something.
