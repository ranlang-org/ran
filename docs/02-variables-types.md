# Variables & Types

Ran keeps variable declarations light. There are three forms — pick whichever reads
best; you can mix them freely.

## Declaring variables

Ran has three declaration forms. The rule of thumb: reach for **`var`** unless you
specifically want a guarantee.

### `var` — flexible (the everyday form)

`var` declares a binding you can reassign freely. Use it when you don't want to think
about whether the value will change — it just works:

```ran
var total = 0
var name = "Alice"
total = total + 5      # fine
```

### `let` — immutable (enforced)

`let` declares a binding that must never change. Reassigning it is a hard compile error
(`E0100`) — this is one of Ran's strictness guarantees, so use `let` when you *want* the
compiler to lock a value down:

```ran
let limit = 100
limit = 200            # error[E0100]: cannot assign to immutable `let` binding `limit`
```

### `let mut` — explicitly mutable

`let mut` is the explicit "this is mutable" form (same effect as `var`, but spells out
the intent). Handy when reviewing code where mutability matters:

```ran
let mut counter = 0
counter = counter + 1
```

### Bare assignment (shell-style)

You can also skip the keyword entirely — a bare `name = value` declares a mutable
binding, just like a shell variable. Great for top-of-file config:

```ran
port = 8080
host = "0.0.0.0"
```

### Which should I use?

- **`var`** — default; you don't need to reason about mutability.
- **`let`** — when a value must stay constant (the checker enforces it).
- **`let mut`** — when you want to *document* that a binding is intentionally mutable.
- **bare `name = value`** — quick scripts and top-level config.

> Coming from Rust? `let x` stays immutable (and is now enforced), `let mut x` stays
> mutable, and `var x` is a lighter everyday alternative to `let mut`.

## The built-in types

| Type | Example | Description |
|------|---------|-------------|
| `int` | `42`, `-7` | 64-bit signed integer (i64) |
| `float` | `3.14`, `-0.5` | 64-bit floating point (f64) |
| `str` | `"hello"` | UTF-8 text string |
| `decimal` | `dec("19.99")` | Exact base-10 fixed-point (money/business math) |
| `bool` | `true`, `false` | Boolean |
| array | `[1, 2, 3]` | Ordered list of values |
| map | created with `map()` | Key/value dictionary |

> **One `int`, no size juggling.** Unlike C/Rust/Go, you never pick `int8`/`int16`/
> `int32`/`int64` — there is just `int`. It is a 64-bit signed integer with **overflow
> protection**: an operation that would exceed the range stops with `error[E1010]`
> instead of silently wrapping (safer than shell, which wraps quietly). When you need
> values beyond 64 bits or exact fractional math (money, accounting), use `decimal`,
> which is exact and arbitrary-scale. (A future arbitrary-precision integer mode is on
> the roadmap; it is kept separate so the common `int` path stays native-fast.)

Check any value's type at runtime with `typeof`:

```ran
echo typeof(42)        # int
echo typeof(3.14)      # float
echo typeof("hi")      # string
echo typeof(true)      # bool
echo typeof([1, 2])    # array
```

Note that `typeof` returns `"string"` for `str` values (and `"int"`, `"float"`,
`"bool"`, `"array"`, `"map"`, `"void"` for the others).

## Type annotations

Annotations are optional - Ran infers types from literals - but you can add them, and
they are most useful on function parameters. Bash-style assignment (`name="value"`) is
always untyped; only `let` bindings take an annotation.

```ran
let name: str = "Ran"
let port: int = 8080
```

Types are optional, but **enforced when present**: if an annotation does not match the
value you assign, the checker reports a type mismatch (`error[E0004]`). See
[12 - Error Handling](12-error-handling.md).

## String interpolation

Strings can embed a **plain variable name** with `$var`, or `${var}` when you need an
explicit boundary.

```ran
name="World"
port=8080

echo "Hello, $name!"          # Hello, World!
echo "Port: ${port}"          # Port: 8080
echo "Doubled: ${port}0"      # Doubled: 80800  (braces end the name, then literal 0)
```

> Important: interpolation only substitutes **variable names**. It does **not**
> evaluate expressions or function calls inside `${ }`. Writing `"${time.now()}"` or
> `"${x * 2}"` leaves the text as-is, because there is no variable with that name.
> To interpolate a computed value, store it in a variable first:

```ran
import "std::time" as time

let now = time.now()
echo "Now: $now"              # Now: 1718000000

let doubled = port * 2
echo "Doubled: $doubled"
```

If a variable name is not found, the `$name` text is left in the string literally.

### Escape sequences

By default `echo` prints a string **literally** - escape sequences like `\n` and `\t`
are not interpreted, so the backslash and letter show up as-is:

```ran
echo "line one\nline two"     # prints: line one\nline two  (one line, literal \n)
echo "name:\tRan"             # prints: name:\tRan          (literal \t)
```

Pass the bash-style `-e` flag to interpret `\n`, `\t`, and `\r`:

```ran
echo -e "line one\nline two"  # prints two lines
echo -e "name:\tRan"          # prints a real tab
```

Quote escapes always work in any string, with or without `-e`:

```ran
echo "a quote: \""            # literal quote
echo "backslash: \\"          # literal backslash
echo "json: {\"k\": 1}"       # prints: json: {"k": 1}
```

## Numbers

```ran
let count = 42          # int
let ratio = 3.14        # float

let total = count + 8   # int arithmetic
let half = ratio / 2.0  # float arithmetic
let mixed = 3.5 + 2     # 5.5  (int is promoted to float)
```

Integers support `+ - * / %` and all comparisons. Floats support `+ - * /` and all
comparisons (`< <= > >= == !=`). You can also **mix int and float** in one expression:
the int is promoted to a float, so `3.5 + 2` is `5.5` and `2 + 3.5` is `5.5`. Division
or modulo by zero on integers yields `0`.

```ran
echo 3.5 > 2.0          # true
echo 3.5 == 3.5         # true
echo 2 + 3.5            # 5.5
echo 7 / 2              # 3   (int division stays int)
echo 7.0 / 2.0          # 3.5
```

Convert between types with the built-in `int`, `float`, and `str` functions:

```ran
let n = int("123")      # 123 (string -> int)
let f = float(5)        # 5.0 (int -> float)
let s = str(99)         # "99" (anything -> string)
```

If a string can't be parsed as a number, `int` returns `0` and `float` returns `0.0`.

## Booleans

```ran
let ready = true
let done = false

if ready {
    echo "go"
}
```

## Arrays

Arrays are ordered collections written with square brackets:

```ran
let numbers = [1, 2, 3, 4, 5]
let names = ["Alice", "Bob", "Charlie"]
let empty = []
```

Iterate over them with `for ... in`:

```ran
for n in numbers {
    echo "$n"
}
```

Index into them with `[int]`:

```ran
echo numbers[0]           # 1
```

Arrays come with useful methods (see [10 - Standard Library](10-stdlib.md) for the
full list):

```ran
let nums = [3, 1, 2]

echo nums.len()           # 3
echo nums.first()         # 3
echo nums.last()          # 2
echo nums.contains(1)     # true
echo nums.join(", ")      # 3, 1, 2
```

Add to an array with the built-in `push`:

```ran
let mut items = [1, 2]
push(items, 3)
echo items.len()          # 3
```

## Maps

Maps are key/value dictionaries. Create one with `map()`, then use `set` and `get`:

```ran
let scores = map()
set(scores, "alice", 90)
set(scores, "bob", 85)

echo get(scores, "alice")   # 90
echo scores.has("bob")      # true
echo scores.keys()          # array of keys
echo scores.values()        # array of values
echo scores.len()           # 2
```

Index into a map with a string key, too:

```ran
echo scores["alice"]        # 90
```

Field access (`obj.field`) works when the variable holds a map:

```ran
echo scores.alice           # 90
```

## Common gotchas

- **No spaces in bash-style assignment.** Write `port=8080`, not `port = 8080`. The
  `let` form is the opposite: `let port = 8080` needs the spaces.
- **Interpolation is variable-only.** `${time.now()}` and `${x + 1}` are not evaluated.
  Assign to a variable first.
- **`int` math stays `int`.** `7 / 2` is `3`. Use floats (`7.0 / 2.0`) for fractional
  results. Mixing int and float promotes to float (`3.5 + 2` is `5.5`).
- **Floats support comparisons** (`< <= > >= == !=`) and mixed int/float arithmetic.
- **`${ }` ends the variable name.** `"$portnumber"` looks for a variable named
  `portnumber`; write `"${port}number"` to interpolate `port` then literal text.

Next: [Functions](03-functions.md).
