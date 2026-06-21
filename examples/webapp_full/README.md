# Full Web App Example — Login, Sessions, Dashboard, CRUD

A complete, runnable web application in Ran, with JSON files as the raw data
store. It demonstrates the whole stack: authentication, signed sessions,
a role-aware dashboard, and per-user task CRUD.

```fish
cd examples/webapp_full
ran app.ran
# open http://localhost:8080
# login:  admin / admin123   (role: admin)
#         user  / secret123  (role: member)
```

## What it shows

- **Login** — `POST /login` checks the password against `data/users.json`,
  where passwords are stored as `sha256(salt + ":" + password)` (never plaintext).
- **Stateless signed sessions** — on success the server sets a cookie
  `session = username.HMAC_SHA256(secret, username)`. Every request re-verifies
  the HMAC (`crypto.hmac_sha256`), so there is no server-side session table to
  manage and a forged cookie is rejected.
- **Auth-gated pages** — `GET /dashboard` redirects to `/login` (HTTP 302) when
  there is no valid session, otherwise renders the dashboard with the user's
  name and role injected into a template.
- **Per-user CRUD** — `GET/POST/PUT/DELETE /api/tasks` operate only on the
  logged-in user's tasks; unauthenticated requests get `401`.
- **Roles** — the user's role is read from `users.json` and shown in the UI.

## Files

| File | Role |
|------|------|
| `app.ran` | Server: routes, auth, handlers, JSON persistence |
| `public/login.html` | Login form (served at `GET /login`) |
| `public/dashboard.html` | Dashboard template (`{{user}}`, `{{role}}`) |
| `public/style.css` | Styling (served as a static file) |
| `data/users.json` | Seed users (hashed passwords) |
| `data/tasks.json` | Task store (created/updated at runtime) |

## HTTP features used

- Cookies in (`$cookie_session`) and out (`http.set_cookie` / `http.clear_cookie`)
- Status control (`http.set_status(401)`) and `http.redirect("/login")`
- Path params (`/api/tasks/:id`), `$req_body`, full REST verbs
- `crypto.sha256` / `crypto.hmac_sha256` for password hashing and session signing

## Honest limitations

- **JSON-file storage races** under concurrent writes (read-modify-write). Fine
  for a demo or single user; production wants a real database — the planned
  **native SQLite** module (see `TODO.md`).
- No CSRF protection or rate limiting; add them before public exposure.
- Passwords use a fast hash (`sha256`); a real system uses a slow password KDF.
