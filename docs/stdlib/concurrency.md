# Concurrency — `concurrency`

The `concurrency` module provides the runtime primitives for parallel work:
thread join, channels, wait groups, and synchronized shared state. The `spawn { }`
statement is built in and needs no import.

```ran
import "std::concurrency" as conc
```

This page is the API reference. For a guided tour with full examples, see
[06 - Concurrency](../06-concurrency.md).

## Threads

| Call | Description |
|------|-------------|
| `spawn { ... }` | Built-in statement; runs the block on a new OS thread. |
| `conc.last_thread()` | Handle of the most recently spawned thread (call right after `spawn`). |
| `conc.join(handle)` | Block until the thread finishes; return its value. Re-join/invalid handle → `E0612` error value; a faulting body is delivered as an error value. |

```ran
spawn { return 42 }
let h = conc.last_thread()
echo "result = " + str(conc.join(h))     # 42
```

## Channels

| Call | Description |
|------|-------------|
| `conc.chan(capacity)` | Create a channel. `capacity > 0` is bounded; `0` is rendezvous. Returns a handle. |
| `conc.send(ch, value)` | Send a value; blocks if the buffer is full. Closed channel → `E0611` error value. |
| `conc.recv(ch)` | Receive the next value; blocks while empty and open. Returns the closed indicator once drained and closed. |
| `conc.is_closed(value)` | `true` when `value` is the closed indicator from `recv`. |
| `conc.close(ch)` | Close the channel. |

Channels are FIFO with no loss or duplication.

## Wait groups

| Call | Description |
|------|-------------|
| `conc.waitgroup()` | Create a wait group; returns a handle. |
| `conc.add(wg, n)` | Add `n` to the counter. |
| `conc.done(wg)` | Decrement the counter. More `done` than `add` → `E0610` error value (no underflow). |
| `conc.wait(wg)` | Block until the counter reaches zero (returns immediately if already zero). |

## Shared state

| Call | Description |
|------|-------------|
| `conc.shared(value)` | Create synchronized shared state; returns a handle. |
| `conc.shared_get(s)` | Lock, return a copy of the value, unlock. |
| `conc.shared_set(s, value)` | Lock, store `value`, unlock. |
| `conc.shared_add(s, n)` | Atomic read-modify-write: add `n`, return the new value. |

All access is serialized through a lock, so concurrent updates are never lost.
If the lock cannot be acquired within 30 seconds, the call returns an `E0614`
error value instead of hanging.

```ran
let counter = conc.shared(0)
let wg = conc.waitgroup()
conc.add(wg, 4)

let mut i = 0
while i < 4 {
    spawn {
        conc.shared_add(counter, 1)
        conc.done(wg)
    }
    i = i + 1
}
conc.wait(wg)
echo "total = " + str(conc.shared_get(counter))   # 4
```

## Safe by construction

Writing a captured variable directly from inside `spawn` is a data race and is
rejected by the ownership checker as `E0613`. Use `shared` or a channel to move
state between threads. See [05 - Ownership](../05-ownership.md).

## Diagnostics

| Code  | Meaning |
|-------|---------|
| E0610 | wait group counter went negative |
| E0611 | send on a closed channel / dropped receiver |
| E0612 | join on an already-joined or invalid handle |
| E0613 | unsynchronized shared write captured by `spawn` |
| E0614 | shared-state lock acquisition timed out |

Errors are returned as handleable values (a map with `error: true` and a `code`).
