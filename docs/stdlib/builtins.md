# Built-in Functions & Value Methods

These are always available — no `import` needed.

## Global functions

| Call | Returns | Description |
|------|---------|-------------|
| `echo expr` | — | Print a line; `echo -e` interprets `\n \t \r` |
| `print(args...)` / `println(args...)` | — | Print args separated by spaces |
| `len(x)` | int | Length of string, array, or map |
| `typeof(x)` | str | `int`, `float`, `string`, `bool`, `array`, `map`, `void` |
| `str(x)` | str | Convert to string |
| `int(x)` | int | Convert to integer |
| `float(x)` | float | Convert to float |
| `bool(x)` | bool | Truthiness |
| `range(n)` / `range(a, b)` | array | `[0..n)` or `[a..b)` |
| `push(arr, value)` | — | Append to an array variable in place |
| `map()` | map | Create an empty map |
| `set(m, key, value)` | — | Insert into a map variable in place |
| `get(m, key)` | value | Read from a map variable |
| `keys(m)` | array | Map keys |
| `values(m)` | array | Map values |
| `abs(x)` | number | Absolute value |
| `assert(cond, msg?)` | — | Abort with `msg` if `cond` is falsy |
| `exit(code)` | — | Exit with a status code |

## String methods

`s.len()`, `s.to_upper()`, `s.to_lower()`, `s.trim()`, `s.contains(x)`,
`s.starts_with(x)`, `s.ends_with(x)`, `s.replace(a, b)`, `s.split(d)`,
`s.chars()`, `s.repeat(n)`, `s.slice(start, end)`

## Array methods

`a.len()`, `a.first()`, `a.last()`, `a.contains(x)`, `a.join(sep)`,
`a.reverse()`, `a.slice(start, end)`

## Map methods

`m.keys()`, `m.values()`, `m.has(key)`, `m.len()`

## Example

```ran
fn main() {
    let xs = [3, 1, 2]
    echo len(xs)            # 3
    echo xs.contains(2)     # true

    let m = map()
    set(m, "a", 1)
    echo get(m, "a")        # 1
    echo m.has("a")         # true

    for i in range(3) {
        echo i              # 0,1,2
    }
}
```

## String interpolation

Inside any string, `$name` and `${name}` expand to the variable's value:

```ran
let who = "world"
echo "hello $who"          # hello world
echo "sum=${1}"            # literal ${1} stays if no such var
```
