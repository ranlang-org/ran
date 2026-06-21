# Dependency Policy

Ran is built for self-hosted, enterprise systems that handle real money. The
dependency policy exists to keep the toolchain auditable, reproducible, and
free of supply-chain surprises.

## Principle: zero third-party crates by default

The compiler, runtime, and standard library depend **only on the Rust standard
library**. `Cargo.toml` has an empty `[dependencies]` section, and it stays that
way unless a dependency clears the bar below.

Why:

- **Auditability.** The entire system can be read and reviewed by your team.
- **Reproducibility.** No registry, no lockfile drift, no yanked crates.
- **Supply-chain safety.** No transitive dependencies you didn't choose. This
  matters most for money-handling code.
- **Longevity.** The toolchain keeps building years from now without chasing
  upstream churn.

## What this means in practice

Capabilities are implemented in-tree, including:

- SHA-256, a stream cipher, and LZ compression (`support/crypto`)
- Exact decimal arithmetic (`support/decimal`)
- The HTTP/1.1 server and client (`stdlib/net`)
- JSON encode/decode, templating, PRNG

## The bar for adding a dependency

A third-party crate may be considered **only if all** of the following hold,
and only with explicit sign-off recorded in this file:

1. The capability is security-critical and genuinely impractical to implement
   correctly in-house (the canonical example: a **TLS** stack — do not roll your
   own TLS).
2. The crate is widely used, actively maintained, and has a clean audit history.
3. Its transitive dependency tree is small and itself vettable.
4. The version is **pinned exactly** (`=x.y.z`), not a range.
5. A named owner on your team is responsible for tracking its advisories.

### Known candidates (not yet adopted)

| Need | Status | Note |
|------|--------|------|
| TLS / HTTPS (server + client) | Under consideration | The one case where a vetted crate (e.g. `rustls`) is likely justified. Until then, terminate TLS at a local reverse proxy. |
| CSPRNG | Under consideration | For tokens/keys. `rand` today is **not** cryptographically secure. The OS RNG can also be read directly from `/dev/urandom` with std only. |

## Operational rules

- Run `ran build` in CI as a gate; it fails on any compile-time error.
- Pin and distribute one `ran` binary version across the team.
- Record any approved dependency, its version, and its owner in this document
  before it is merged.

## Approved dependencies

**System libraries (linked via FFI, not cargo crates):**

| Library | Use | Linked by | Verification |
|---------|-----|-----------|--------------|
| OpenSSL (`libssl`/`libcrypto`) | TLS for the HTTPS client (and future DB TLS) | `build.rs` (`-lssl -lcrypto`), FFI in `src/support/tls.rs` | Verified against live HTTPS (valid cert → 200; expired cert → rejected) |
| SQLite3 (`libsqlite3`) | Native SQLite database (Kelompok B, module `db`) | `build.rs` (`-lsqlite3`), FFI in `src/support/sqlite_ffi.rs` | Availability checked at runtime on `db.connect` (`E0501`/`E0502` diagnostics) |

Each library has a named owner responsible for tracking its advisories
(currently the runtime/FFI maintainer, alongside OpenSSL).

Rationale: the user authorized OpenSSL for secure connections. Linking the
system library via FFI uses a battle-tested TLS stack while keeping the project
dependency manifest empty (no third-party package supply chain). Certificate
**and** hostname verification are enforced in the client. The same reasoning
extends to SQLite3: linking the system library via FFI reuses a mature, widely
audited native database implementation while the dependency manifest stays
empty.

Native web delivery (Kelompok A) adds **no** system library: the built-in web
server serves built web assets (markup, stylesheets, client scripts, and static
files) using the standard library only — no embedded engine is linked.

### Linking mode

By default the system library above is **dynamically linked** (the build emits
`-lsqlite3` and resolves the shared object at load time). An optional
`--link-static` build mode instead links the static archive (`libsqlite3.a`)
when present, producing a fully self-contained binary for distribution to
machines that may not have the shared library installed.

**Third-party packages:** _None._ The dependency manifest remains empty. Adding
SQLite3 as an FFI-linked system library does **not** introduce any package-registry
dependency.
