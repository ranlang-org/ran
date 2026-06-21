# `str` — String Utilities

```ran
import "std::str" as str
```

Module functions take the string as the first argument. Many of these are also
available as **methods** on string values (see [builtins.md](builtins.md)).

| Call | Returns | Description |
|------|---------|-------------|
| `str.from(value)` | str | Stringify any value |
| `str.len(s)` | int | Length in characters |
| `str.upper(s)` | str | Uppercase |
| `str.lower(s)` | str | Lowercase |
| `str.trim(s)` | str | Trim whitespace both ends |
| `str.trim_start(s)` | str | Trim leading whitespace |
| `str.trim_end(s)` | str | Trim trailing whitespace |
| `str.contains(s, needle)` | bool | Substring test |
| `str.starts_with(s, prefix)` | bool | Prefix test |
| `str.ends_with(s, suffix)` | bool | Suffix test |
| `str.index_of(s, needle)` | int | Char index of first match, `-1` if absent |
| `str.replace(s, from, to)` | str | Replace all occurrences |
| `str.split(s, delim)` | array | Split into parts |
| `str.join(array, sep)` | str | Join an array into a string |
| `str.repeat(s, n)` | str | Repeat `n` times |
| `str.reverse(s)` | str | Reverse characters |
| `str.pad_left(s, width, pad)` | str | Left-pad to width |
| `str.pad_right(s, width, pad)` | str | Right-pad to width |
| `str.to_int(s)` | int | Parse integer (`0` on failure) |
| `str.to_float(s)` | float | Parse float (`0.0` on failure) |

## Example

```ran
import "std::str" as str

fn main() {
    let parts = str.split("a,b,c", ",")
    echo str.join(parts, " | ")          # a | b | c

    echo str.pad_left("7", 4, "0")        # 0007
    echo str.index_of("hello world", "world")  # 6
    echo str.to_int("  42 ") + 1          # 43
}
```

## Notes

- Indexes and lengths are **character**-based (Unicode scalar values), not byte
  offsets.
- For padding, only the first character of `pad` is used.
