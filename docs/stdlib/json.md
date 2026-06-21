# `json` — JSON Encode/Decode

```ran
import "std::json" as json
```

| Call | Returns | Description |
|------|---------|-------------|
| `json.encode(value)` / `json.stringify(value)` | str | Serialize to compact JSON |
| `json.pretty(value)` | str | Serialize with 2-space indentation |
| `json.decode(text)` / `json.parse(text)` | value | Parse JSON into Ran values |
| `json.valid(text)` | bool | True if `text` is one well-formed JSON value |
| `json.get(value_or_text, "a.b.0")` | value | Dotted-path lookup (numeric segments index arrays) |

## Robustness

- **String escapes** on decode: `\" \\ \/ \b \f \n \r \t` and `\uXXXX`,
  including UTF-16 **surrogate pairs** (e.g. emoji).
- **Encode** escapes quotes, backslash, control characters (`\u00XX`), and the
  named short escapes — output is always valid JSON.
- Parsing is bounds-safe: malformed input never panics (it yields a best-effort
  value); use `json.valid` first when you need strictness.
- `decimal` values encode as **exact unquoted JSON numbers**.

## Type mapping

| JSON | Ran |
|------|-----|
| object | map |
| array | array |
| string | str |
| number (int) | int |
| number (decimal) | float |
| `true`/`false` | bool |
| `null` | void |

## Example

```ran
import "std::json" as json

fn main() {
    let text = "{\"name\": \"Ran\", \"version\": 1, \"tags\": [\"fast\", \"safe\"]}"
    let data = json.decode(text)

    echo data["name"]          # Ran
    echo data["tags"][0]       # fast

    let out = json.pretty(data)
    echo out
}
```

## Notes

- `json.decode` is a tolerant parser intended for well-formed input; malformed
  input yields best-effort partial values rather than throwing.
- Map key order in encoded output is not guaranteed (maps are unordered).
- Use `json.encode` for wire/storage, `json.pretty` for human-readable logs and
  config dumps.
