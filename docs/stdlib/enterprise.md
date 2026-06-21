# Building Large / Enterprise Projects with Ran

This guide collects patterns for using Ran and its standard library in
production, self-hosted services.

## 1. Project structure

```
myservice/
├── ran.toml          # project manifest (ran init)
├── main.ran          # entry point: fn main()
├── lib/
│   ├── config.ran    # configuration module
│   ├── users.ran     # domain logic
│   └── handlers.ran  # HTTP handlers
└── public/           # static assets served by http
```

One file = one module. Import local modules by relative path:

```ran
import "./lib/config" as config
import "./lib/handlers" as handlers
```

Local modules are merged at load time; stdlib imports stay native.

## 2. Configuration

Read all configuration from the environment so a single compiled binary is
promotable across dev → staging → prod:

```ran
import "std::os" as os

fn load_config() -> map {
    let c = map()
    set(c, "port", os.env_or("PORT", "8080"))
    set(c, "env", os.env_or("APP_ENV", "development"))
    set(c, "log_level", os.env_or("LOG_LEVEL", "info"))
    return c
}
```

## 3. Logging & observability

- Use `log.info/warn/error`; logs go to **stderr** with UTC timestamps.
- Keep stdout for program output / health endpoints.
- Aggregate by redirecting stderr (fish): `./myservice 2> /var/log/myservice.log`

```ran
import "std::log" as log
log.info("request", $req_method, $req_path)
```

## 4. HTTP services

```ran
import "std::http" as http

fn health() -> str { return "{\"status\":\"ok\"}" }

fn main() {
    http.get("/health", "health")
    http.get("/users/:id", "get_user")
    http.server(8080)
}
```

Operational knobs:

| Concern | Lever |
|---------|-------|
| Bind only to localhost | `set -x RAN_HOST 127.0.0.1` |
| Worker pool size | `set -x RAN_WORKERS 64` |
| Port from CLI | `ran main.ran --port 9090` (needs `fn port(p)`) |

The server has **no built-in auth or TLS**. Terminate TLS and authenticate at a
reverse proxy (nginx/Caddy) in front of the service.

## 5. Calling other services

```ran
import "std::http" as http
import "std::log" as log

fn fetch_orders() -> str {
    let r = http.request("GET", "http://orders.internal:8080/orders", "")
    if !r["ok"] {
        log.error("orders fetch failed", r["status"], r["error"])
        return "[]"
    }
    return r["body"]
}
```

For external `https://` dependencies, route through an internal proxy — the
std-only client is plaintext-only.

## 6. Concurrency

Use `spawn` for background work; the runtime joins spawned tasks before exit.

```ran
fn worker() { echo "doing work" }

fn main() {
    spawn { worker() }
    spawn { worker() }
    echo "main continues"
}
```

## 7. Failure handling

- Compile-time: undefined names, arity, type, and syntax errors stop the build
  ([errors.md](errors.md)). Run `ran build` in CI to gate merges.
- Runtime: overflow (`E1001`) and divide-by-zero (`E1002`) abort loudly with
  exit code `70` — failures are never silent.
- Use `assert(cond, "message")` for invariants and `log.fatal(...)` for
  unrecoverable startup conditions.

## 8. Build & deploy

```fish
ran build main.ran -o myservice
./myservice
```

The compiled binary is standalone (interpreter + obfuscated source embedded).
Note: this is **obfuscation, not encryption** — do not rely on it to protect
secrets. Keep credentials in the environment, never in source.

## 9. CI checklist

1. `ran test` — runs the project's `test_*` functions.
2. `ran build` each service entry point — fails on any compile-time error.
3. Run a smoke test that hits `/health`.
4. Pin and distribute one `ran` binary version across the team.

## Current limitations to plan around

| Area | Status | Mitigation |
|------|--------|------------|
| Inbound TLS (HTTPS server) | Not yet wired | Terminate TLS at a reverse proxy |
| Outbound TLS (HTTPS client) | Supported (system OpenSSL) | Verified cert + hostname |
| Source protection | Obfuscation only | Keep secrets in env |
| CSPRNG | `rand` is not secure | Avoid for tokens/keys |
| HTML auto-escaping | Manual | Escape untrusted input |
| Execution speed | Tree-walking interpreter | Native/VM codegen is on the roadmap |
