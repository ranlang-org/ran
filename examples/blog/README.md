# Blog example — a tidy web project structure

A small but complete blog built entirely with Ran: server-side routes, a shared
layout, request logging, and organized static assets. It shows a **clear file
structure** — where each file lives and how everything connects — the way a
modern web framework does.

## Run

```fish
cd examples/blog
ran app.ran                 # http://localhost:8080
# or pick another port:
ran app.ran --port 3000
```

Try a few URLs:

- `/` — home, lists posts
- `/about` — about page
- `/posts/native-web` — a single post (server-rendered)
- `/posts/anything-else` — unknown slug → 404 page

## Build a standalone binary

```fish
cd examples/blog
ran build app.ran -o blog
./blog --port 3000          # serves from ./public next to the binary
```

The build inlines every local import (`util`, `content`, `routes`, `views`) into
the binary, so it runs the same as `ran run`. The `public/` directory is read
from disk at runtime, so keep it next to the binary (or run from this folder).

## Project structure

```
blog/
├── app.ran                 # BOOTSTRAP — register routes, then serve
├── routes.ran              # CONTROLLERS — request handlers (URL → HTML) + logging
├── views.ran               # VIEWS — layout() + shared partials
├── content.ran             # DATA — blog posts (seed)
├── util.ran                # PURE RAN — helpers with no built-in library (escape_html)
└── public/                 # WEB ROOT — URL mirrors the file path
    ├── favicon.svg                 →  /favicon.svg
    └── assets/
        ├── css/
        │   ├── reset.css           →  /assets/css/reset.css
        │   └── blog.css            →  /assets/css/blog.css
        └── js/
            └── enhance.js          →  /assets/js/enhance.js
```

### Rules that keep paths obvious

1. **`public/` is the web root.** An asset's URL is **exactly** its file path
   under `public/`. `public/assets/css/blog.css` always shows at
   `/assets/css/blog.css`. One-to-one mapping, no hidden rewriting.

2. **Multiple CSS/JS files are normal and explicit.** Add files under
   `assets/css/` or `assets/js/` and reference them with `<link>`/`<script>`.
   The order is visible (e.g. `reset.css` first, then `blog.css`).

3. **One source of truth for the page frame.** The `<head>`, CSS/JS includes,
   header, and footer live in `layout()` (`views.ran`). Change it once, every
   page updates — like a Layout component.

4. **Clear separation of roles:** data (`content.ran`) → view (`views.ran`) →
   controller (`routes.ran`) → bootstrap (`app.ran`), with pure-Ran helpers in
   `util.ran`.

## How it connects

```
app.ran
  ├─ import "./util"       pure Ran (no stdlib): escape_html(s)
  ├─ import "./content"    data:    all_posts(), find_post(slug)
  ├─ import "./views"      views:   layout(title, content), post_card(p)
  ├─ import "./routes"     routes:  page_home(), page_post(), page_about(), page_not_found()
  └─ register_routes()     http.get("/", "page_home"), http.get("/posts/:slug", "page_post"), ...
                           web.serve("public", port)   ← dynamic routes + static assets
```

When a request arrives:

- **Matches a route** (e.g. `/posts/native-web`) → the handler in `routes.ran`
  runs. The path param `:slug` arrives as the variable `param_slug`.
- **Matches no route** → served as a static file from `public/`.

## Logging

Every request is logged once, at a single point, with method, path, and the
status returned (`routes.ran` → `log_request`). Logs go to **stderr** via the
`log` module, so stdout stays clean. The server start is logged at `INFO`, and a
missing post logs at `WARN`:

```
INFO  [2026-06-19T01:57:27Z] blog server starting on port 3000
INFO  [2026-06-19T01:57:47Z] GET / -> 200
WARN  [2026-06-19T01:57:47Z] GET /posts/nope -> 404
```

## Pure-Ran helpers (no built-in library)

`util.ran` imports **nothing** — no `std::` module. `escape_html()` is built
only on core language features (the string `.replace` method), and the views use
it to escape dynamic text before it reaches the page. It shows how ordinary
logic can already be written entirely in Ran today; as the language grows, more
of the standard library can move into plain-Ran modules like this one.

## Style note

In Ran one expression does not continue onto the next line, so build multi-line
HTML by accumulating:

```ran
let mut h = "<article>"
h = h + "<h1>" + title + "</h1>"
h = h + "</article>"
return h
```

See [docs/17 - Building Websites](../../docs/17-building-websites.md) and
[docs/stdlib/web.md](../../docs/stdlib/web.md) for the `web` module reference.
