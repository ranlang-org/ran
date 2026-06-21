# Web — `web` (native asset serving)

The `web` module turns a directory of front-end files into a running web server.
It serves HTML, CSS, and client-side assets directly, with correct content
types, cache validation, optional single-page-app fallback, and an optional
front-end build step — all built in.

```ran
import "std::web" as web
```

## Quick start

Put your front-end files in a directory (commonly `public/`) and serve it:

```ran
import "std::web" as web

fn main() {
    web.serve("public")        # serves public/ on port 8080, then blocks
}
```

Open `http://localhost:8080/` and the server returns `public/index.html`.
Requests for `/style.css`, `/app.js`, images, fonts, and other assets are served
from the same directory with the right `Content-Type`.

## Functions

| Call | Description |
|------|-------------|
| `web.serve(dir)` | Set the web root to `dir` and start the server (blocks, like an HTTP server). Defaults to port 8080. |
| `web.serve(dir, port)` | Same, on an explicit port. |
| `web.spa(enabled)` | Enable/disable single-page-app fallback (default off). Call before `serve`. |
| `web.build(cmd)` | Record a front-end build command to run once before serving. Call before `serve`. |

The default port can also be set with the `RAN_PORT` environment variable.

## Content types

The server picks a `Content-Type` from the file extension. Text types are served
as UTF-8:

| Extension | Content-Type |
|-----------|--------------|
| `.html` | `text/html` |
| `.css` | `text/css` |
| `.js`, `.mjs` | `text/javascript` |
| `.json`, `.map` | `application/json` |
| `.svg` | `image/svg+xml` |
| `.png`, `.jpg`, `.jpeg`, `.webp`, `.gif`, `.ico` | image types |
| `.wasm` | `application/wasm` |
| `.woff`, `.woff2` | font types |
| `.txt` | `text/plain` |
| anything else | `application/octet-stream` (and `E0405`) |

## Single-page-app fallback

A single-page application uses one HTML entry point and handles routing on the
client. Enable SPA mode so any request that does not match a file returns
`index.html` instead of `404`:

```ran
import "std::web" as web

fn main() {
    web.spa(true)              # unmatched paths serve index.html
    web.serve("public")
}
```

With SPA mode off, a request for a missing file returns `404`.

## Cache validation

The server sends `ETag` and `Last-Modified` headers with each asset. When a
client revalidates with `If-None-Match` / `If-Modified-Since` and the asset is
unchanged, the server responds `304 Not Modified` with no body, so unchanged
assets are not re-sent.

## Front-end build hook

If your front end has a build step, register it with `web.build`. The command
runs to completion **before** the server starts serving, so stale assets are
never served in dev mode:

```ran
import "std::web" as web

fn main() {
    web.build("make assets")   # runs once before serving; must succeed
    web.serve("public")
}
```

If the build command fails, serving is aborted with `E0404` rather than serving
stale assets.

## Combining with HTTP routes

The web server shares the HTTP server runtime, so you can register dynamic
routes alongside static assets. Define handlers and register them before
`serve`:

```ran
import "std::web" as web
import "std::http" as http

fn health() {
    return "ok"
}

fn main() {
    http.get("/health", "health")   # dynamic route
    web.serve("public")             # static assets + the route above
}
```

## Security

- **Path traversal is blocked.** Requests are resolved within the web root only;
  a path that escapes it (for example `../secrets`) is rejected with `403`
  (`E0403`).
- **The server does not terminate TLS.** Put a TLS-terminating proxy in front for
  HTTPS, and add authentication there or in your routes — the static server is
  unauthenticated. See [../14-security.md](../14-security.md).

## Diagnostics

| Code  | Meaning |
|-------|---------|
| E0401 | web root directory does not exist (nothing is served) |
| E0402 | asset could not be read |
| E0403 | request path escaped the web root (rejected `403`) |
| E0404 | front-end build command failed |
| E0405 | unknown extension served as `application/octet-stream` |

See [../17-building-websites.md](../17-building-websites.md) for an end-to-end
walkthrough.
