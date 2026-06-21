# Standard Library

Ran's standard library is built in. There is nothing to install - `echo`, string and
array methods, and modules like `http`, `fs`, and `json` are always available.

This chapter is the reference for built-in functions, value methods, and the standard
modules.

## Built-in functions

Global functions you can call from anywhere.

### Output

```ran
echo "Hello, $name!"      # print with $var interpolation + newline
echo -e "a\nb"            # -e interprets \n, \t, \r (here: two lines)
println("Hello")          # print values separated by spaces, with a newline
print("no newline part")  # print values separated by spaces
```

`echo` and `print` / `println` all perform `$var` interpolation on string content. As
always, interpolation substitutes variable names only, not expressions or calls.

By default `echo` prints escape sequences **literally**: `echo "a\nb"` prints the eight
characters `a\nb` on one line. Pass `-e` to interpret `\n`, `\t`, and `\r`:
`echo -e "a\nb"` prints two lines. Quote escapes (`\"`, `\'`, `\\`) always work, with or
without `-e`, so JSON like `"{\"k\": 1}"` is fine either way.

### Inspection

```ran
len("hello")              # 5  (string length in BYTES)
len([1, 2, 3])            # 3  (array length)
typeof(42)                # "int"
typeof("hi")              # "string"
```

`len` works on strings, arrays, and maps. For strings it counts **bytes**, not
characters. `typeof` returns one of: `int`, `float`, `string`, `bool`, `array`, `map`,
`void`.

### Conversion

```ran
str(99)                   # "99"   (anything -> string)
int("123")                # 123    (string/float/bool -> int)
float(5)                  # 5.0    (int/string -> float)
```

If a string can't be parsed, `int` returns `0` and `float` returns `0.0`.

### Process control

```ran
exit(0)                   # exit the process with a status code
exit(1)                   # non-zero = error
```

### Collections

```ran
# Arrays
let mut nums = [1, 2]
push(nums, 3)             # append a value

# Maps
let m = map()             # create an empty map
set(m, "key", "value")    # set a key
get(m, "key")             # -> "value"
```

`push`, `set`, and `get` take the collection as their first argument.

### Ranges

```ran
range(5)                  # [0, 1, 2, 3, 4]
range(2, 6)               # [2, 3, 4, 5]
```

`range(n)` builds `[0, 1, ..., n-1]`. `range(a, b)` builds `[a, ..., b-1]` (the end is
exclusive). It returns a normal array, so it pairs naturally with `for`:

```ran
for i in range(5) {
    echo "i=$i"
}
```

### Map helpers

```ran
let m = map()
set(m, "a", 1)
set(m, "b", 2)

keys(m)                   # ["a", "b"]   (array of keys)
values(m)                 # [1, 2]       (array of values)
```

`keys` and `values` are global functions that return a map's keys or values as an
array. (The `m.keys()` / `m.values()` methods do the same thing.)

### Math helper

```ran
abs(-7)                   # 7     (int in -> int out)
abs(-3.5)                 # 3.5   (float in -> float out)
```

`abs` returns the absolute value of an int or a float, with no import needed. For the
rest of the math functions, import the [`math`](#math) module.

### Assertions

```ran
assert(1 == 1)                       # passes silently
assert(x > 0, "x must be positive")  # exits with an error if x <= 0
```

`assert(cond)` checks a condition; if it is falsy the program prints
`assert failed` and exits with a non-zero status. `assert(cond, "message")` includes
your message in the output. Use it for quick sanity checks and example tests.

## String methods

Call methods on a string value with `.`:

```ran
let s = "Hello World"

echo s.len()                   # 11
echo s.to_upper()              # "HELLO WORLD"
echo s.to_lower()              # "hello world"
echo s.trim()                  # "Hello World" (trims surrounding whitespace)
echo s.contains("World")       # true
echo s.starts_with("Hello")    # true
echo s.ends_with("World")      # true
echo s.replace("World", "Ran") # "Hello Ran"
echo s.split(" ")              # ["Hello", "World"]
echo s.chars()                 # ["H", "e", "l", "l", "o", ...]
echo s.repeat(2)               # "Hello WorldHello World"
echo s.slice(0, 5)             # "Hello"
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.len()` | int | Number of bytes |
| `.to_upper()` | str | Uppercased copy |
| `.to_lower()` | str | Lowercased copy |
| `.trim()` | str | Whitespace removed from both ends |
| `.contains(s)` | bool | Whether `s` appears in the string |
| `.starts_with(s)` | bool | Prefix test |
| `.ends_with(s)` | bool | Suffix test |
| `.replace(a, b)` | str | Replace `a` with `b` |
| `.split(sep)` | array | Split into an array of parts |
| `.chars()` | array | Split into individual characters |
| `.repeat(n)` | str | The string repeated `n` times |
| `.slice(start, end)` | str | Substring from `start` to `end` |

## Array methods

```ran
let arr = [3, 1, 2]

echo arr.len()                 # 3
echo arr.first()               # 3
echo arr.last()                # 2
echo arr.contains(1)           # true
echo arr.join(", ")            # "3, 1, 2"
echo arr.reverse()             # [2, 1, 3]
echo arr.slice(0, 2)           # [3, 1]
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.len()` | int | Number of elements |
| `.first()` | value | First element |
| `.last()` | value | Last element |
| `.contains(x)` | bool | Whether `x` is present |
| `.join(sep)` | str | Join elements into a string |
| `.reverse()` | array | Reversed copy |
| `.slice(start, end)` | array | Sub-array from `start` to `end` |

## Map methods

```ran
let m = map()
set(m, "a", 1)
set(m, "b", 2)

echo m.keys()                  # ["a", "b"]
echo m.values()                # [1, 2]
echo m.has("a")                # true
echo m.len()                   # 2
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.keys()` | array | The map's keys |
| `.values()` | array | The map's values |
| `.has(key)` | bool | Whether `key` is present |
| `.len()` | int | Number of entries |

Map iteration order is not guaranteed.

## Modules

Modules are accessed through a **mandatory alias**. Before you can call a stdlib
module you must import it with `import "name" as alias`, then call methods on the
alias. By convention, alias a module to its own name (`import "std::fs" as fs`), but any
identifier works (`import "std::fs" as disk` lets you call `disk.read(...)`).

```ran
import "std::http" as http
import "std::time" as time
import "std::fs" as fs
import "std::json" as json
import "std::os" as os
import "std::math" as math
import "std::html" as html
import "std::str" as str
import "std::rand" as rand
```

Using a module without importing it is `error[E0001]: undefined variable or module`.
Importing a stdlib module without an alias is `error[E0005]: stdlib import requires an
alias`. The runtime-wired standard modules are `http`, `web`, `db`, `concurrency`,
`time`, `fs`, `json`, `os`, `math`, `html`, `str`, `rand`, `log`, `decimal`, `env`,
and `crypto`. Local file imports are different: they use
`import "./path"` with no alias - see [13 - Modules & Imports](13-modules-imports.md).

> Not importable: `net`, `sync`, `fmt`, `hardware`, `io`, and `regex` are **not**
> stdlib modules; importing one reports `module 'name' not found`. (`hardware`
> remains library-only - see [08 - Hardware](08-hardware.md).)

### `http` - web server

```ran
import "std::http" as http

http.get("/path", "handler_name")    # register a GET route
http.post("/path", "handler_name")   # register a POST route
http.server(8080)                    # start the server (blocking)
http.listen(8080)                    # alias for http.server
```

See [07 - Networking](07-networking.md) for the complete guide.

### `time`

```ran
import "std::time" as time

time.sleep(1000)          # pause for 1000 milliseconds
time.now()                # current Unix timestamp (seconds)
time.now_ms()            # current Unix timestamp (milliseconds)
```

Example:

```ran
import "std::time" as time

fn main() {
    let start = time.now_ms()
    time.sleep(50)
    let end = time.now_ms()
    let elapsed = end - start
    echo "Slept about $elapsed ms"
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.sleep(ms)` | void | Pause for `ms` milliseconds |
| `.now()` | int | Unix time in seconds |
| `.now_ms()` | int | Unix time in milliseconds |

### `fs` - file system

```ran
import "std::fs" as fs

fs.read("file.txt")              # -> file contents as a string
fs.write("out.txt", "hello")     # write a string to a file -> true/false
fs.append("out.txt", " more")    # append to a file (creates it if missing)
fs.exists("file.txt")            # -> true if the path exists
fs.readdir(".")                  # -> array of entry names in a directory
fs.remove("out.txt")             # delete a file or directory
fs.mkdir("logs")                 # create a directory
fs.is_file("file.txt")           # -> true if the path is a regular file
fs.is_dir("logs")                # -> true if the path is a directory
```

Example:

```ran
import "std::fs" as fs

fn main() {
    fs.mkdir("notes")
    fs.write("notes/today.txt", "first note")
    fs.append("notes/today.txt", "\nsecond note")

    if fs.is_file("notes/today.txt") {
        let content = fs.read("notes/today.txt")
        echo "Notes: $content"
    }

    for name in fs.readdir("notes") {
        echo "  - $name"
    }

    fs.remove("notes/today.txt")
    echo fs.exists("notes/today.txt")   # false
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.read(path)` | str | File contents |
| `.write(path, s)` | bool | Write `s`, replacing contents |
| `.append(path, s)` | bool | Append `s`, creating the file if missing |
| `.exists(path)` | bool | Whether the path exists |
| `.readdir(path)` | array | Entry names in a directory |
| `.remove(path)` | bool | Delete a file or directory |
| `.mkdir(path)` | bool | Create a directory |
| `.is_file(path)` | bool | Whether the path is a regular file |
| `.is_dir(path)` | bool | Whether the path is a directory |

### `json`

```ran
import "std::json" as json

json.encode(value)        # -> JSON string (all value types)
json.decode(string)       # -> parsed value (objects -> map, arrays -> array, etc.)
```

`json.encode` serializes ints, floats, strings, bools, arrays, and maps.
`json.decode` fully parses JSON: objects become maps, arrays become arrays, and
nested structures, numbers, bools, and strings come back as the matching Ran value.
JSON `null` decodes to `void`. Read fields out of a decoded object with `data["key"]`.

```ran
import "std::json" as json

fn main() {
    let m = map()
    set(m, "name", "Ran")
    set(m, "port", 8080)

    let text = json.encode(m)
    echo $text                       # e.g. {"name":"Ran","port":8080}

    # Scalars decode to values:
    let n = json.decode("42")        # 42 (int)
    let flag = json.decode("true")   # true (bool)
    echo "$n $flag"

    # Objects and arrays decode into real maps and arrays:
    let data = json.decode("{\"name\": \"Ran\", \"tags\": [\"fast\", \"small\"]}")
    let name = data["name"]
    let tags = data["tags"]
    echo "name: $name"               # name: Ran
    echo tags                        # [fast, small]
    echo tags.len()                  # 2

    let nums = json.decode("[10, 20, 30]")
    echo nums[1]                     # 20
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.encode(value)` | str | Serialize any value to a JSON string |
| `.decode(string)` | value | Parse JSON into ints, floats, strings, bools, arrays, maps, or `void` for null |

### `os`

```ran
import "std::os" as os

os.args()                 # -> array of command-line arguments
os.env("HOME")            # -> value of an environment variable, or void if unset
os.setenv("KEY", "val")   # set an environment variable for this process
os.cwd()                  # -> current working directory
os.platform()             # -> "linux", "macos", "windows", ...
os.arch()                 # -> "x86_64", "aarch64", ...
os.exit(0)                # exit with a status code
```

Example:

```ran
import "std::os" as os

fn main() {
    let dir = os.cwd()
    let plat = os.platform()
    let cpu = os.arch()
    echo "Running in $dir on $plat ($cpu)"

    os.setenv("RAN_GREETING", "hi")
    let g = os.env("RAN_GREETING")
    echo "greeting: $g"

    let args = os.args()
    let count = args.len()
    echo "Got $count arguments"
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.args()` | array | Command-line arguments |
| `.env(key)` | str/void | Environment variable value, or void if unset |
| `.setenv(key, val)` | void | Set an environment variable for this process |
| `.cwd()` | str | Current working directory |
| `.platform()` | str | Operating system name |
| `.arch()` | str | CPU architecture |
| `.exit(code)` | never | Exit with a status code |

### `math`

```ran
import "std::math" as math

math.abs(-7)              # 7
math.abs(-3.5)            # 3.5
math.max(3, 9)            # 9
math.max(3.5, 9.1)        # 9.1
math.min(3, 9)            # 3
math.sqrt(16.0)           # 4
math.pow(2, 10)           # 1024
math.floor(3.7)           # 3   (returns int)
math.ceil(3.2)            # 4   (returns int)
math.round(3.5)           # 4   (returns int)
math.sin(0.0)             # 0
math.cos(0.0)             # 1
math.tan(0.0)             # 0
math.log(2.718281828)     # ~1  (natural log)
math.log10(1000.0)        # 3
math.pi()                 # 3.141592653589793
math.e()                  # 2.718281828459045
```

`abs`, `max`, and `min` work on both ints and floats. `floor`, `ceil`, and `round`
return an int. The remaining functions work on numbers and return floats.

Example:

```ran
import "std::math" as math

fn main() {
    let a = 3.0
    let b = 4.0
    let c = math.sqrt(a * a + b * b)
    echo "hypotenuse: $c"            # hypotenuse: 5

    let area = math.pi() * 2.0 * 2.0
    echo "circle area: $area"
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.abs(x)` | int/float | Absolute value |
| `.max(a, b)` | int/float | Larger of two numbers |
| `.min(a, b)` | int/float | Smaller of two numbers |
| `.sqrt(x)` | float | Square root |
| `.pow(b, e)` | number | `b` raised to the power `e` |
| `.floor(x)` | int | Round down |
| `.ceil(x)` | int | Round up |
| `.round(x)` | int | Round to nearest |
| `.sin(x)` `.cos(x)` `.tan(x)` | float | Trigonometric functions (radians) |
| `.log(x)` | float | Natural logarithm |
| `.log10(x)` | float | Base-10 logarithm |
| `.pi()` | float | The constant pi |
| `.e()` | float | The constant e |

### `str` - string helpers

The `str` module groups string operations as plain functions. They mirror the string
methods but are handy when you prefer a functional style or are converting a value
first.

```ran
import "std::str" as str

fn main() {
    echo str.from(42)                    # "42"   (any value -> string)
    echo str.upper("hello")              # "HELLO"
    echo str.lower("HELLO")              # "hello"
    echo str.trim("  hi  ")              # "hi"
    echo str.len("hello")                # 5
    echo str.contains("hello world", "world")   # true
    echo str.replace("a-b-c", "-", "+")  # "a+b+c"
    echo str.split("a,b,c", ",")         # ["a", "b", "c"]
    echo str.join(["a", "b", "c"], "-")  # "a-b-c"
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.from(x)` | str | Convert any value to a string |
| `.upper(s)` | str | Uppercased copy |
| `.lower(s)` | str | Lowercased copy |
| `.trim(s)` | str | Whitespace removed from both ends |
| `.len(s)` | int | Number of bytes |
| `.contains(s, sub)` | bool | Whether `sub` appears in `s` |
| `.replace(s, from, to)` | str | Replace `from` with `to` |
| `.split(s, delim)` | array | Split into an array of parts |
| `.join(array, sep)` | str | Join an array into a string |

### `rand` - random numbers

```ran
import "std::rand" as rand

fn main() {
    let dice = rand.int(1, 7)    # int in [1, 7) -> 1..6
    echo "rolled: $dice"

    let r = rand.float()         # float in [0.0, 1.0)
    echo "ratio: $r"

    let coin = rand.bool()       # true or false
    echo "coin: $coin"
}
```

| Method | Returns | Description |
|--------|---------|-------------|
| `.int(lo, hi)` | int | Random int in `[lo, hi)` (lo inclusive, hi exclusive) |
| `.float()` | float | Random float in `[0.0, 1.0)` |
| `.bool()` | bool | Random `true` or `false` |

> Not cryptographic: `rand` uses a time-seeded xorshift generator. It is fine for
> games, sampling, jitter, and test data, but do **not** use it for tokens, passwords,
> keys, or anything security-sensitive.

### `html`

```ran
import "std::html" as html

html.render(template)     # interpolate $var names in a template string
```

> Status note: `html.render` is **partial** - it only performs `$var` / `${var}`
> interpolation against your variables. It is not a full template engine (no loops,
> conditionals, or expression evaluation).

## Tips and gotchas

- **Interpolation is variable-only** everywhere, including `echo` and `html.render`.
  Compute values into variables first.
- **Import stdlib modules with an alias** before using them (`import "std::fs" as fs`); the
  alias is mandatory.
- **`echo` prints escapes literally**; use `echo -e` to interpret `\n`, `\t`, `\r`.
- **`len` on strings counts bytes**, not characters.
- **`push`, `set`, and `get` take the collection first**, e.g. `push(nums, 3)`.
- **`time.sleep` is in milliseconds.** One second is `time.sleep(1000)`.
- **`fs.read` returns contents directly**; check `fs.exists` first to avoid errors.
- **`json.decode` returns real maps and arrays**; access object fields with
  `data["key"]`.
- **`math` works on ints and floats** now (`abs`, `max`, `min`, `sqrt`, `pow`, ...);
  `floor` / `ceil` / `round` return ints.
- **`rand` is not cryptographic.** Use it for games and test data, not for secrets.
- **Not-importable module names** (`net`, `sync`, `fmt`, `hardware`, `io`, `regex`)
  report `module 'name' not found`.

Next: [Syntax Reference](11-syntax-reference.md).
