# Functions

Functions are the building blocks of a Ran program. They take parameters, optionally
return a value, and can call each other (including themselves).

## Declaring a function

```ran
fn name(parameters) -> return_type {
    # body
}
```

The `-> return_type` part is optional. If your function does not return a value, leave
it off.

## Functions with no parameters

```ran
fn greet() {
    echo "Hello!"
}

fn main() {
    greet()
}
```

## Parameters

Each parameter is written as `name: type`. Separate multiple parameters with commas.
Parameters are positional, and the checker enforces the argument count.

```ran
fn greet(name: str) {
    echo "Hello, $name!"
}

fn describe(name: str, age: int) {
    echo "$name is $age years old"
}

fn main() {
    greet("Ran")
    describe("Alice", 30)
}
```

> There are no default arguments. Passing the wrong number of arguments is an error
> (`error[E0003]`). **Closures** (anonymous function values) *are* supported — see
> below.

## Closures (anonymous functions)

`fn(params) { ... }` is a first-class value: it captures the variables visible where
it is defined, and can be stored in a variable, passed as an argument, and returned
from another function.

```ran
fn apply(g, n) -> int {
    return g(n)
}

fn make_adder(x: int) {
    return fn(n) { return n + x }   # captures x
}

fn main() {
    let triple = fn(v) { return v * 3 }
    echo apply(triple, 7)           # 21

    let add5 = make_adder(5)        # closure keeps x = 5
    echo add5(100)                  # 105
}
```

A closure parameter shadows a captured variable of the same name. Closures run on the
same guarded call path as named functions (recursion guard, frame model), so they are
memory-safe.

## Return values

Use `-> type` to declare the return type and `return` to send a value back.

```ran
fn add(a: int, b: int) -> int {
    return a + b
}

fn main() {
    let sum = add(10, 20)
    echo "10 + 20 = $sum"      # 10 + 20 = 30
}
```

To use a returned value in a string, store it in a variable first - string
interpolation substitutes variable names only, not function calls:

```ran
fn square(n: int) -> int {
    return n * n
}

fn main() {
    let result = square(5)
    echo "5 squared is $result"   # 5 squared is 25
}
```

## Recursion

Functions can call themselves. This is fully supported.

```ran
fn factorial(n: int) -> int {
    if n <= 1 {
        return 1
    }
    return n * factorial(n - 1)
}

fn fibonacci(n: int) -> int {
    if n <= 1 {
        return n
    }
    return fibonacci(n - 1) + fibonacci(n - 2)
}

fn main() {
    let f = factorial(5)
    let fib = fibonacci(10)
    echo "5! = $f"          # 5! = 120
    echo "fib(10) = $fib"   # fib(10) = 55
}
```

## Calling functions

Call a function by name with parentheses. Functions can be defined in any order - you
can call a function declared later in the file.

```ran
fn main() {
    let result = double(triple(4))   # 24
    echo "$result"
}

fn double(n: int) -> int {
    return n * 2
}

fn triple(n: int) -> int {
    return n * 3
}
```

## Functions and the standard library

You call built-in module functions with `module.function(...)`. They work alongside
your own functions:

```ran
import "std::fs" as fs

fn read_config() -> str {
    return fs.read("config.txt")
}

fn main() {
    if fs.exists("config.txt") {
        echo read_config()
    } else {
        echo "no config found"
    }
}
```

See [10 - Standard Library](10-stdlib.md) for everything available.

## Functions as HTTP handlers

When you build a web server, route handlers are ordinary functions referenced by
**name as a string**. The server calls them with no arguments and uses the returned
string as the response body.

```ran
import "std::http" as http

fn handle_home() -> str {
    return "<h1>Welcome</h1>"
}

fn main() {
    http.get("/", "handle_home")
    http.server(8080)
}
```

See [07 - Networking](07-networking.md) for the full picture, including how request
data reaches your handler.

## Putting it together

```ran
fn celsius_to_fahrenheit(c: int) -> int {
    return c * 9 / 5 + 32
}

fn label(temp: int) -> str {
    if temp > 85 {
        return "hot"
    } else {
        return "comfortable"
    }
}

fn main() {
    let readings = [20, 25, 35]
    for c in readings {
        let f = celsius_to_fahrenheit(c)
        let l = label(f)
        echo "${c}C = ${f}F ($l)"
    }
}
```

Notice that `f` and `l` are computed into variables before being interpolated.

## Tips and gotchas

- **The entry point is always `main()`.** Top-level statements run first, then
  `main()` is called.
- **Return types are optional.** Add `-> type` only when the function produces a value.
- **Use an explicit `return`.** There is no implicit "last expression" return.
- **Argument count is checked** (`error[E0003]`), and simple type mismatches are
  flagged (`error[E0004]`). See [12 - Error Handling](12-error-handling.md).
- **Interpolate variables, not calls.** Store a call's result in a variable, then use
  `$var` in strings.

Next: [Control Flow](04-control-flow.md).
