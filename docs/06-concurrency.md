# Concurrency

Ran runs work in parallel with real OS threads and gives you safe tools to
coordinate them: thread join, channels, wait groups, and synchronized shared
state. The primitives live in the `concurrency` module, imported with an alias:

```ran
import "std::concurrency" as conc
```

`spawn { }` is a built-in statement and needs no import.

## The `spawn` block

Wrap a block in `spawn { }` and it runs on its own OS thread while the rest of
your program keeps going:

```ran
import "std::time" as time

fn main() {
    spawn {
        echo "running in the background"
    }

    echo "main keeps going"
    time.sleep(100)
}
```

`spawn` starts a new thread immediately and does **not** wait for it. The thread
receives a clone of the current environment, so it can read what was defined
before it.

## Joining a thread for its result

To wait for a spawned thread and collect its return value, grab a handle with
`conc.last_thread()` right after the `spawn`, then `conc.join(handle)`:

```ran
import "std::concurrency" as conc

fn main() {
    spawn {
        return 42
    }
    let h = conc.last_thread()
    let result = conc.join(h)      # blocks until the thread finishes
    echo "result = $result"        # result = 42
}
```

`join` returns the thread's value. Two error cases come back as handleable error
values (a map with `error: true` and a `code`), never a crash:

- Re-joining the same handle, or an invalid handle, yields `E0612`.
- A thread whose body faults (e.g. divide-by-zero) delivers the fault to the
  joiner as an error value, and the process keeps running.

```ran
spawn {
    let z = 0
    return 10 / z          # faults inside the thread
}
let h = conc.last_thread()
let r = conc.join(h)
if r["error"] {
    echo "thread failed with " + r["code"]   # E1002
}
```

## Channels

A channel carries values between threads in FIFO order, with no loss or
duplication. Create one with `conc.chan(capacity)`:

- `capacity > 0` — a **bounded** buffer; `send` blocks when full.
- `capacity == 0` — a **rendezvous** channel; each `send` waits for a `recv`.

```ran
import "std::concurrency" as conc

fn main() {
    let ch = conc.chan(4)          # bounded buffer of 4

    spawn {
        let mut i = 1
        while i <= 3 {
            conc.send(ch, i)
            i = i + 1
        }
        conc.close(ch)             # no more values will be sent
    }

    let h = conc.last_thread()

    # Drain until the channel is closed and empty.
    let mut done = false
    while !done {
        let v = conc.recv(ch)
        if conc.is_closed(v) {
            done = true
        } else {
            echo "got $v"
        }
    }
    conc.join(h)
}
```

Channel operations:

| Call | Behavior |
|------|----------|
| `conc.chan(capacity)` | Create a channel; returns a handle. |
| `conc.send(ch, value)` | Send a value; blocks if a bounded buffer is full. Sending on a closed channel returns a handleable `E0611` error value. |
| `conc.recv(ch)` | Receive the next value; blocks while empty and open. Returns a distinguishable closed indicator once all senders are closed and the buffer is drained. |
| `conc.is_closed(value)` | `true` when `value` is the closed indicator returned by `recv`. |
| `conc.close(ch)` | Close the channel; pending receivers drain the buffer, then see the closed indicator. |

## Wait groups

A wait group waits for a set of threads to finish without collecting their
results. Add the expected count, have each worker call `done`, and `wait` for
the counter to reach zero:

```ran
import "std::concurrency" as conc
import "std::time" as time

fn main() {
    let wg = conc.waitgroup()
    conc.add(wg, 3)

    let mut i = 1
    while i <= 3 {
        spawn {
            time.sleep(50)
            echo "worker done"
            conc.done(wg)
        }
        i = i + 1
    }

    conc.wait(wg)          # blocks until all three call done()
    echo "all workers completed"
}
```

`wait` returns immediately when the counter is already zero. Calling `done` more
often than `add` drives the counter negative and returns a handleable `E0610`
error value (the counter does not underflow).

## Synchronized shared state

To share mutable state across threads safely, wrap it with `conc.shared(value)`.
Every thread that holds the handle refers to the same value, and all access is
serialized through a lock, so concurrent updates are never lost:

```ran
import "std::concurrency" as conc

fn main() {
    let counter = conc.shared(0)
    let wg = conc.waitgroup()
    conc.add(wg, 4)

    let mut i = 0
    while i < 4 {
        spawn {
            let mut j = 0
            while j < 250 {
                conc.shared_add(counter, 1)   # atomic read-modify-write
                j = j + 1
            }
            conc.done(wg)
        }
        i = i + 1
    }

    conc.wait(wg)
    let total = conc.shared_get(counter)
    echo "counter total = $total"             # 1000, every time
}
```

Shared-state operations:

| Call | Behavior |
|------|----------|
| `conc.shared(value)` | Create synchronized shared state; returns a handle. |
| `conc.shared_get(s)` | Acquire the lock, return a copy of the value, release. |
| `conc.shared_set(s, value)` | Acquire the lock, store `value`, release. |
| `conc.shared_add(s, n)` | Atomic read-modify-write: add `n` and return the new value. |

Lock acquisition uses a deadline; if it cannot acquire within 30 seconds it
returns a handleable `E0614` error value instead of hanging the process.

## Why writing captured variables is rejected

Writing a captured binding directly from inside `spawn` is an unsynchronized
data race. The ownership checker reports it as `E0613`:

```ran
let mut total = 0
spawn {
    total = total + 1      # E0613 — data race
}
```

Use `shared` (above) or a channel instead. See [05 - Ownership](05-ownership.md)
for the full rule and migration patterns.

## Coordinating with time

The `time` module helps pace and stagger work. `time.sleep` takes
**milliseconds**:

```ran
import "std::time" as time

time.sleep(1000)        # pause this thread for one second
let ts = time.now()     # current Unix timestamp in seconds
echo "Timestamp: $ts"
```

Store `time.now()` in a variable before interpolating it — `${time.now()}` is
not evaluated inside a string.

## Design notes

- **`spawn` uses OS threads.** Each spawned block is a genuine thread scheduled
  by the operating system.
- **Output ordering is not guaranteed.** Concurrent threads interleave.
- **Prefer join / wait groups over `time.sleep`** for correctness. Use `join`
  when you need a result, a wait group when you only need completion.
- **Share through `shared` or channels**, never by writing captured variables.

## Diagnostics

| Code  | Meaning |
|-------|---------|
| E0610 | wait group counter went negative (`done` called more often than `add`) |
| E0611 | send on a closed channel / dropped receiver |
| E0612 | join on an already-joined or invalid thread handle |
| E0613 | unsynchronized shared write captured by `spawn` (ownership checker) |
| E0614 | shared-state lock acquisition timed out |

Each carries its code, a `file:line:col` location, and a fix hint.

Next: [Networking](07-networking.md).
