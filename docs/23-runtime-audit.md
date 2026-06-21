# Runtime Audit: Concurrency, CPU, and Structure

A standing audit of the interpreter and server for correctness, multithreading,
and CPU behavior, plus the planned structural work. Updated as items are closed.

## 1. Multithreading audit (HTTP server)

**Model:** connections are accepted on the main thread and dispatched to a
**fixed, bounded worker pool** over a shared queue.

| Aspect | Status | Notes |
|--------|--------|-------|
| Thread count | ✅ bounded | `worker_count()` = CPUs × 32, capped 256; tune via `RAN_WORKERS` |
| Queue | ✅ bounded | `sync_channel` applies backpressure (excess waits in OS accept backlog) |
| Lock scope | ✅ minimal | the receiver mutex is held only to dequeue, never while serving |
| Worker identity | ✅ named | `ran-worker-N` for debugging/observability |
| Shutdown | ⚠️ basic | workers exit when the queue closes; no graceful drain yet |
| Per-request isolation | ✅ mitigated | handler faults are caught → `500`; see below |

### ✅ Mitigated: a failing request no longer takes down the server
Runtime faults now **unwind** to a catch boundary (release profile uses
`panic = "unwind"`), and the HTTP dispatcher wraps each handler in
`catch_fault`. A fault (overflow, divide-by-zero, bad decimal, missing
`env.require`, ...) in one handler returns `500` and is logged; the server keeps
serving. Verified live.

**Remaining (future):** full `Result`-threaded evaluation so **user code** can
`try`/recover from errors (today recovery happens only at the server and
top-level boundaries). `assert`/`exit`/`log.fatal` remain intentional
terminations.

### CPU behavior
- Worker threads are appropriate for the **blocking I/O** the server does
  (threads park on socket reads). Over-subscription (×32) is fine for I/O-bound
  handlers; for CPU-bound handlers it would be too many — but interpreted
  handlers are rarely CPU-bound. The 256 cap prevents pathological counts.
- The interpreter itself is single-threaded per request; `spawn` starts extra
  OS threads and clones environment state (cheap correctness over shared-memory
  complexity). True parallel data sharing (channels, atomics) is future work.

### Smaller hardening already applied
- Request line, header line, header count, and total header bytes are capped.
- HTTP client response is capped at 64 MB.

## 2. Documentation audit

- README, `docs/00-introduction.md`, `docs/19-why-ran.md`, and
  `docs/18-interop-and-ecosystem.md` rewritten as **standalone, internal-use**
  documents; comparative "like language X" framing removed.
- Money/config/error docs reworded to not name other languages directly.
- Internal-use banners added (README, introduction).
- Remaining: a line-by-line pass over the deeper chapters (06, 07, 09, 14, 17)
  still contains incidental shell-fence labels and accurate implementation notes
  (e.g. "the compiler is written in Rust" — a true architectural fact, kept).
  These are low-impact and tracked for a follow-up editorial pass.

## 3. Code structure / file split (in progress)

`src/runtime/mod.rs` was ~4.9k lines. It is being split into a `runtime/` module
tree (no behavior change; child modules carry parts of the `Environment`
inherent impl and reach parent privates via `use super::*`, with cross-module
entry points marked `pub(super)`).

Done (each landed with the full test suite green):
```
runtime/
├── mod.rs              # Value, Flow, Environment, execute/run_tests, interp core, helpers
├── json.rs             # JSON parse/validate engine
├── module_dispatch.rs  # call_module_method (http/web/db/concurrency/fs/...) dispatch
├── server.rs           # built-in HTTP/web server wiring
├── builtins.rs         # call_function (builtins) + call_method (value methods)
├── frame.rs            # scope/frame engine: indexed local frames + shared globals,
│                       #   variable access, call-stack frames (the slot model)
└── helpers/            # microkernel-style: one concern per file (not a monolith)
    ├── mod.rs
    ├── concurrency.rs  # concurrency handle/error value constructors
    └── db.rs           # SQLite value mapping + handleable errors
```
`mod.rs` went from ~4.9k to ~2.9k lines (incl. the test modules).

Remaining candidates (future passes, same mechanical recipe):
- Move the concurrency/db error-value helpers into `runtime/value_helpers.rs`.
- Split `exec_statement`/`eval_expression` into `runtime/interp.rs`.
- Relocate the `#[cfg(test)]` modules (keeping them direct children of `runtime`
  so their `use super::*` still resolves).
- Secondary large files: `stdlib/net.rs`, `support/sqlite_ffi.rs`,
  `semantics/types.rs`, `frontend/parser.rs`.

## 4. Language correctness / modernization (planned, each its own pass)

| Item | Why it matters | Risk |
|------|----------------|------|
| ~~Lexical block scoping~~ ✅ done | Scope stack: block/loop locals no longer leak; functions see globals+params only; assignment mutates the defining scope | — |
| **Error model (`Result`)** | Full propagation so user code can `try`/recover (server + top-level recovery already done via unwind/catch). | High |
| **Bytecode VM** | Performance toward native; the VM exists but is not wired in. Staged plan in `docs/16-roadmap.md`. | High |

Recommended order: error model → lexical scoping → enum/match → VM. Each lands
behind the existing 61-test suite (plus new tests) before becoming default.
