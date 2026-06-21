# `http` — HTTP Server & Client

```ran
import "std::http" as http
```

The `http` module provides both a multi-threaded server and a minimal client.

---

## Server

| Call | Description |
|------|-------------|
| `http.get(path, handler_name)` | Register a GET route |
| `http.post(path, handler_name)` | Register a POST route |
| `http.put(path, handler_name)` | Register a PUT route |
| `http.patch(path, handler_name)` | Register a PATCH route |
| `http.delete(path, handler_name)` | Register a DELETE route |
| `http.server(port)` / `http.listen(port)` | Start serving (blocking) |

Routes map a method + path to the **name** of a handler function (a string).
Paths may contain parameters: `/users/:id`. For a full CRUD walkthrough see
[../24-realtime-crud-tutorial.md](../24-realtime-crud-tutorial.md).

### Request data injected into handlers

When a request is routed, the handler runs with these variables predefined:

| Variable | Contents |
|----------|----------|
| `$req_method` | HTTP method (`GET`, `POST`, ...) |
| `$req_path` | Request path |
| `$req_body` | Raw request body |
| `$param_<name>` | Path parameter (e.g. `:id` → `$param_id`) |
| `$query_<name>` | Query-string parameter |
| `$cookie_<name>` | Cookie value (e.g. `session` → `$cookie_session`) |

A cookie that is not present is unset (`typeof` is `"void"`), so guard with
`if typeof(cookie_session) != "string" { ... }`.

### Controlling the response

Call these inside a handler before returning, to set status, headers, and
cookies:

| Call | Effect |
|------|--------|
| `http.set_status(code)` | Override the response status (e.g. `401`, `404`) |
| `http.set_header(name, value)` | Add a response header |
| `http.set_cookie(name, value [, max_age])` | Set a cookie (HttpOnly, SameSite=Lax) |
| `http.clear_cookie(name)` | Expire a cookie immediately |
| `http.redirect(location)` | Reply `302` with a `Location` header |

```ran
fn login() -> str {
    # ... verify credentials ...
    http.set_cookie("session", token, 86400)   # 1 day
    return "{\"ok\":true}"
}

fn dashboard() -> str {
    if current_user() == "" {
        http.redirect("/login")
        return ""
    }
    return "<h1>Welcome</h1>"
}
```

### Response convention

The handler's returned string determines the content type:

- Starts with `<` → served as `text/html`
- Starts with `{` or `[` → served as `application/json`
- Otherwise → `text/plain`

Returning a map serializes to JSON automatically.

### Examples

```ran
import "std::http" as http

fn hello() -> str {
    return "Hello, $param_name!"
}

fn main() {
    http.get("/hello/:name", "hello")
    http.server(8080)
}
```

A complete app with **login, signed sessions, a role-aware dashboard, and
per-user CRUD** (JSON-backed) is in [`examples/webapp_full/`](../../examples/webapp_full/).
A simpler CRUD walkthrough is the [realtime CRUD tutorial](../24-realtime-crud-tutorial.md).

### Configuration & operations

| Setting | How |
|---------|-----|
| Bind host | env `RAN_HOST` (default `0.0.0.0`); set `RAN_HOST=127.0.0.1` to restrict to localhost |
| Port via CLI | `ran app.ran --port 9090` (calls `fn port(p)`) |
| Worker pool size | env `RAN_WORKERS` (default scales with CPU, capped) |
| Static files | files under `public/` are served automatically |
| Max body size | 10 MB |
| Keep-alive | 30s idle, 100 requests/connection |

Security: the server has **no built-in authentication**. Put it behind a
reverse proxy / auth layer for anything exposed beyond localhost.

### Fault isolation

If a handler hits a runtime fault (overflow, divide-by-zero, bad decimal, a
missing `env.require`, ...), the server returns `500 Internal Server Error`,
logs the fault, and keeps serving — one bad request cannot take the process
down. See [errors.md](errors.md).

---

## Client

Plaintext HTTP/1.1 client. Returns a map: `{ status, body, ok, error }`.

| Call | Description |
|------|-------------|
| `http.fetch(url)` | GET request |
| `http.post_to(url, body)` | POST request with a body |
| `http.request(method, url, body)` | Arbitrary method |

```ran
import "std::http" as http
import "std::log" as log

fn main() {
    let r = http.fetch("http://example.com/api")
    if r["ok"] {
        echo r["body"]
    } else {
        log.error("request failed", r["status"], r["error"])
    }
}
```

### Result map

| Key | Type | Meaning |
|-----|------|---------|
| `status` | int | HTTP status code (`0` if the request never completed) |
| `body` | str | Response body |
| `ok` | bool | `true` when `200 <= status < 300` |
| `error` | str | Transport error message, empty on success |

### HTTPS / TLS

The client supports `https://` with TLS provided by the **system OpenSSL**
(libssl). The server's certificate chain is verified against the system trust
store **and** the hostname is checked — an invalid or expired certificate makes
the request fail (returned in the `error` field), it does not connect insecurely.

```ran
let r = http.fetch("https://api.example.com/v1/orders")
if r["ok"] { echo r["body"] }
```

Notes:
- Requires `libssl`/`libcrypto` present at build and run time (standard on
  servers). See [../21-dependency-policy.md](../21-dependency-policy.md).
- Server-side TLS (terminating HTTPS in the Ran HTTP server) is not yet wired
  in; terminate inbound TLS at a reverse proxy for now. Outbound client TLS is
  fully supported.
