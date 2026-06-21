# Security

When you compile a Ran program with `ran build`, your source code is **compressed and
encrypted** before it is embedded in the output binary. This chapter explains what that
protects, how it works, and - just as importantly - what it does not protect.

## The problem with embedded source

Many simple "compile to binary" tools just paste the original source into the
executable as plain text, so anyone can recover it:

```bash
strings ./app      # would reveal readable source
```

Ran avoids this. The source embedded in a compiled binary is encrypted, so a `strings`
dump or hex editor shows ciphertext, not your code.

## How source protection works

`ran build` performs these steps (see [09 - Compilation](09-compilation.md) for the
full pipeline):

1. **Compression.** The `.ran` source is compressed with a built-in LZ77 coder.
2. **Key derivation.** A 32-byte key is derived with an iterated SHA-256 KDF that runs
   **100,000 rounds**, making brute force expensive.
3. **Encryption.** The compressed bytes are encrypted with a **SHA-256 CTR-mode stream
   cipher** (the keystream is SHA-256 of `key || nonce || counter`, XORed with the
   data).
4. **Nonce.** A 16-byte nonce is taken from the first 16 bytes of `sha256(source)` and
   stored with the payload so the runtime can decrypt later.
5. **Stripping.** The base runtime binary is stripped of debug symbols.
6. **Embedding.** The ciphertext, nonce, size, and a magic marker are appended to the
   stripped binary.

The encryption and hashing are implemented in **pure Rust with no external
dependencies**. SHA-256 is a real FIPS 180-4 implementation. (Note: this is a
SHA-256-based CTR stream cipher - it is not ChaCha20 or AES.)

## The encrypted binary layout

```
[ stripped ran runtime ][ ciphertext ][ nonce: 16B ][ size: u64 LE ][ "RANENCv3" ]
```

When the binary runs, the embedded runtime detects the `RANENCv3` marker, reads the
size and nonce, decrypts and decompresses the source, and runs it. If the marker is
absent, the binary behaves as the normal Ran CLI.

## What is protected

- **Casual source recovery.** `strings`, `cat`, and hex viewers show ciphertext.
- **Copy-paste theft.** Someone cannot trivially lift your source out of the binary.
- **Symbol snooping.** Debug symbols are stripped from the binary.

```bash
ran build app.ran -o app
strings app | grep -i "fn main"
# (no output -- the source is encrypted)
```

## What is NOT protected

Be realistic about the threat model. This raises the bar against casual inspection; it
is **not** a guarantee against a determined, skilled attacker. In particular:

- **An attacker who has the `ran` toolchain.** The KDF salt and passphrase are
  **embedded in the toolchain itself** (they are constants in the compiler). Anyone with
  the same `ran` build can derive the same key and decrypt the payload. The protection
  is against casual inspection (`strings`, hexdump, decompilers), not against someone
  determined who has the toolchain.
- **Secrets baked into code.** Encrypting the source does **not** make it safe to
  hardcode API keys, passwords, or tokens. Treat the binary as eventually readable for
  the purposes of secret management.
- **Runtime behavior.** Anything your program does over the network or to the file
  system is still observable while it runs.

## Best practices

### Keep secrets out of the binary

Never hardcode credentials. Read them from the environment at runtime with `os.env`:

```ran
import "std::os" as os

fn main() {
    let api_key = os.env("API_KEY")
    if api_key == "" {
        echo "API_KEY not set"
        exit(1)
    }
    # use api_key without ever writing it in the source
}
```

### Validate untrusted input

If you build an HTTP server, treat request data (`req_body`, `query_*`, `param_*`) as
untrusted. Validate and sanitize before using it. See [07 - Networking](07-networking.md).

### Secure your web endpoints

- The built-in server has **no authentication**. Add your own auth and authorization in
  handlers for anything sensitive.
- CORS headers are applied by default (open policy). Review whether that fits your
  needs.
- Static file serving blocks directory traversal (`..`), but only serve files you
  intend to expose from `public/`.

### Don't rely on encryption for licensing or DRM

Source encryption deters casual copying. It is not a robust licensing or anti-tamper
system, especially since the key material ships in the toolchain. If you need strong
guarantees, keep that logic on a server you control.

## Summary

| Aspect | Status |
|--------|--------|
| Source readable via `strings` | No (compressed + encrypted) |
| Debug symbols present | No (stripped) |
| Cipher | SHA-256 CTR-mode stream cipher (pure Rust) |
| KDF | Iterated SHA-256, 100,000 rounds |
| Magic marker | `RANENCv3` |
| Safe to hardcode secrets | No - use `os.env` |
| Resistant to casual inspection | Yes |
| Resistant to an attacker with the `ran` toolchain | No (key material is embedded) |
| Third-party crypto dependencies | None (pure Rust) |

Source encryption is a sensible default that protects your code from casual recovery.
Pair it with good secret hygiene (`os.env`) and server-side controls for anything that
genuinely needs to stay private.

Next: [CLI Reference](15-cli-reference.md).
