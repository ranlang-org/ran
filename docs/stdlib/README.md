# Ran Standard Library Reference

The Ran standard library is built into the runtime — no external packages, no
dependency resolver, no version skew. Every module documented here is available
in `ran run`, the REPL, and compiled binaries.

## Importing

Stdlib modules require an explicit alias (enforced by error `E0005`):

```ran
import "std::log" as log
import "std::http" as http
import "std::json" as json
```

You then call methods on the alias: `log.info("hello")`, `json.encode(data)`.

## Module index

| Module | Purpose | Page |
|--------|---------|------|
| `decimal` | **Exact money & business math** | [decimal.md](decimal.md) |
| `crypto` | Hashing (SHA-256, HMAC) & encoding | [crypto.md](crypto.md) |
| `env`  | Environment & `.env` configuration | [env.md](env.md) |
| `log`  | Leveled structured logging | [log.md](log.md) |
| `http` | HTTP server + client (TLS) | [http.md](http.md) |
| `web`  | Native HTML/CSS/asset serving (SPA, cache, build hook) | [web.md](web.md) |
| `db`   | Embedded SQLite (queries, transactions, money) | [database.md](database.md) |
| `concurrency` | Threads, channels, wait groups, shared state | [concurrency.md](concurrency.md) |
| `fs`   | Filesystem access | [fs.md](fs.md) |
| `os`   | Process & environment | [os.md](os.md) |
| `time` | Clocks & timestamps | [time.md](time.md) |
| `json` | JSON encode/decode | [json.md](json.md) |
| `str`  | String utilities | [str.md](str.md) |
| `math` | Numeric functions | [math.md](math.md) |
| `rand` | Pseudo-random numbers | [rand.md](rand.md) |
| `html` | Template rendering | [html.md](html.md) |

Built-in functions (no import) and value methods: [builtins.md](builtins.md).
Structs, methods & OOP: [oop.md](oop.md).
Native web serving (HTML/CSS/assets): [web.md](web.md).
Database connectivity (SQLite): [database.md](database.md).
Concurrency (threads, channels, wait groups, shared state): [concurrency.md](concurrency.md).
Worked examples — banking & e-commerce: [bank-and-ecommerce.md](bank-and-ecommerce.md).
Compiler & runtime error codes: [errors.md](errors.md).
Large/enterprise project guide: [enterprise.md](enterprise.md).

## Using the stdlib in large / enterprise projects

Ran is designed to be self-hosted and stable. Recommendations for production
codebases:

1. **Project layout.** Use `ran init` to scaffold `main.ran`, `lib/`, and
   `public/`. Keep reusable code in `lib/` and import with relative paths:
   `import "./lib/users" as users`. One file = one module.

2. **Configuration via environment.** Read config with `os.env_or(key, default)`
   so the same binary runs across environments without recompiling.

3. **Structured logging.** Prefer `log.info/warn/error` over `echo` for servers.
   Log lines go to **stderr** with an ISO-8601 UTC timestamp, leaving stdout
   clean for program output or piping.

4. **Networking.** The `http` server is multi-threaded with a bounded worker
   pool (tune with `RAN_WORKERS`). Bind address and port are configurable — see
   [http.md](http.md) and the security notes below.

5. **Error handling.** The static checker rejects undefined names, arity errors,
   and type mismatches before running (see [errors.md](errors.md)). Runtime
   faults (overflow `E1001`, divide-by-zero `E1002`) abort with a clear message
   and a non-zero exit code, so failures are loud, not silent.

6. **Determinism in CI.** Builds and runs are std-only and reproducible. Pin the
   `ran` binary version your team uses and check it into your toolchain.

## Security posture (read before deploying)

- The HTTP **client** verifies certificates and hostnames over TLS (system
  OpenSSL); outbound `https://` is supported. The HTTP/web **server** is
  unauthenticated and does **not** terminate TLS yet — put auth and TLS
  termination in front of it.
- `ran build` **obfuscates** embedded source; it is **not** secure encryption.
  The key ships in the binary. Treat compiled binaries as readable by a
  determined party. See [../14-security.md](../14-security.md).
- Static file serving rejects path traversal and resolves paths within the
  served directory only.

## Stability & versioning

Function signatures listed here are considered stable for the current `0.x`
series. Breaking changes are recorded in [../20-changelog.md](../20-changelog.md).
