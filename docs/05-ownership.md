# Ownership & Memory Safety

Ran keeps memory safe **without a garbage collector** by tracking who owns each
value and who is allowed to read or change it. Values have a single owner, are
released automatically when that owner goes out of scope, and can be *borrowed*
so a function can use them without taking ownership.

As of this release the ownership and borrow checker is **real**: it runs during
analysis and reports use-after-move, conflicting borrows, dangling references,
move-while-borrowed, and unsynchronized shared writes. To make the rollout safe
for existing code, enforcement ships behind a mode flag (see
[Rollout & migration](#rollout--migration) below).

## The core rules

1. **Every value has exactly one owner.**
2. **When the owner goes out of scope, the value is released** — no manual
   `free`, no collector pauses.
3. **Values can be borrowed without giving up ownership.**

Scalars (`int`, `float`, `bool`, `void`) are cheap and are **copied** on use, so
they are never "moved" and never trigger move errors. Everything else (`str`,
arrays, maps, structs) has a single owner and is **moved** when assigned to a new
binding or passed by value to a function.

## Move semantics

Passing a non-scalar value by value transfers ownership. After the move, the
original binding may not be used again:

```ran
fn consume(s: str) {
    echo "got $s"
}

fn main() {
    let data = "important"
    consume(data)     # `data` is moved into `consume`
    consume(data)     # error in strict mode: use of moved value `data`
}
```

To keep using a value after handing it to a function, borrow it instead of
moving it (next section), or build an independent value and pass that.

## Borrowing

A borrow lets a function use a value without taking ownership. The caller keeps
the value and can keep using it after the call.

Immutable borrow (`&`) — read-only, any number at a time:

```ran
fn greet(name: &str) {
    echo "Hello, $name!"
}

fn main() {
    let name = "Ran"
    greet(&name)                # borrow: ownership stays with `main`
    echo "Still valid: $name"   # `name` is still usable
}
```

### The borrowing rules

The checker enforces:

1. Either **one** mutable borrow **or** any number of immutable borrows at a
   time — never both at once.
2. A borrow must not outlive the value it points to (no dangling references).
3. A value may not be moved while it is borrowed.

### Mutable borrows write back to the caller

A `&mut` parameter is a real mutable borrow: changes the callee makes are
observed by the caller after the function returns. Write-back targets the
caller's binding through the lvalue you passed — a variable, an array element
(`arr[i]`), a map value (`map[k]`), or a struct field (`obj.field`):

```ran
fn bump(x: &mut int) {
    x = x + 41
}

fn main() {
    let mut counter = 1
    bump(&mut counter)
    echo "Counter: $counter"   # Counter: 42
}
```

By-value parameters are unaffected: passing `counter` (without `&mut`) gives the
function an independent copy, and the caller sees no change.

In `warn` mode, each `&mut` call site prints an informational note that write-back
occurred; in `strict` mode the borrow rules above are enforced.

> **One surface-syntax limitation:** deref-assignment through a reference
> (`*p = ...`) is not parsed yet — it reports a syntax error (`E0102`) unrelated
> to ownership checking. Use a `&mut` parameter or normal assignment instead.

## Rollout & migration

Real enforcement changes the behavior of programs that previously relied on
ownership being a no-op, so it is introduced in two stages controlled by a mode
setting.

### Choosing the mode

Pick the mode with the `--ownership` flag or the `RAN_OWNERSHIP` environment
variable (the flag wins if both are set):

```fish
# Default this release: report findings but keep running.
ran run --ownership=warn examples/ownership.ran

# Opt in to enforcement: findings become errors and abort before running.
ran run --ownership=strict examples/ownership.ran

# Same selection via the environment (fish shell):
set -x RAN_OWNERSHIP strict
ran run examples/ownership.ran
```

- **`warn` (default this release):** ownership findings are reported as
  non-fatal diagnostics and the program still runs. Use it to audit a codebase
  without breaking it.
- **`strict` (opt-in now, default next release):** the same findings become hard
  errors that abort the program **before** it runs. Use it once the warnings are
  cleared.

The recommended path is: run under `warn`, fix every finding, then switch to
`strict` (or set `RAN_OWNERSHIP=strict`) to lock it in.

### Diagnostics you may see

| Code  | Meaning                          | Typical fix |
|-------|----------------------------------|-------------|
| E0210 | use-after-move                   | Borrow with `&`, or build an independent value instead of reusing the moved one. |
| E0212 | conflicting borrows              | Narrow a borrow's lifetime so a `&mut` does not overlap other borrows. |
| E0214 | dangling reference               | Return an owned value, or shorten the borrow so it does not outlive its referent. |
| E0215 | move while borrowed              | Finish using the borrow before moving the value. |
| E0613 | unsynchronized shared write      | Wrap shared state with the `shared` API (or send it through a channel) instead of writing a captured binding directly. |

Each diagnostic carries its code, a `file:line:col` location, and a fix hint.

### Before / after patterns

**1. Reusing a value after it was moved → narrow to a borrow.**

When a function only needs to *read* its arguments, take them by borrow so the
caller keeps ownership. This is the fix applied to `examples/banking.ran`:

```ran
# Before (strict: E0210 — `alice`/`bob` moved into the first transfer,
# then used again by the second):
fn transfer(from, to, amount) { ... }
transfer(alice, bob, dec("250.75"))
transfer(bob, alice, dec("10000.00"))   # use of moved value

# After — `transfer` borrows; the caller keeps both accounts:
fn transfer(from: &Account, to: &Account, amount) { ... }
transfer(&alice, &bob, dec("250.75"))
transfer(&bob, &alice, dec("10000.00"))  # fine: nothing was moved
```

**2. Needing the updated value back → `&mut`, or return and rebind.**

A `&mut` parameter writes its changes back to the caller, so a mutating helper
can take the value by mutable borrow:

```ran
fn increment(val: &mut int) {
    val = val + 1
}

fn main() {
    let mut counter = 0
    increment(&mut counter)         # counter is now 1
    echo "Counter: $counter"
}
```

When you prefer a pure style, model the helper as a function that returns the
new value and rebind at the call site:

```ran
fn bumped(val: int) -> int { return val + 1 }
counter = bumped(counter)           # counter updated by rebinding
```

**3. Writing shared state from `spawn` → use the `shared` API.**

Writing a captured binding from inside `spawn` is an unsynchronized data race
(`E0613`). Wrap the state with `shared` and mutate it through the synchronized
helpers so updates are serialized and none are lost:

```ran
# Before (strict: E0613 — captured `total` written without synchronization):
let mut total = 0
spawn {
    total = total + 1          # data race
}

# After — synchronized shared state:
import "std::concurrency" as conc

let counter = conc.shared(0)
let wg = conc.waitgroup()
conc.add(wg, 4)

let mut i = 0
while i < 4 {
    spawn {
        conc.shared_add(counter, 1)   # atomic read-modify-write
        conc.done(wg)
    }
    i = i + 1
}
conc.wait(wg)
echo "total: " + str(conc.shared_get(counter))
```

See [06 - Concurrency](06-concurrency.md) for the full `shared`/channel/wait-group API.

## Why aim for no garbage collector?

Skipping a collector is a deliberate goal that buys Ran:

- **Predictable performance** — no collector pauses.
- **Lower memory overhead** — values released as soon as their owner is gone.
- **Suitability for systems and embedded work** — see [08 - Hardware](08-hardware.md).
- **Compile-time safety** — many memory bugs become errors before the program runs.

## Tips and gotchas

- **Audit with `warn`, enforce with `strict`.** Clear the warnings first, then
  switch the mode.
- **Scalars are copied,** so `int`/`float`/`bool` never cause move errors.
- **`&mut` writes back to the caller.** A function can update a value through a
  `&mut` parameter, or you can return it and reassign: `x = f(x)`.
- **No deref-assignment yet.** `*p = ...` is a syntax error (`E0102`); use a
  `&mut` parameter or a normal assignment.
- **Share, don't capture-and-write.** Use the `shared` API or a channel for state
  touched by `spawn`.

Next: [Concurrency](06-concurrency.md).
