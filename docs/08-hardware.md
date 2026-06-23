# Hardware & Embedded

One of Ran's long-term goals is to run close to the metal: GPIO pins, memory-mapped
registers, serial ports, and raw system calls. Because Ran has no garbage collector and
produces a compact native binary, it is a plausible fit for embedded and systems work.

> Status note: the hardware layer is **library-only**. Code for GPIO, MMIO, serial, and
> syscalls exists in `src/stdlib/hardware.rs`, but `hardware` is **not exposed to Ran**
> as an importable module. As of v0.3.8, `import "hardware" as hardware` reports
> `module 'hardware' not found` - the Rust code is never reachable from `.ran` code.
> This chapter documents the intended model and what you can actually build today. See
> [16 - Roadmap](16-roadmap.md).

## What works today

You cannot drive real hardware from `.ran` code yet. What you *can* do is prototype the
control flow and timing of an embedded program using the parts of the language that are
fully working - loops, functions, and `time`:

```ran
# Simulate a sensor polling loop (runs today as a plain script)
import "std::time" as time

fn main() {
    let mut tick = 0
    while tick < 5 {
        let ts = time.now()
        echo "Reading sensor (tick $tick) at $ts"
        time.sleep(1000)
        tick = tick + 1
    }
    echo "Polling complete"
}
```

A "blink" program today is really just print-and-sleep - the pin calls are not
connected:

```ran
import "std::time" as time

LED_PIN=13

fn main() {
    echo "Simulating blink on pin $LED_PIN"

    for i in [1, 2, 3, 4, 5] {
        echo "Blink $i: on"
        time.sleep(500)
        echo "Blink $i: off"
        time.sleep(500)
    }

    echo "Done"
}
```

The `examples/hardware.ran` example runs exactly like this - as an ordinary script,
with no real hardware access.

## The intended model (design goal)

The rest of this chapter describes the capabilities that exist in the library and are
planned to be exposed to `.ran` code.

### GPIO

The hardware layer models a pin with a number and a mode, supporting:

- **Modes:** input, output, pull-up, pull-down
- **States:** high, low
- **Operations:** read, write, toggle

### Memory-mapped I/O

For direct register access, the library provides volatile 32-bit reads and writes at a
base address plus an offset:

- A register bound to a base address
- `read32(offset)` - a volatile 32-bit read
- `write32(offset, value)` - a volatile 32-bit write

This is inherently unsafe (raw memory access) and is intended for an `unsafe` context.

### Serial / UART

The serial interface models a UART connection defined by a device path and a baud rate,
for example `/dev/ttyUSB0` at `9600` baud.

### System calls

On Linux x86_64, the library can issue raw system calls (via the `syscall`
instruction), passing a syscall number and up to three arguments. On other platforms it
returns an error sentinel. Like MMIO, raw syscalls are unsafe.

## Target platforms (planned)

| Platform | Status |
|----------|--------|
| Linux x86_64 | Primary target for the toolchain |
| Linux ARM64 | Planned |
| Cortex-M (STM32) | Planned |
| ESP32 | Planned |
| RISC-V | Planned |

## Tips and gotchas

- **`hardware` is not importable.** `import "hardware" as hardware` errors with
  `module 'hardware' not found`; the bindings are not exposed to Ran yet.
- **Prototype the logic now.** Build and test control flow with `time`, loops, and
  functions, and swap in hardware calls when the bindings land.
- **`time.sleep` is in milliseconds.** Use it to pace blink loops and polling.

Next: [Compilation](09-compilation.md).
