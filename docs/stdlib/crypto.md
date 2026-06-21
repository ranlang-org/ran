# `crypto` — Hashing & Encoding

```ran
import "std::crypto" as crypto
```

Cryptographic hashing and common encodings, built on an in-tree SHA-256
(FIPS 180-4) with no external dependencies.

| Call | Returns | Description |
|------|---------|-------------|
| `crypto.sha256(s)` | str | SHA-256 of `s`, lowercase hex |
| `crypto.sha256_hex(s)` | str | Alias of `sha256` |
| `crypto.hmac_sha256(key, msg)` | str | HMAC-SHA256 (RFC 2104), lowercase hex |
| `crypto.hex(s)` | str | Hex-encode the bytes of `s` |
| `crypto.base64(s)` | str | Base64-encode (RFC 4648, `=` padded) |
| `crypto.base64_decode(s)` | str | Base64-decode (`void` if invalid) |

## Example

```ran
import "std::crypto" as crypto

fn main() {
    echo crypto.sha256("abc")
    # ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad

    # Sign a payload with a shared secret (e.g. webhook / API token check)
    let sig = crypto.hmac_sha256("my-secret-key", "payload-to-sign")
    echo sig

    echo crypto.base64("hello")          # aGVsbG8=
    echo crypto.base64_decode("aGVsbG8=") # hello
}
```

## Verifying a signature (constant idea)

```ran
import "std::crypto" as crypto

fn valid(body, provided_sig) -> bool {
    let expected = crypto.hmac_sha256("shared-secret", body)
    return expected == provided_sig
}
```

> Compare full hex strings (as above). A timing-safe comparison primitive is a
> future addition; for internal services the string compare is acceptable.

## What this is for

- **Integrity / fingerprints**: hash file contents, dedupe, ETags.
- **Signing & verification**: HMAC for webhooks, API tokens, signed cookies.
- **Encoding**: Base64/hex for transport and storage.

## What this is NOT (yet)

- **Not a password hasher.** SHA-256/HMAC are fast hashes; for passwords use a
  slow KDF (bcrypt/argon2-style). Not yet provided — do not store raw SHA-256 of
  passwords.
- **Not random.** Secure random bytes/tokens (CSPRNG) are a separate, planned
  addition; `rand` is not cryptographic.
- **Not encryption.** There is no public symmetric/asymmetric encryption API.
  (The internal cipher used by `ran build` is obfuscation only — see
  [../14-security.md](../14-security.md).)

These hashing primitives are exactly the ones that are standard, well-defined,
and safe to expose; the riskier "roll-your-own" pieces are deliberately left
out until they can be done correctly.
