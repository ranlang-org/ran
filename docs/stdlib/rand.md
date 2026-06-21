# `rand` — Pseudo-Random Numbers

```ran
import "std::rand" as rand
```

| Call | Returns | Description |
|------|---------|-------------|
| `rand.int(lo, hi)` | int | Random integer in `[lo, hi)` |
| `rand.float()` | float | Random float in `[0.0, 1.0)` |
| `rand.bool()` | bool | Random boolean |

## Example

```ran
import "std::rand" as rand

fn main() {
    let dice = rand.int(1, 7)     # 1..6
    echo "rolled $dice"
    if rand.bool() {
        echo "heads"
    } else {
        echo "tails"
    }
}
```

## ⚠️ Not cryptographically secure

`rand` uses a time-seeded xorshift64 generator. It is fine for sampling,
jitter, test data, and load generation. **Do not** use it for tokens, session
IDs, passwords, keys, or anything security-sensitive. A CSPRNG is a roadmap
item.
