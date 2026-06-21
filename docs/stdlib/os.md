# `os` — Process & Environment

```ran
import "std::os" as os
```

| Call | Returns | Description |
|------|---------|-------------|
| `os.args()` | array | Command-line arguments |
| `os.env(key)` | str | Environment variable (`void` if unset) |
| `os.env_or(key, default)` | str | Environment variable or a default |
| `os.setenv(key, value)` | bool | Set an environment variable |
| `os.cwd()` | str | Current working directory |
| `os.platform()` | str | OS name (`linux`, `macos`, ...) |
| `os.arch()` | str | CPU architecture (`x86_64`, `aarch64`, ...) |
| `os.hostname()` | str | Machine hostname |
| `os.getpid()` | int | Current process ID |
| `os.exit(code)` | — | Exit immediately with a status code |

## Example: environment-driven config

```ran
import "std::os" as os
import "std::log" as log

fn main() {
    let port = os.env_or("PORT", "8080")
    let env = os.env_or("APP_ENV", "development")
    log.info("starting in", env, "on port", port)
}
```

Run it (fish):

```fish
set -x PORT 9090
set -x APP_ENV production
ran app.ran
```

## Notes

- `os.env_or` is the recommended way to read config so one binary runs across
  all environments.
- `os.exit(0)` for success, non-zero for failure, by convention.
