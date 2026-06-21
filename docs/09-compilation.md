# Compilation

`ran build` turns your `.ran` program into a single, standalone native binary. The
output runs anywhere compatible with **no dependencies** - no Ran install, no separate
runtime, and no external compiler toolchain.

Important: `ran build` does **not** compile to machine code or bytecode. It embeds your
source (compressed and encrypted) into a stripped copy of the `ran` runtime binary.
When the binary runs, it decodes that source and runs it with the same tree-walking
interpreter as `ran run`.

## Building a binary

```bash
ran build app.ran -o myapp
./myapp
```

If you omit `-o`, the output name defaults to the source filename without its
extension:

```bash
ran build server.ran
# produces ./server
```

Your program must contain a `main()` function, or the build stops with an error.

## What you get

| Property | Value |
|----------|-------|
| External dependencies | None |
| Output format | Native ELF executable |
| Debug symbols | Stripped |
| Embedded source | Compressed and encrypted |
| Startup | Fast (decode + interpret) |

```bash
file myapp
# ELF 64-bit LSB executable, x86-64, stripped
```

## Build output & artifact dumps

`ran build` shows a concise, animated progress line (a live spinner with elapsed
time) and a final summary with the total compile time:

```
  ◆ Compiling blog (entry: app.ran)
  ⠹ Compiling blog [0.4s]          ← live spinner while linking (on a TTY)
  ✓ Finished blog [optimized] 1178 B src → 986 KB bin in 0.13s
    ✓ Built ./blog
  ▸ Standalone runs on another machine with no `ran` install
```

Add **`--debug`** for a detailed, per-stage log and a full artifact dump under
`./debug/`:

```
ran build --debug
  ◆ Compiling blog (entry: app.ran)
  Platform  linux x86_64 · 8 CPU · ...
  ▸ Lexing    app.ran → 1065 tokens in 0.001s [+0.004s]
  ▸ Parsing   21 statements, no syntax errors [+0.004s]
  ▸ Checking  16 fn · 0 struct · 0 enum · ownership=warn [+0.004s]
  ▸ Emitting  debug/blog.{tokens,ast,check,bc}.txt [+0.009s]
  ✓ Finished blog [optimized] ... in 0.13s
    ▸ Dump  ./debug/blog.{tokens,ast,check,bc}.txt
```

| Dump (`./debug/`, `--debug` only) | Contents |
|------|----------|
| `<name>.tokens.txt` | the full token stream from the lexer |
| `<name>.ast.txt` | the parsed AST (merged across imports) |
| `<name>.check.txt` | analysis summary: counts, ownership mode, dependencies, functions |
| `<name>.bc.txt` | disassembled bytecode (experimental VM target) |

## Truly standalone

The output binary embeds the runtime, so it needs **no `ran` install and no source
files** on the target. You can verify this with an empty environment:

```bash
env -i ./myapp        # no PATH, no `ran`, nothing — still runs
```

Caveat: a program that uses TLS (`http` client) or the `db` module dynamically
links the system OpenSSL/SQLite library, so the target needs those `.so` files
unless you statically link them (roadmap). A pure-compute program needs only
libc.

## Experimental: run on the bytecode VM (`--vm`)

`ran <file> --vm` runs the program on the experimental bytecode VM instead of the
tree-walking interpreter. The VM is bounded (step budget + stack cap) so it can
never loop forever or leak; when it meets a construct it does not yet implement
(or a `fn main()` program, which it cannot enter yet) it prints a note and falls
back to the interpreter. It currently runs simple top-level scripts; completing
the VM, then native code generation, is the path on the bootstrap roadmap.



1. **Locate the runtime.** Ran reads its own executable to use as the base runtime.
2. **Clean the base.** Any previously embedded payload is stripped off to get a clean
   runtime image.
3. **Strip symbols.** Debug symbols are removed using the external `strip` tool.
4. **Compress your source.** The `.ran` source is compressed with a built-in LZ77 coder
   (window 4096, minimum match 4, maximum match length 255).
5. **Encrypt it.** The compressed bytes are encrypted with a SHA-256 CTR-mode stream
   cipher. The key is derived with an iterated SHA-256 KDF (100,000 rounds). The nonce
   is the first 16 bytes of `sha256(source)`.
6. **Append the payload.** The ciphertext plus metadata is appended to the stripped
   runtime.

The result is one self-contained executable.

## Binary layout

```
[ stripped ran runtime ][ ciphertext ][ nonce: 16B ][ size: u64 LE ][ "RANENCv3" ]
```

- The last 8 bytes are the magic marker `RANENCv3`.
- The 8 bytes before that hold the ciphertext size (little-endian `u64`).
- Before the size is a 16-byte nonce.
- Before the nonce is the encrypted (and compressed) source.

## What happens when you run it

When you launch a compiled binary, the embedded runtime:

1. Checks the tail of its own file for the `RANENCv3` magic marker.
2. If present, reads the size and nonce, locates the ciphertext, decrypts it, and
   decompresses it back to the original source.
3. Runs the recovered program with the interpreter.

If the marker is absent (for example, the plain `ran` tool itself), the binary behaves
as the normal Ran CLI instead.

## Why the source is protected

A naive "compile to binary" tool embeds the source as plain text, so anyone can recover
it with `strings` or a hex editor. Ran compresses and encrypts the embedded source so:

- `strings myapp` does **not** reveal your code.
- A hex dump shows ciphertext, not readable Ran.
- Casual inspection and copy-paste theft are prevented.

For the full threat model - including what this does **not** protect against - see
[14 - Security](14-security.md).

## Distributing your binary

Because the output is self-contained, distribution is a file copy:

```bash
ran build app.ran -o app
scp app user@server:/usr/local/bin/
# runs on the server with nothing else installed
```

## A complete example

```ran
# greet.ran
name="World"

fn main() {
    echo "Hello, $name!"
    echo "Built with ran build"
}
```

```bash
ran build greet.ran -o greet
# ran: compiling greet.ran -> greet
#      source: 84 bytes
#      binary: ... bytes (... KB)
#   ok compiled -> greet

./greet
# Hello, World!
# Built with ran build

strings greet | grep "Hello"
# (no output -- the source is encrypted)
```

## Tips and gotchas

- **A `main()` is required to build.** Scripts you only `run` may rely on top-level
  statements, but `ran build` needs an entry point.
- **`-o` sets the output name.** Without it, the binary is named after the source file.
- **Binaries are platform-native.** A binary built on Linux x86_64 runs on Linux
  x86_64. Cross-compilation is planned, not yet available.
- **The runtime is embedded.** That is why the output is self-contained.
- **Don't rely on encryption for secrets.** Read configuration from the environment
  with `os.env` (see [10 - Standard Library](10-stdlib.md) and
  [14 - Security](14-security.md)).

Next: [Standard Library](10-stdlib.md).
