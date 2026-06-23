# Getting Started

This guide gets Ran installed and walks you from your first line of code to a compiled
binary.

## Installation

### From source

Ran is bootstrapped with the Rust toolchain. You only need Rust for the initial build;
once compiled, the `ran` binary is self-contained.

```bash
git clone https://github.com/ranlang-org/ran
cd ran
cargo build --release

# Put it on your PATH
cp target/release/ran ~/.local/bin/
```

### Verify the install

```bash
ran version
# ran v0.3.9
# The Ran Programming Language
# A self-hosted language for internal systems and business tooling.
# Engine: bytecode VM (default) with tree-walking interpreter fallback
```

If you see the version banner, you are ready to go.

## Your first program

Create a file called `hello.ran`:

```ran
name="World"

fn main() {
    echo "Hello, $name!"
}
```

Run it directly:

```bash
ran hello.ran
# Hello, World!
```

That's the whole loop: write a `.ran` file, run it with `ran`.

## The `.ran` extension

All Ran source files use the `.ran` extension. The file you run is the program's main
module.

## Program entry point

Every Ran program starts at the `main()` function. Top-level statements (like variable
declarations) run **before** `main()`, which makes them handy for configuration.

```ran
# This runs first (top-level)
config="production"
port=8080

# This is the entry point
fn main() {
    echo "Mode: $config"
    echo "Port: $port"
}
```

```bash
ran app.ran
# Mode: production
# Port: 8080
```

## Comments

Ran supports several comment styles. Use whichever you prefer:

```ran
# bash-style line comment
// C++ style line comment
name="Ran"   // inline comment

/* C-style block comment.
   Block comments can be nested. */
```

A line whose first non-whitespace character is `;` is also a comment, which makes it
easy to disable a line: `;echo "skipped"` and `; echo "skipped"` both do nothing. See
[11 - Syntax Reference](11-syntax-reference.md) for the full rules.

## Ways to run a program

```bash
# 1. Run a file directly
ran hello.ran

# 2. Use the explicit "run" subcommand
ran run hello.ran

# 3. Run the default file in the current directory (no filename)
ran run
```

The third form looks for `main.ran` first, then `src/main.ran`.

## Starting a new project

Use `ran init` to scaffold a project layout:

```bash
ran init myapp
cd myapp
ran main.ran
```

`ran init` creates:

```
myapp/
+-- ran.toml      # project manifest (scaffolding only - see note)
+-- main.ran      # entry point
+-- public/       # static files (for web apps)
+-- lib/          # local modules
+-- .gitignore
```

If you run `ran init` with no name, it initializes a project in the current directory.

> Status note: `ran.toml` is created for you, but nothing reads it yet. It is a
> placeholder for a future package/build manifest. Your entry point is determined by
> the file you run, not by `ran.toml`.

## Trying things in the REPL

For quick experiments, start the interactive REPL:

```bash
ran repl
# Ran REPL v0.3.9
# Type expressions or statements. Type 'exit' or Ctrl+D to quit.
```

You can type statements directly without wrapping them in `main()`:

```
ran> echo "hello from the repl"
hello from the repl
ran> exit
Bye!
```

> Status note: the REPL does **not** persist state between lines. Each line is checked
> and executed on its own, so a variable or function defined on one line is not visible
> on the next. Use it for one-off expressions and statements. REPL commands: `exit` /
> `quit` (also `.exit` / `.quit`), `help` / `.help`, and `.clear`.

## Compiling to a binary

When your program is ready to ship, compile it to a standalone native binary:

```bash
ran build hello.ran -o hello
./hello
# Hello, World!
```

The output binary:

- Has **no external dependencies** - copy it anywhere compatible and run it.
- Is **stripped** of debug symbols.
- Carries its **source compressed and encrypted** inside, so it is not recoverable
  with `strings` or a hex dump.

`ran build` requires your program to contain a `main()` function, or it stops with an
error. If you omit `-o`, the output name defaults to the source filename without its
extension:

```bash
ran build server.ran
# produces ./server
```

See [09 - Compilation](09-compilation.md) and [14 - Security](14-security.md) for how
this works.

## A quick checklist

You now know how to:

- Install and verify Ran
- Write and run a `.ran` file
- Use top-level config and a `main()` entry point
- Scaffold a project with `ran init`
- Experiment in the REPL (no state persists)
- Compile to a standalone binary

Next up: [Variables & Types](02-variables-types.md).
