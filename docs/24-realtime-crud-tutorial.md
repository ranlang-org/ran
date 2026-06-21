# Tutorial: A Realtime CRUD Web App (JSON-backed)

This walkthrough builds a complete notes board: create, read, update, delete,
with data persisted to a JSON file and a browser UI that stays in sync across
tabs. The full source is in [`examples/crud/`](../examples/crud/).

```fish
cd examples/crud
ran app.ran
# open http://localhost:8080
```

Type a note and press Enter; open a second tab and watch it appear within a
second. Every change is saved to `data/notes.json`.

---

## 1. Reading the imports (and what `as http` means)

The program starts with:

```ran
import "std::http" as http
import "std::fs"   as fs
import "std::json" as json
import "std::time" as time
import "std::log"  as log
```

An import has two parts:

```
import "std::http"   as   http
        ^^^^^^^^^^         ^^^^
        WHAT to load       the LOCAL NAME you call it by (the "alias")
```

- **`"std::http"`** names *which* module to load. The `std::` prefix means "from
  the standard library". It is required for stdlib modules so it is always
  obvious where a name comes from — there is no hidden global `http`.
- **`as http`** binds that module to a **local name** in this file. From then on
  you call its functions through that name: `http.get(...)`, `http.server(...)`.

Why have an alias at all? Three reasons:

1. **Explicitness.** Nothing is in scope unless you named it. Reading `http.get`
   you know exactly which module `get` came from.
2. **Renaming / avoiding clashes.** The alias is yours to choose. You could
   write `import "std::http" as web` and then call `web.get(...)`. If two
   modules had a similar name, aliases keep them apart.
3. **One obvious style.** Because the alias is mandatory (error `E0006` if you
   forget the `std::` prefix, `E0005` if you forget the alias), every file reads
   the same way.

So `import "std::http" as http` is simply: *load the standard HTTP module and
let me refer to it as `http` in this file.* The repeated word is just a common
convention (load `http`, call it `http`); it is not special.

Local files import the same way but **without** `std::`, and the alias is
optional because their declarations merge into your program:

```ran
import "./lib/auth" as auth      # ./lib/auth.ran
import "../shared/money.ran"     # parent paths allowed
```

---

## 2. The data model & persistence

Notes are a JSON array of objects on disk (`data/notes.json`):

```json
[
  { "id": "1718000000001", "text": "buy milk", "created": 1718000000 }
]
```

Two helpers wrap the file:

```ran
DATA_FILE = "data/notes.json"

fn load_notes() -> array {
    if fs.exists(DATA_FILE) {
        return json.decode(fs.read(DATA_FILE))   # text -> array of maps
    }
    return []                                     # first run: empty list
}

fn save_notes(notes) {
    fs.write(DATA_FILE, json.pretty(notes))       # array -> pretty JSON text
}
```

- `fs.read` returns the file as a string; `json.decode` turns JSON text into Ran
  values (objects become **maps**, arrays become **arrays**).
- `json.pretty` does the reverse with indentation; `json.encode` is the compact
  form.

---

## 3. How routing works

In `main` you register routes, then start the server:

```ran
fn main() {
    if !fs.exists("data") { fs.mkdir("data") }

    http.get("/api/notes", "list_notes")
    http.post("/api/notes", "create_note")
    http.put("/api/notes/:id", "update_note")
    http.delete("/api/notes/:id", "delete_note")

    http.server(8080)   # serves public/ as static files, then the routes above
}
```

Key ideas:

- **A route maps `method + path` to a handler by name.** The second argument is
  a **string** — the name of the function to call, not the function itself
  (Ran does not have first-class function values yet).
- **`:id` is a path parameter.** A request to `/api/notes/42` matches
  `/api/notes/:id` and makes `42` available to the handler.
- **Static files first.** `http.server` serves `public/` for `GET` requests
  (so `/` returns `public/index.html`), then falls through to your routes.
- Methods available: `http.get` / `post` / `put` / `delete` / `patch`.

### Request data injected into handlers

A handler is an ordinary function. Before calling it, the server defines these
variables in its scope:

| Variable | Contents |
|----------|----------|
| `req_method` | `"GET"`, `"POST"`, ... |
| `req_path` | the request path |
| `req_body` | the raw request body (a string) |
| `param_<name>` | a path parameter, e.g. `:id` → `param_id` |
| `query_<name>` | a query-string value, e.g. `?q=x` → `query_q` |

You read them as normal variables (`req_body`, `param_id`) or interpolate them
in strings (`"id is $param_id"`).

### What a handler returns

A handler returns a **string**, and the server picks the `Content-Type` from the
first character:

- starts with `<` → `text/html`
- starts with `{` or `[` → `application/json`
- otherwise → `text/plain`

Returning a map serializes to JSON automatically. This is why the handlers below
return JSON text directly.

---

## 4. The handlers, one by one

### Read — `GET /api/notes`

```ran
fn list_notes() -> str {
    return json.encode(load_notes())     # "[ ... ]" -> served as JSON
}
```

### Create — `POST /api/notes`

```ran
fn create_note() -> str {
    let body = json.decode(req_body)     # parse the request JSON
    let text = body["text"]              # index a map with ["key"]
    if text == "" { return "{\"error\":\"text is required\"}" }

    let note = map()                     # build a new object
    set(note, "id", str(time.now_ms()))  # string id, easy to match in the URL
    set(note, "text", text)
    set(note, "created", time.now())

    let notes = load_notes()
    push(notes, note)                    # append to the array variable
    save_notes(notes)
    return json.encode(note)             # echo the created note back
}
```

Note the data-structure helpers: `map()` makes an empty map, `set(m, k, v)` /
`get(m, k)` write/read it, `m["k"]` indexes it, and `push(arr, v)` appends to an
array. These take the **variable** (e.g. `note`, `notes`) and mutate it in place.

### Update — `PUT /api/notes/:id`

```ran
fn update_note() -> str {
    let body = json.decode(req_body)
    let new_text = body["text"]

    let notes = load_notes()
    let result = []
    let found = false
    for n in notes {
        if str(n["id"]) == param_id {    # path param is a string -> compare as string
            set(n, "text", new_text)
            found = true
        }
        push(result, n)
    }
    save_notes(result)

    if found {
        return "{\"ok\":true}"
    }
    return "{\"error\":\"not found\"}"
}
```

> The id is stored as a string so it compares directly with `param_id` (path
> parameters are always strings). Ran has no ternary operator, so the result is
> chosen with a plain `if`/`return`.

### Delete — `DELETE /api/notes/:id`

Rebuild the list without the matching id:

```ran
fn delete_note() -> str {
    let notes = load_notes()
    let result = []
    for n in notes {
        if str(n["id"]) != param_id { push(result, n) }
    }
    save_notes(result)
    return "{\"ok\":true}"
}
```

---

## 5. "Realtime" via polling

The browser (`public/index.html`) calls `GET /api/notes` once a second and
re-renders:

```js
load();
setInterval(load, 1000);   // poll every second
```

That is enough for a shared board to feel live across tabs without any extra
protocol. True push (Server-Sent Events / WebSocket) is a roadmap item; see
[16 - Roadmap](16-roadmap.md).

---

## 6. Things that look unusual (and why)

- **`import "std::http" as http`** — covered in §1. `std::` = standard library;
  `as <name>` = the local name you call it by.
- **Handlers are referenced by name string** (`"list_notes"`), because functions
  are not yet first-class values.
- **`$param_id` / `req_body` appear "from nowhere"** — the server injects them
  into the handler's scope per request (§3).
- **Content-type is inferred from the first character** of the returned string
  (§3) — return `{...}`/`[...]` for JSON, `<...>` for HTML.
- **`set`/`get`/`push`/`map()`** operate on a named variable and mutate it; this
  is the current map/array API.
- **`dec("19.99")`** (not used here) is how you make exact decimals for money —
  see [stdlib/decimal.md](stdlib/decimal.md).

---

## 7. Honest limitations

- **Concurrent writes race.** Each request reads the whole file, modifies it, and
  writes it back. Under simultaneous writes, one update can overwrite another.
  Fine for a demo; a real app uses a database (the planned `db` module, see
  [stdlib/database.md](stdlib/database.md)) or a lock.
- **No auth.** The API and server are open; add authentication before exposing
  it beyond localhost.
- **Polling, not push.** ~1s latency by design.

The money math, routing, JSON, and persistence shown here are production-shaped;
swap the JSON file for `db` later and the handlers barely change.
