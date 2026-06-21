# `log` — Leveled Logging

Structured, leveled logging for servers and long-running jobs. Output goes to
**stderr** with an ISO-8601 UTC timestamp, so stdout stays clean for program
data.

```ran
import "std::log" as log
```

## Methods

| Call | Level | Behavior |
|------|-------|----------|
| `log.debug(args...)` | DEBUG | Diagnostic detail |
| `log.info(args...)`  | INFO  | Normal operation |
| `log.warn(args...)`  | WARN  | Recoverable problem |
| `log.error(args...)` | ERROR | Failure that needs attention |
| `log.fatal(args...)` | FATAL | Logs then exits with code `1` |

Each call accepts any number of arguments; they are stringified, interpolated
(`$var` works), and joined with spaces.

## Output format

```
INFO  [2026-06-17T12:28:35Z] starting app
WARN  [2026-06-17T12:28:35Z] low disk
```

Levels are color-coded on terminals that support ANSI.

## Example

```ran
import "std::log" as log

fn main() {
    let port = 8080
    log.info("server booting on port", port)
    log.warn("running without TLS")
    # log.fatal("config missing")  # would exit(1)
}
```

## Notes for production

- Logs are line-oriented; redirect stderr to your aggregator:
  `./app 2> app.log` (fish: `./app 2> app.log`).
- Use `log.error` for handled failures and reserve `log.fatal` for
  unrecoverable startup problems.
