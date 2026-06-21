# `env` — Environment & Configuration

```ran
import "std::env" as env
```

A configuration library: read environment variables with **typed getters and
defaults**, **require** critical variables (fail fast), and load **`.env`
files**. For services configured entirely through the environment, this is the
single source of runtime config.

## Reading values

| Call | Returns | Description |
|------|---------|-------------|
| `env.get(key)` | str / void | Raw value, or `void` if unset |
| `env.get_or(key, default)` | str | Value or a default |
| `env.require(key)` | str | Value, or **abort** with `E1005` if unset |
| `env.has(key)` | bool | Whether the variable is set |

## Typed getters (with defaults)

| Call | Returns | Notes |
|------|---------|-------|
| `env.int(key, default)` | int | Parses an integer, falls back to default |
| `env.float(key, default)` | float | Parses a float |
| `env.bool(key, default)` | bool | `true/1/yes/on/y` → true; `false/0/no/off/n` → false |
| `env.decimal(key, default)` | decimal | **Exact** — for money config like tax rates |

`env.decimal` is the recommended way to read monetary configuration (rates,
limits, fees) so values stay exact end-to-end.

## Writing

| Call | Description |
|------|-------------|
| `env.set(key, value)` | Set a variable for this process |
| `env.unset(key)` | Remove a variable |
| `env.all()` | Map of every environment variable |

## Loading `.env` files

| Call | Description |
|------|-------------|
| `env.load(path)` | Load a `.env` file; **does not override** existing vars. Returns count set. |
| `env.load_override(path)` | Like `load`, but overrides existing vars |
| `env.load_default()` | Load `./.env` if present (no error if missing) |

`.env` format:

```sh
# comment lines start with #
APP_NAME="Ran Shop"      # quotes are stripped
export TAX_RATE=0.11     # optional `export` prefix
DEBUG=yes
PORT=8443
```

Quoted values keep inner spaces; unquoted values drop trailing ` # comments`.
A missing file is not an error (returns 0), so `env.load_default()` is safe to
call unconditionally.

## Example: environment-driven config

```ran
import "std::env" as env
import "std::log" as log

fn main() {
    env.load_default()                       # pick up ./.env in dev

    let app  = env.require("APP_NAME")        # fail fast if absent in prod
    let port = env.int("PORT", 8080)
    let rate = env.decimal("TAX_RATE", "0.00")
    let debug = env.bool("DEBUG", false)

    log.info("starting", app, "on port", port)
    if debug { log.warn("debug mode on") }
    echo "tax rate: " + rate
}
```

## Relationship to `os`

`os.env`/`os.env_or`/`os.setenv` provide raw access. `env` adds typed getters,
`require`, decimal config, and `.env` loading. Prefer `env` for application
configuration; use `os` for general process/environment queries.

## Errors

| Code | Cause |
|------|-------|
| `E1005` | `env.require(key)` called for a variable that is not set |
