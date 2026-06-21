# CLI Reference

The `ran` command is your entry point to everything: running programs, compiling
binaries, scaffolding projects, and experimenting in the REPL. This chapter documents
every command.

## Overview

```bash
ran <file.ran>            # run a file directly
ran run [file.ran]        # run the project (auto-detects the entry)
ran build [file.ran]      # compile to a standalone binary
ran test [file.ran]       # run all test_* functions
ran init [name]           # create a new project (src/ layout)
ran repl                  # start the interactive REPL
ran version               # print version info
ran help                  # show usage
```

If you run `ran` with no arguments, it prints usage and exits.

## Entry resolution (run / build / test)

When you don't pass a file, Ran finds the entry in this order:

1. An explicit `file.ran` argument, if given.
2. The `entry = "..."` field in `ran.toml` (may be any path/name, e.g.
   `server.ran` in the project root — it does **not** have to be `main.ran`).
3. Common locations: `src/main.ran`, then `main.ran`.
4. Otherwise, the first `*.ran` file (searched in `.` then `src/`) that defines
   `fn main(`.

`ran.toml` is **optional**. A standalone file runs and builds with no manifest.
A manifest is only created/updated by `ran build` when the project actually
uses dependencies (see below).

---

## ran <file.ran>

Run a `.ran` file directly. If the first argument ends in `.ran`, Ran runs it. The file
is checked by the strict analyzer first (see [12 - Error Handling](12-error-handling.md));
if it passes, it runs on the tree-walking interpreter.

```bash
ran hello.ran
ran examples/webserver.ran
```

---

## ran run [file.ran]

Run a file using the `run` subcommand. If you omit the filename, Ran looks for
`main.ran` in the current directory, then `src/main.ran`.

```bash
ran run app.ran      # run a specific file
ran run              # run ./main.ran (or ./src/main.ran)
```

This is convenient inside a project created by `ran init`. The file must have a `.ran`
extension, or Ran reports an error.

---

## --port <N> (global flag)

Run the program through a user-defined `fn port(p: int)` instead of `main()`, passing
the flag value as the argument. This is the current way to run the same program on a
different port without editing code. Works with `ran <file>`, `ran run`, and compiled
binaries.

```bash
ran site.ran --port 3000     # calls port(3000) instead of main()
ran run site.ran --port 8000 # calls port(8000)
./mysite --port 80           # compiled binary
```

`--port=3000` (with `=`) is also accepted. The recommended pattern keeps a default
port in `main()` and the flag-driven port in `port()`, sharing a `serve` helper:

```ran
import "std::http" as http

fn serve(p: int) {
    http.get("/", "home")
    http.server(p)
}

fn port(p: int) {   # entry point for --port N
    serve(p)
}

fn main() {         # entry point when no --port is given
    serve(8080)
}

fn home() { return "<h1>Hi</h1>" }
```

Rules:

- `fn port` must take **exactly one** parameter (the int port).
- If you pass `--port N` but never define `fn port`, Ran stops with:

```
error: --port requires a 'fn port(p: int)' function
  = help: define `fn port(p: int) { http.server(p) }`
```

> The old `RAN_PORT` environment override inside `http.server` was removed.

---

## ran build [file.ran] [-o <output>]

Compile a program into a standalone native binary. The source is compressed and
obfuscated and embedded, the binary is stripped. Entry resolution follows the
rules above, so `ran build` with no argument builds the project entry.

```bash
ran build                 # build the project entry (from ran.toml or detection)
ran build app.ran -o app  # build a specific file to ./app
./app
```

### Output name

- With `-o <name>`, the binary is written to `<name>`.
- Without `-o`, the name comes from `ran.toml` `[project].name`, else the entry
  file stem.

### Automatic ran.toml management

`ran build` scans the program's `import`s and keeps `ran.toml`'s
`[dependencies]` in sync:

- If `ran.toml` **exists**, missing std modules are appended.
- If it does **not** exist and you ran a **project build** (no explicit file),
  a minimal `ran.toml` is created with the detected entry + dependencies.
- If you built an **explicit one-off file** (`ran build x.ran`) and there's no
  `ran.toml`, none is created — the directory stays clean.

### What it prints (discrete, always-visible stages)

```
   Compiling shopapp (entry: server.ran)
     Parsing   server.ran
     Updated ran.toml +deps: decimal, log
    Checking  semantics (3 statements)
   Finishing  linking standalone binary
  Dependencies decimal, log
    Finished shopapp [optimized] 161 B src -> 1642.9 KB bin in 0.90s
       Built ./shopapp
```

### Requirements

The entry must contain a `main()` function. If not, the build stops with
`error: no main() function found`.

---

## ran test [file.ran]

Run every function named `test_*` in the entry (and its imported modules) and
report results. In test mode, a failing `assert(cond, "msg")` records the
failure instead of aborting, so all tests run.

```bash
ran test            # test the project entry
ran test app.ran    # test a specific file
```

```
running 2 tests
  ok   test_greeting
  FAIL test_math — expected 4
test result: FAILED. 1 passed; 1 failed
```

Exit code is `0` when all tests pass, `1` otherwise — suitable for CI gating.

---

## ran init [name]

Scaffold a new project. With a name, it creates a new directory; without one, it
initializes a project in the current directory.

```bash
ran init myapp
ran init            # initialize in the current directory
```

It creates this structure:

```
myapp/
+-- ran.toml          # project manifest (entry, deps auto-managed by `ran build`)
+-- src/
|   +-- main.ran      # entry point
|   +-- lib/          # local modules
+-- public/           # static files (for web apps)
+-- .gitignore
```

| File / dir | Purpose |
|------------|---------|
| `ran.toml` | Project manifest. `[build].entry` and `[dependencies]` are read and auto-managed. |
| `src/main.ran` | Program entry point |
| `src/lib/` | Local modules |
| `public/` | Static files served by the HTTP server |
| `.gitignore` | Pre-filled ignore rules |

The scaffolded `entry` is `src/main.ran`, but you can point `[build].entry` at
any file (e.g. a differently named file in the project root) and `ran
run/build/test` will honor it.

After scaffolding:

```bash
cd myapp
ran run
ran test
ran build
```

---

## ran repl

Start an interactive Read-Eval-Print Loop. You can type statements and expressions
directly, without wrapping them in `main()`.

```bash
ran repl
# Ran REPL v0.2.1
# Type expressions or statements. Type 'exit' or Ctrl+D to quit.
```

```
ran> echo "hello"
hello
ran> exit
Bye!
```

> Status note: the REPL does **not** persist state between lines. A variable or
> function you define on one line is not available on the next. Use it for one-off
> expressions and statements.

### REPL commands

| Command | Action |
|---------|--------|
| `exit` / `quit` | Leave the REPL (also `.exit`, `.quit`) |
| `help` / `.help` | Show REPL help |
| `.clear` | Clear the screen |
| Ctrl+D | Quit |

---

## ran version

Print version and tagline.

```bash
ran version
# ran v0.2.1
# The Ran Programming Language
# A self-hosted language for internal systems and business tooling.
# Engine: tree-walking interpreter
```

Aliases: `--version`, `-v`.

---

## ran help

Show usage and the list of available commands.

```bash
ran help
```

Aliases: `--help`, `-h`.

---

## Quick examples

```bash
# Scaffold, then run
ran init blog && cd blog && ran main.ran

# Run a script directly
ran script.ran

# Build and run a binary
ran build main.ran -o app && ./app

# Experiment quickly
ran repl
```

## Tips and gotchas

- **`.ran` extension is required** for files passed to `ran` and `ran run`.
- **`-o` precedes the output name**, e.g. `ran build app.ran -o app`.
- **Entry resolution**: explicit file → `ran.toml` `entry` → `src/main.ran` /
  `main.ran` → first `*.ran` with `fn main(`.
- **`ran.toml` is optional**; it's only created by `ran build` when a project
  actually uses dependencies.
- **A `main()` function is required to build.** Scripts you only `run` can rely
  on top-level statements.
- **Compiled binaries are self-contained** (they dynamically link system
  OpenSSL when TLS is used).

That's the whole CLI. Head back to the [Introduction](00-introduction.md) for the big
picture, or jump to the [Roadmap](16-roadmap.md) for feature status.
