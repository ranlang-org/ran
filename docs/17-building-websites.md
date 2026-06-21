# Building Websites with Ran

Ran ships with a built-in HTTP server, so you can build a website or JSON API
without installing any framework or dependency. This guide shows the fast path.

See also: [07 - Networking](07-networking.md) for the full server reference.

## The smallest server

```ran
import "std::http" as http

fn home() {
    return "<h1>Hello from Ran</h1>"
}

fn main() {
    http.get("/", "home")
    http.server(8080)
}
```

```bash
ran site.ran
# Server listening on http://0.0.0.0:8080
```

Open http://localhost:8080 in a browser. The first line, `import "std::http" as http`, is
required - stdlib modules need a mandatory alias.

## How routing works

A route maps a method and path to a **handler function name** (a string):

```ran
import "std::http" as http

http.get("/", "home")            // GET  /
http.get("/about", "about")      // GET  /about
http.post("/api/users", "create_user")  // POST /api/users
```

A handler is an ordinary function that returns a string. Ran auto-detects the
content type from the first character of the return value:

- starts with `<`  -> sent as `text/html`
- starts with `{` or `[`  -> sent as `application/json`
- anything else  -> sent as `text/plain`

```ran
fn home()   { return "<h1>Home</h1>" }      // HTML
fn health() { return "{\"ok\": true}" }     // JSON
fn ping()   { return "pong" }               // plain text
```

> Status: only `http.get` and `http.post` can register routes today.
> PUT/DELETE/PATCH are planned.

## Reading request data

The server injects request information into your handler as plain variables you
can interpolate with `$name`:

| Variable | Contains |
|----------|----------|
| `$req_method` | the HTTP method, e.g. `GET` |
| `$req_path`   | the request path, e.g. `/api/hi/Ran` |
| `$req_body`   | the raw request body (for POST) |
| `$param_<name>` | a path parameter (see below) |
| `$query_<name>` | a query-string parameter |

### Path parameters

Use `:name` in the route path. It arrives as `$param_name`:

```ran
import "std::http" as http

fn greet() {
    return "Hello, $param_name!"
}

fn main() {
    http.get("/hi/:name", "greet")
    http.server(8080)
}
```

```bash
curl localhost:8080/hi/Ran
# Hello, Ran!
```

### Query parameters

`/search?q=rust` arrives as `$query_q`:

```ran
import "std::http" as http

fn search() {
    return "You searched for: $query_q"
}

fn main() {
    http.get("/search", "search")
    http.server(8080)
}
```

```bash
curl "localhost:8080/search?q=rust"
# You searched for: rust
```

> Tip: request variables are interpolated into your returned string
> automatically. `return "Hello, $param_name!"` just works.

## Choosing a port: --port calls fn port(p: int)

You set the default port in code with `http.server(port)`, but running with `--port N`
calls a user-defined `fn port(p: int)` as the entry point **instead of** `main()`. This
keeps the in-code default and the flag-driven port separate, so you never start two
servers by accident:

```bash
ran site.ran                 # no --port: main() runs
ran site.ran --port 3000     # port(3000) runs instead of main()
ran run site.ran --port 8000 # port(8000) runs
```

The recommended pattern shares a `serve` helper between `main` and `port`:

```ran
import "std::http" as http

fn serve(p: int) {
    http.get("/", "home")
    echo "Listening on $p"
    http.server(p)
}

fn home() { return "<h1>Hi</h1>" }

fn port(p: int) {   // used by --port N
    serve(p)
}

fn main() {         // default when no --port
    serve(8080)
}
```

`fn port` must take exactly one int parameter. If you pass `--port N` without defining
`fn port`, Ran stops with `error: --port requires a 'fn port(p: int)' function`.

## Serving static files: the `web` module

The `web` module serves a whole directory of front-end assets — HTML, CSS,
client-side scripts, images, fonts — with correct content types, cache
validation, optional single-page-app fallback, and an optional build hook. Point
it at a directory and it serves it:

```ran
import "std::web" as web

fn main() {
    web.serve("public")        // serves ./public on port 8080, then blocks
}
```

Dynamic routes and static assets coexist: register routes with `http.get`, then
call `web.serve` to start the server.

```ran
import "std::http" as http
import "std::web" as web

fn health() { return "{\"ok\": true}" }

fn main() {
    http.get("/api/health", "health")   // dynamic route
    web.serve("public", 8080)           // static assets + the route above
}
```

A request that matches a route runs the handler; anything else is served as a
file from the web root. See [stdlib/web.md](stdlib/web.md) for SPA fallback,
cache validators, and the build hook.

## Project structure: clear, framework-style layout

The single rule that keeps file locations obvious: **`public/` is the web root,
and a URL path mirrors the file path beneath it.** A file at
`public/assets/css/blog.css` is always served at `/assets/css/blog.css` — a
one-to-one mapping, with no hidden rewriting. Multiple CSS/JS files are normal:
add them under `assets/css/` or `assets/js/` and reference them in order.

A clean, framework-style layout separates data, views, routes, and assets:

```
blog/
+-- app.ran          # bootstrap: register routes, then serve
+-- routes.ran       # controllers: request handlers (URL -> HTML)
+-- views.ran        # views: a shared layout() + partials
+-- content.ran      # data: the blog posts
+-- public/          # web root (URL == file path)
    +-- favicon.svg              ->  /favicon.svg
    +-- assets/
        +-- css/
        |   +-- reset.css        ->  /assets/css/reset.css
        |   +-- blog.css         ->  /assets/css/blog.css
        +-- js/
            +-- enhance.js       ->  /assets/js/enhance.js
```

Keep the page frame — `<head>`, the CSS/JS includes, header, and footer — in one
`layout()` function so every page shares it (a single source of truth, like a
Layout component). Local files are wired together with `import`:

```ran
// app.ran
import "std::http" as http
import "std::web" as web
import "./content"   // data:   all_posts(), find_post(slug)
import "./views"     // views:  layout(title, content), post_card(p)
import "./routes"    // routes: page_home(), page_post(), ...

fn main() {
    http.get("/", "page_home")
    http.get("/posts/:slug", "page_post")
    web.serve("public", 8080)
}
```

A complete, runnable version of this layout is in
[`examples/blog/`](../examples/blog/) (with its own README explaining each file).

### Building HTML strings

In Ran, one expression does not continue onto the next line, so build multi-line
HTML by accumulating into a mutable string:

```ran
fn page_post(title, body) {
    let mut h = "<article>"
    h = h + "<h1>" + title + "</h1>"
    h = h + "<div class=\"prose\"><p>" + body + "</p></div>"
    h = h + "</article>"
    return layout(title, h)
}
```

MIME types are detected from the file extension (html, css, js, json, svg, png,
jpg, webp, gif, ico, wasm, woff, woff2, txt, and more); unknown extensions are
served as `application/octet-stream`.

## A complete example

```ran
// blog.ran - run with: ran blog.ran --port 3000
import "std::http" as http

fn home() {
    return "<h1>My Blog</h1><a href=\"/posts/1\">First post</a>"
}

fn post() {
    return "<h1>Post #$param_id</h1><p>Content for post $param_id.</p>"
}

fn api_posts() {
    return "[{\"id\": 1, \"title\": \"Hello\"}]"
}

fn serve(p: int) {
    http.get("/", "home")
    http.get("/posts/:id", "post")
    http.get("/api/posts", "api_posts")

    echo "Blog running on port $p"
    http.server(p)
}

fn port(p: int) {   // used by --port
    serve(p)
}

fn main() {         // default port
    serve(3000)
}
```

```bash
ran blog.ran --port 3000
curl localhost:3000/posts/1
# <h1>Post #1</h1><p>Content for post 1.</p>
```

## Building it into one binary

Ship your whole site as a single native file:

```bash
ran build blog.ran -o blog
./blog --port 3000
```

The binary contains your code (compressed and encrypted) plus the runtime. No
dependencies needed on the target machine. See [09 - Compilation](09-compilation.md).

## Server features at a glance

- Concurrent connection handling (a thread per connection)
- HTTP/1.1 keep-alive (up to 100 requests per connection, 30s timeout)
- Path parameters (`:id`), query strings, cookies
- CORS enabled by default (origin `*`)
- Static file serving from `public/`
- 64KB read buffer, 10MB max request body

## Security note

The built-in server has **no authentication or rate limiting**. Do not expose it
directly to the public internet for sensitive workloads without putting it behind
a reverse proxy (and adding auth in your handlers). Read secrets from the
environment with `os.env("API_KEY")` rather than hard-coding them.

## Limitations today

- Only GET and POST routes can be registered (PUT/DELETE/PATCH are planned).
- Middleware is planned, not yet runnable.
- For dynamic HTML, build strings by accumulating with `+` (and `$var`
  interpolation); a real template engine is planned.
- The server does not terminate TLS — front it with a TLS-terminating proxy.

Handlers **can** set custom status codes and headers from Ran: `http.set_status(code)`,
`http.set_header(name, value)`, `http.set_cookie(...)`, and `http.redirect(location)`.

See [16 - Roadmap](16-roadmap.md) for what is coming next.
