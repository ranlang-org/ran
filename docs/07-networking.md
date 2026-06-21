# Networking

Ran ships with an HTTP server built into the `http` module. There are no frameworks to
install. You register routes, point them at handler functions, and start the server.

The server (internally called **FastServer**) handles each connection on its own thread
and supports HTTP/1.1 keep-alive, path parameters, query parameters, cookies, CORS, and
static file serving.

> Security note: the built-in server has no authentication or authorization. Anything
> you expose is reachable by anyone who can reach the port. Add your own auth and input
> validation inside handlers before serving sensitive data or actions.

## A minimal server

```ran
import "std::http" as http

fn handle_home() -> str {
    return "<h1>Hello from Ran!</h1>"
}

fn main() {
    http.get("/", "handle_home")
    http.server(8080)
}
```

```bash
ran server.ran
# [ran] FastServer listening on http://0.0.0.0:8080
# [ran]    workers: 8, keep-alive: 30s
```

```bash
curl http://localhost:8080/
# <h1>Hello from Ran!</h1>
```

> Note: the "workers: 8" line is cosmetic. The server actually spawns a fresh thread
> per incoming connection rather than using a fixed pool.

Three pieces are at work here:

1. `import "std::http" as http` brings in the HTTP module under the `http` alias. The alias
   is mandatory - without it the program will not compile (see
   [13 - Modules & Imports](13-modules-imports.md)).
2. `http.get(path, "handler_name")` registers a route. The second argument is the
   **name** of the function to call, as a string.
3. `http.server(port)` starts the server and **blocks**, handling requests until you
   stop the process. (`http.listen(port)` is an alias for `http.server`.)

### Choosing the port: --port calls fn port(p: int)

Running with `--port N` calls a user-defined `fn port(p: int)` as the entry point
**instead of** `main()`. This keeps the default port (in `main`) and the flag-driven
port (in `port`) separate, so you never start two servers by accident. The recommended
pattern shares a `serve` helper:

```ran
import "std::http" as http

fn serve(p: int) {
    http.get("/", "handle_home")
    echo "Listening on :$p"
    http.server(p)
}

fn handle_home() -> str {
    return "<h1>Hello from Ran!</h1>"
}

fn port(p: int) {   # entry point when run with --port N
    serve(p)
}

fn main() {         # entry point when no --port is given
    serve(8080)
}
```

```bash
ran server.ran                 # no --port: main() runs, serves on 8080
ran server.ran --port 3000     # port(3000) runs, serves on 3000
./compiled_server --port 80    # works for compiled binaries too
```

Rules for `fn port`:

- It must take **exactly one** parameter (the int port).
- If you pass `--port N` but never define `fn port`, Ran stops with:
  `error: --port requires a 'fn port(p: int)' function`.
- `--port=3000` (with `=`) is also accepted.

> The old behavior (a magic `RAN_PORT` environment override inside `http.server`) has
> been removed. Use the `fn port(p: int)` pattern instead.

See [17 - Building Websites](17-building-websites.md) for a full web-building guide.

## Which methods can I register?

Only `GET` and `POST` routes can be registered today, via `http.get` and `http.post`.
There are no `http.put` / `http.delete` / `http.patch` registration calls.

## How handlers work

A handler is an ordinary function called with **no arguments** that returns a string.
The server inspects what you return and picks a content type automatically:

| Returned value | Response type |
|----------------|---------------|
| string starting with `<` | `text/html` |
| string starting with `{` or `[` | `application/json` |
| any other string | `text/plain` |
| a map value | `application/json` (encoded) |

```ran
fn handle_html() -> str {
    return "<p>This is HTML</p>"          # served as text/html
}

fn handle_json() -> str {
    return "{\"status\":\"ok\"}"          # served as application/json
}

fn handle_text() -> str {
    return "just plain text"               # served as text/plain
}
```

## Registering routes

Register routes before calling `http.server`:

```ran
import "std::http" as http

fn main() {
    http.get("/", "handle_home")
    http.get("/about", "handle_about")
    http.post("/submit", "handle_submit")

    http.server(3000)
}
```

## Reading request data

When the server calls your handler, it injects the request details as variables you can
read directly inside the function:

| Variable | Contents |
|----------|----------|
| `req_method` | the HTTP method, e.g. `"GET"` or `"POST"` |
| `req_path` | the request path, e.g. `"/users/42"` |
| `req_body` | the raw request body as a string |
| `query_<name>` | the value of query parameter `<name>` |
| `param_<name>` | the value of path parameter `<name>` |

### Path parameters

Declare a path parameter with `:name` in the route. Inside the handler, read it as
`param_<name>`:

```ran
import "std::http" as http

fn handle_user() -> str {
    return "User ID: $param_id"
}

fn main() {
    http.get("/users/:id", "handle_user")
    http.server(8080)
}
```

```bash
curl http://localhost:8080/users/42
# User ID: 42
```

You can use multiple path parameters, e.g. `/posts/:pid/comments/:cid` exposes
`param_pid` and `param_cid`. Matching is by exact path segment.

### Query parameters

A request to `/search?q=ran&limit=10` exposes `query_q` and `query_limit` (query
strings are percent-decoded, and `+` becomes a space):

```ran
import "std::http" as http

fn handle_search() -> str {
    return "Searching for '$query_q' (limit $query_limit)"
}

fn main() {
    http.get("/search", "handle_search")
    http.server(8080)
}
```

```bash
curl "http://localhost:8080/search?q=ran&limit=10"
# Searching for 'ran' (limit 10)
```

### Request body and method

```ran
import "std::http" as http

fn handle_submit() -> str {
    echo "Got a $req_method to $req_path"
    echo "Body: $req_body"
    return "{\"received\":true}"
}

fn main() {
    http.post("/submit", "handle_submit")
    http.server(8080)
}
```

```bash
curl -X POST http://localhost:8080/submit -d '{"name":"Ran"}'
# {"received":true}
```

The `echo` lines print to the server's own stdout, which is handy for logging.

## Returning JSON

Build JSON strings directly, or return a map (which is encoded to JSON automatically):

```ran
import "std::http" as http

fn handle_api() -> str {
    return "[{\"id\":1,\"text\":\"Learn Ran\"},{\"id\":2,\"text\":\"Build something\"}]"
}

fn main() {
    http.get("/api/todos", "handle_api")
    http.server(3000)
}
```

Because the string starts with `[`, the server sends it with an `application/json`
content type.

## Serving static files

The server automatically serves files from a `public/` directory. A request for
`/style.css` returns `public/style.css`, and a request for `/` falls back to
`public/index.html`. MIME types are detected from the file extension, responses get a
`Cache-Control` header, and directory traversal (`..`) is blocked.

```
myapp/
+-- main.ran
+-- public/
    +-- index.html
    +-- style.css
    +-- app.js
```

This pairs nicely with handlers that serve HTML read from disk:

```ran
import "std::http" as http
import "std::fs" as fs

fn handle_index() -> str {
    return fs.read("public/index.html")
}

fn main() {
    http.get("/", "handle_index")
    http.server(3000)
}
```

## CORS

Cross-Origin Resource Sharing headers are applied by default (origin `*`, methods
`GET, POST, PUT, DELETE, PATCH, OPTIONS`), and the server answers CORS preflight
(`OPTIONS`) requests automatically. Review whether this open policy fits your needs.

## Connection details

For reference, the server uses HTTP/1.1 keep-alive (30-second timeout, up to 100
requests per connection), sets `TCP_NODELAY`, reads with a 64KB buffer, and caps
request bodies at 10MB.

## A complete web app

```ran
import "std::http" as http
import "std::fs" as fs

fn handle_index() -> str {
    return fs.read("public/index.html")
}

fn handle_api_todos() -> str {
    return "[{\"id\":1,\"text\":\"Learn Ran\",\"done\":false}]"
}

fn handle_user() -> str {
    return "{\"id\":\"$param_id\"}"
}

fn serve(p: int) {
    http.get("/", "handle_index")
    http.get("/api/todos", "handle_api_todos")
    http.get("/users/:id", "handle_user")

    echo "Ran web app on http://localhost:$p"
    http.server(p)
}

fn port(p: int) {   # used by --port
    serve(p)
}

fn main() {         # default port
    serve(3000)
}
```

A runnable version of this lives in `examples/webapp/`.

## Tips and gotchas

- **Import the http module with an alias.** Start the file with `import "std::http" as http`
  (and `import "std::fs" as fs` if you read files). Without the alias the program will not
  compile.
- **Pass the handler name as a string.** It's `http.get("/", "handle_home")`, not
  `http.get("/", handle_home)`.
- **Use `fn port(p: int)` for `--port`.** Running with `--port N` calls `fn port(N)`
  instead of `main()`; without `fn port`, `--port` is an error.
- **Only GET and POST can be registered** today.
- **Register routes before `http.server`.**
- **`http.server` blocks.** Code after it won't run until the server stops, so put
  startup `echo`s before it.
- **Escape quotes in JSON strings.** Inside a `"..."` string, write `\"`.
- **Read request data via injected variables** (`param_id`, `query_q`, `req_body`,
  `req_method`, `req_path`).
- **Middleware is not available.** A middleware type exists internally but is not
  executed.

Next: [Hardware & Embedded](08-hardware.md).
