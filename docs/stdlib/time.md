# `time` — Clocks & Timestamps

```ran
import "std::time" as time
```

| Call | Returns | Description |
|------|---------|-------------|
| `time.now()` | int | Unix time in **seconds** |
| `time.now_ms()` | int | Unix time in **milliseconds** |
| `time.now_iso()` | str | Current UTC time as ISO-8601 (`2026-06-17T12:28:35Z`) |
| `time.sleep(ms)` | — | Block the current task for `ms` milliseconds |

## Example: simple timing

```ran
import "std::time" as time
import "std::log" as log

fn main() {
    let start = time.now_ms()
    time.sleep(150)
    let elapsed = time.now_ms() - start
    log.info("slept for", elapsed, "ms")
    echo time.now_iso()
}
```

## Notes

- `now_iso()` is computed with pure integer arithmetic (no external crates) and
  is accurate for dates after 1970, always in UTC (`Z`).
- `time.sleep` affects only the calling task; other `spawn`ed tasks keep
  running.
