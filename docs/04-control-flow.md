# Control Flow

Control flow decides which code runs and how many times. Ran keeps it simple:
`if`/`else`, `for` loops over arrays, and `while` loops. Conditions never use
parentheses, and blocks always use curly braces.

## If / Else

```ran
if condition {
    # runs when condition is true
} else {
    # runs otherwise
}
```

No parentheses around the condition, and the braces are required:

```ran
let x = 15

if x > 10 {
    echo "big"
} else {
    echo "small"
}
```

### If without else

The `else` branch is optional:

```ran
let ready = true

if ready {
    echo "starting up"
}
```

### Nested conditions

To build a chain of conditions, nest `if`/`else`:

```ran
fn main() {
    let score = 85

    if score > 90 {
        echo "Grade: A"
    } else {
        if score > 80 {
            echo "Grade: B"
        } else {
            echo "Grade: C"
        }
    }
}
```

## Comparison operators

Use these inside conditions. They work on integers, floats, and strings:

| Operator | Meaning |
|----------|---------|
| `==` | equal |
| `!=` | not equal |
| `<` | less than |
| `<=` | less than or equal |
| `>` | greater than |
| `>=` | greater than or equal |

Integers and floats compare numerically; strings compare lexicographically
(`"abc" < "abd"` is `true`). `==` and `!=` also work on bools (`true == true`).

```ran
if 3.5 > 2.0 {
    echo "floats compare"
}

if "apple" < "banana" {
    echo "strings compare lexicographically"
}
```

## Logical operators

The `!` (not), `&&` (and), and `||` (or) operators all work and operate on truthiness:

```ran
let age = 20
let member = true

if age >= 18 && member {
    echo "access granted"
}

if age < 13 || age > 65 {
    echo "discounted"
}

if !member {
    echo "please sign up"
}
```

> Note: `&&` and `||` **short-circuit**. The right side is evaluated only when it can
> change the result — `&&` skips the right side when the left is false, and `||` skips
> it when the left is true. This means you can safely use the left side to guard the
> right, for example `i < n && arr[i] > 0` will not read `arr[i]` when `i < n` is false:

```ran
fn first_positive(a: [int], n: int) -> int {
    let i = 0
    while i < n && a[i] > 0 {
        i = i + 1
    }
    return i
}
```

## For loops

`for ... in` iterates over the elements of an **array**. To count a fixed number of
times, use the built-in `range`, which returns an array. `for` does not iterate over
strings or maps directly.

```ran
for item in [1, 2, 3, 4, 5] {
    echo "$item"
}

for name in ["Alice", "Bob", "Charlie"] {
    echo "Hello, $name"
}
```

You can iterate over a variable that holds an array:

```ran
let tasks = ["build", "test", "deploy"]

for task in tasks {
    echo "Running: $task"
}
```

### Looping a fixed number of times

`range(n)` builds `[0, 1, ..., n-1]`, and `range(a, b)` builds `[a, ..., b-1]`, so it
is the idiomatic counting loop:

```ran
for i in range(5) {
    echo "Attempt $i"        # 0, 1, 2, 3, 4
}

for i in range(1, 4) {
    echo "Step $i"           # 1, 2, 3
}
```

## While loops

A `while` loop runs as long as its condition is true:

```ran
let mut n = 5
while n > 0 {
    echo "$n"
    n = n - 1
}
echo "Go!"
```

Output:

```
5
4
3
2
1
Go!
```

### Accumulating a result

```ran
fn main() {
    let mut total = 0
    let mut i = 1

    while i <= 10 {
        total = total + i
        i = i + 1
    }

    echo "Sum 1..10 = $total"   # Sum 1..10 = 55
}
```

## Combining loops and conditions

```ran
fn main() {
    let numbers = [4, 7, 10, 13, 16]

    for n in numbers {
        if n % 2 == 0 {
            echo "$n is even"
        } else {
            echo "$n is odd"
        }
    }
}
```

## Breaking and continuing

`break` stops the innermost loop immediately; `continue` skips to the next
iteration. Both work in `for` and `while` loops and correctly propagate out of
nested `if`/blocks inside the loop.

```ran
fn main() {
    let mut sum = 0
    for i in range(10) {
        if i == 5 { break }        # stop the loop at 5
        if i % 2 == 0 { continue } # skip even numbers
        sum = sum + i              # 1 + 3 = 4
    }
    echo "sum = $sum"              # sum = 4
}
```

In nested loops, `break`/`continue` affect only the innermost loop:

```ran
for a in [1, 2, 3] {
    for b in [1, 2, 3] {
        if b == 2 { break }   # breaks the inner loop only
        echo "$a-$b"
    }
}
```

A `return` inside a loop (or inside a `match` arm within a loop) unwinds the whole
enclosing function, not just the loop.

## Tips and gotchas

- **Braces are mandatory.** There is no single-line `if x > 0 echo "hi"`.
- **No parentheses on conditions.** Write `if x > 10 { }`, not `if (x > 10) { }`.
- **`&&`, `||`, and `!` all work**, and `&&` / `||` **short-circuit** - the right
  side is evaluated only when it can change the result, so the left side can safely
  guard the right (e.g. `i < n && a[i] > 0`).
- **Comparisons work on ints, floats, and strings** (`==` / `!=` also on bools).
- **`for` iterates arrays** - use `range(n)` to count, and `while` for custom steps.
- **Advance your `while` counter** (e.g. `n = n - 1`) or the loop never ends.
- **Use `let mut` for loop counters.** A plain `let` is meant to stay constant.

Next: [Ownership & Memory Safety](05-ownership.md).
