//! Concurrency module - Go-style concurrency primitives.
//! Provides spawn (goroutines), channels, select, and WaitGroup.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, TryLockError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::runtime::Value;

/// Create a new unbuffered channel pair
pub fn channel<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = mpsc::channel();
    (Sender { inner: tx }, Receiver { inner: rx })
}

/// Channel sender (cloneable, like Go channels)
pub struct Sender<T> {
    inner: mpsc::Sender<T>,
}

impl<T> Sender<T> {
    pub fn send(&self, value: T) -> Result<(), mpsc::SendError<T>> {
        self.inner.send(value)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            inner: self.inner.clone(),
        }
    }
}

/// Channel receiver
pub struct Receiver<T> {
    inner: mpsc::Receiver<T>,
}

impl<T> Receiver<T> {
    pub fn recv(&self) -> Result<T, mpsc::RecvError> {
        self.inner.recv()
    }

    pub fn try_recv(&self) -> Result<T, mpsc::TryRecvError> {
        self.inner.try_recv()
    }
}

/// Spawn a new concurrent task (like Go's goroutine)
pub fn spawn<F>(f: F) -> thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    thread::spawn(f)
}

// ============================================================================
// Ran-level channels (carry runtime `Value`s across threads)
// ============================================================================
//
// Channels are exposed to the Ran language via the `concurrency` module
// (`chan`, `send`, `recv`, `close`). Because `spawn` clones the interpreter
// Environment per thread, the channel endpoints cannot live in the Environment
// itself — a clone would produce two *different* mpsc instances and break
// cross-thread delivery. Instead the real endpoints live in a single process-
// global registry keyed by an opaque `u64` handle id; the Ran value handed to
// the program is just that id (`Value::Int`), so every thread that holds the
// id refers to the *same* underlying channel.
//
// Bounded channels use `mpsc::sync_channel(capacity)`; `capacity == 0` is a
// rendezvous channel (every send blocks until a receiver takes the value).

/// One registered channel: the (single) sending endpoint and the receiving
/// endpoint, each behind its own lock so a blocking `recv` never holds the
/// registry lock and a blocking `send` never blocks receivers.
struct ChannelEntry {
    /// `Some` while the channel is open; set to `None` by `close` so receivers
    /// observe a distinguishable closed state once the buffer drains.
    tx: Mutex<Option<mpsc::SyncSender<Value>>>,
    rx: Mutex<mpsc::Receiver<Value>>,
}

fn channel_registry() -> &'static Mutex<HashMap<u64, Arc<ChannelEntry>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, Arc<ChannelEntry>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_channel_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

fn lookup_channel(id: u64) -> Option<Arc<ChannelEntry>> {
    crate::stdlib::lock_or_fault(channel_registry(), "channel registry").get(&id).cloned()
}

/// Outcome of a `send` on a Ran channel.
#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// The value was delivered (or buffered) successfully.
    Ok,
    /// The channel was closed or every receiver endpoint was dropped; the value
    /// was not delivered. Maps to diagnostic `E0611`.
    Closed,
    /// The handle does not refer to a live channel.
    InvalidHandle,
}

/// Outcome of a `recv` on a Ran channel.
#[derive(Debug)]
pub enum RecvOutcome {
    /// A value was received.
    Value(Value),
    /// All senders are closed and the buffer is drained — the distinguishable
    /// closed indicator (R11.4). An invalid handle is also reported as closed.
    Closed,
}

/// Create a channel with the given buffer capacity and register it.
///
/// `capacity == 0` yields a rendezvous channel; `capacity > 0` yields a bounded
/// buffered channel. Returns the opaque handle id (R11.1, R11.6).
pub fn chan_create(capacity: usize) -> u64 {
    let (tx, rx) = mpsc::sync_channel::<Value>(capacity);
    let id = next_channel_id();
    let entry = Arc::new(ChannelEntry {
        tx: Mutex::new(Some(tx)),
        rx: Mutex::new(rx),
    });
    crate::stdlib::lock_or_fault(channel_registry(), "channel registry").insert(id, entry);
    id
}

/// Send a value on the channel. Blocks while a bounded buffer is full until
/// space is available or the channel closes (R11.2, R11.6). Returns
/// `SendOutcome::Closed` when the channel was closed or the receiver was
/// dropped (R11.7).
pub fn chan_send(id: u64, value: Value) -> SendOutcome {
    let entry = match lookup_channel(id) {
        Some(e) => e,
        None => return SendOutcome::InvalidHandle,
    };
    // Hold the sender lock (not the registry lock) across the potentially
    // blocking send so receivers and other channels stay responsive.
    let guard = crate::stdlib::lock_or_fault(&entry.tx, "channel sender");
    match guard.as_ref() {
        Some(tx) => match tx.send(value) {
            Ok(()) => SendOutcome::Ok,
            Err(_) => SendOutcome::Closed, // receiver endpoint dropped
        },
        None => SendOutcome::Closed, // sender explicitly closed
    }
}

/// Receive a value from the channel. Blocks while the channel is empty and open
/// until a value arrives or the channel closes (R11.3). Returns
/// `RecvOutcome::Closed` once all senders are closed and the buffer is drained
/// (R11.4).
pub fn chan_recv(id: u64) -> RecvOutcome {
    let entry = match lookup_channel(id) {
        Some(e) => e,
        None => return RecvOutcome::Closed,
    };
    let rx = crate::stdlib::lock_or_fault(&entry.rx, "channel receiver");
    match rx.recv() {
        Ok(v) => RecvOutcome::Value(v),
        Err(_) => RecvOutcome::Closed, // all senders dropped, buffer empty
    }
}

/// Close the sending endpoint of a channel. After close, buffered values can
/// still be received; once drained, `recv` reports `Closed` and any further
/// `send` reports `Closed` (R11.4, R11.7). Returns `false` for an unknown
/// handle.
pub fn chan_close(id: u64) -> bool {
    match lookup_channel(id) {
        Some(entry) => {
            // Drop the only sender so the receiver observes the closed state.
            *crate::stdlib::lock_or_fault(&entry.tx, "channel sender") = None;
            true
        }
        None => false,
    }
}

// ============================================================================
// Ran-level thread handles (spawn -> handle, join -> result value)
// ============================================================================
//
// `spawn { }` runs an OS thread whose closure returns a runtime `Value` (the
// thread body's result, or a caught `RuntimeFault` rendered as an error map —
// see runtime). The `JoinHandle<Value>` lives in a single process-global
// registry keyed by an opaque `u64`; the Ran program receives that id (an
// opaque `Int` handle, per the design "Model handle (Thread)"). `join(id)`
// takes the handle out of the registry and blocks on it, so a second `join`
// (or an unknown id) is rejected with `E0612` instead of blocking forever.

/// One registered spawned thread. `handle` is `Some` until the thread is joined
/// (either explicitly via `join_thread` or at shutdown via
/// `join_all_remaining_threads`), after which a re-join observes `None`.
struct ThreadEntry {
    handle: Option<thread::JoinHandle<Value>>,
}

fn thread_registry() -> &'static Mutex<HashMap<u64, ThreadEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, ThreadEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_thread_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Outcome of a `join` on a Ran thread handle.
pub enum JoinOutcome {
    /// The thread finished; this is its result value (which may itself be an
    /// error map produced by a caught `RuntimeFault` — R12.6).
    Value(Value),
    /// The handle was already joined or never existed — maps to `E0612`.
    Invalid,
    /// The thread ended with a genuine (non-fault) panic.
    Panicked,
}

/// Register a spawned thread's join handle and return a fresh opaque id (R12.1).
pub fn register_thread(handle: thread::JoinHandle<Value>) -> u64 {
    let id = next_thread_id();
    crate::stdlib::lock_or_fault(thread_registry(), "thread registry")
        .insert(id, ThreadEntry { handle: Some(handle) });
    id
}

/// Spawn a thread whose body produces a runtime `Value`, capturing any
/// `RuntimeFault` raised inside that body so a faulting thread is delivered to
/// its joiner as an **inspectable error value** instead of crashing the process
/// (R3.6 / R12.6).
///
/// The body is wrapped with the runtime's `catch_fault` (the same panic-as-
/// unwind boundary used by the top-level runner and the per-request server). On
/// a caught fault the `Result<Value, RuntimeFault>` is converted through the
/// existing `fault_to_value` path into a Ran error map
/// (`{ "error": true, "code": <code>, "message": <message> }`) that the joiner
/// can inspect. A normal (non-faulting) body returns its `Value` unchanged. The
/// resulting handle is registered and its opaque join id is returned (R12.1), so
/// `join_thread` later hands the joiner either the real result or the captured
/// error value — never a process crash.
///
/// A genuine (non-`RuntimeFault`) panic is *not* swallowed here: `catch_fault`
/// re-raises it, and `join_thread` surfaces it as `JoinOutcome::Panicked`.
pub fn spawn_value<F>(f: F) -> u64
where
    F: FnOnce() -> Value + Send + 'static,
{
    let handle = thread::spawn(move || -> Value {
        match crate::runtime::catch_fault(f) {
            Ok(v) => v,
            Err(fault) => crate::runtime::fault_to_value(&fault),
        }
    });
    register_thread(handle)
}

/// Join the thread referred to by `id`, blocking until it finishes and
/// returning its result value (R12.2). A handle that was already joined or that
/// never existed yields `JoinOutcome::Invalid` (→ `E0612`, R12.3) without
/// blocking. A genuine panic yields `JoinOutcome::Panicked`.
pub fn join_thread(id: u64) -> JoinOutcome {
    // Take the handle out under the lock so a concurrent/second join sees None,
    // then release the lock before the potentially blocking `join`.
    let handle = {
        let mut reg = crate::stdlib::lock_or_fault(thread_registry(), "thread registry");
        match reg.get_mut(&id) {
            Some(entry) => entry.handle.take(),
            None => None,
        }
    };
    match handle {
        Some(h) => match h.join() {
            Ok(v) => JoinOutcome::Value(v),
            Err(_) => JoinOutcome::Panicked,
        },
        None => JoinOutcome::Invalid,
    }
}

/// Join every still-unjoined spawned thread. Called once at program exit to
/// preserve the previous shutdown-join behavior (threads are awaited before the
/// process exits) now that handles live in the global registry rather than on
/// the interpreter `Environment`.
pub fn join_all_remaining_threads() {
    let handles: Vec<thread::JoinHandle<Value>> = {
        let mut reg = crate::stdlib::lock_or_fault(thread_registry(), "thread registry");
        reg.values_mut().filter_map(|e| e.handle.take()).collect()
    };
    for h in handles {
        let _ = h.join();
    }
}

/// WaitGroup — wait for a set of spawned tasks to complete.
///
/// The counter is **signed** (`i64`) rather than an `AtomicUsize` so that the
/// "more `done` than `add`" case (which would drive the counter below zero) is
/// *detectable* instead of silently wrapping. `done` guards the decrement: when
/// the counter is already at zero it refuses to go negative and reports the
/// condition to the caller, which maps it to diagnostic `E0610` (R12.5).
///
/// The counter + `Condvar` live behind a single `Arc` so the value can be
/// shared across threads (and stored in the process-global registry) while
/// `wait` blocks without holding any registry lock.
pub struct WaitGroup {
    inner: Arc<(Mutex<i64>, Condvar)>,
}

impl WaitGroup {
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(0), Condvar::new())),
        }
    }

    /// Cheap clone that shares the same underlying counter/condvar.
    pub fn clone_handle(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }

    /// Add `n` to the counter (R12.4). `n` is expected to already be a sane
    /// non-negative count (the runtime clamps it to `0..=65535`).
    pub fn add(&self, n: i64) {
        let (lock, cvar) = &*self.inner;
        // NOTE (task 3.2): deliberately NOT routed through `lock_or_fault`.
        // This mutex is Condvar-paired; the paired `cvar.wait(..)` in `wait`
        // also unwraps the lock result, and poison recovery here would leave
        // the counter/condvar invariant inconsistent. Keep the std unwrap so a
        // genuine poisoning surfaces rather than silently continuing.
        let mut count = lock.lock().unwrap();
        *count += n;
        // Adding zero (or arriving back at zero) should wake any waiters that
        // are blocked on an already-empty group.
        if *count <= 0 {
            cvar.notify_all();
        }
    }

    /// Decrement the counter by one. Returns `false` when the counter is already
    /// at zero — decrementing would make it negative, so the decrement is
    /// refused (no underflow) and the caller reports `E0610` (R12.5). On success
    /// returns `true`, and notifies waiters when the counter reaches zero.
    pub fn done(&self) -> bool {
        let (lock, cvar) = &*self.inner;
        // NOTE (task 3.2): Condvar-paired mutex — see `add`. Deliberately not
        // converted to `lock_or_fault` to preserve the counter/condvar invariant.
        let mut count = lock.lock().unwrap();
        if *count <= 0 {
            return false; // would go negative -> E0610, do NOT underflow
        }
        *count -= 1;
        if *count == 0 {
            cvar.notify_all();
        }
        true
    }

    /// Block until the counter reaches zero. Returns immediately when the
    /// counter is already zero (R12.4, N == 0 case).
    pub fn wait(&self) {
        let (lock, cvar) = &*self.inner;
        // NOTE (task 3.2): Condvar-paired mutex — see `add`. Deliberately not
        // converted to `lock_or_fault`; the paired `cvar.wait(..)` below also
        // unwraps, so poison recovery here would break the wait invariant.
        let mut count = lock.lock().unwrap();
        while *count > 0 {
            count = cvar.wait(count).unwrap();
        }
    }

    /// Current counter value (primarily for tests/introspection).
    pub fn count(&self) -> i64 {
        // NOTE (task 3.2): Condvar-paired mutex — see `add`. Deliberately not
        // converted to `lock_or_fault`.
        *self.inner.0.lock().unwrap()
    }
}

impl Default for WaitGroup {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Ran-level wait groups (add / done / wait exposed to the Ran language)
// ============================================================================
//
// Like channels and thread handles, wait groups cannot live in the interpreter
// Environment (which is cloned per spawned thread). They live in a single
// process-global registry keyed by an opaque `u64` handle id; the Ran program
// receives that id as an opaque `Int` handle. Every thread holding the id
// refers to the *same* underlying counter, so `done` calls from spawned threads
// correctly release a `wait` on the parent.

/// Outcome of a wait-group operation dispatched from the Ran runtime.
#[derive(Debug, PartialEq, Eq)]
pub enum WgOutcome {
    /// The operation succeeded.
    Ok,
    /// `done` was called more times than `add` (the counter would go negative).
    /// Maps to diagnostic `E0610` (R12.5).
    Negative,
    /// The handle does not refer to a live wait group.
    InvalidHandle,
}

fn waitgroup_registry() -> &'static Mutex<HashMap<u64, WaitGroup>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, WaitGroup>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_waitgroup_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Clone the wait-group handle out of the registry so callers never hold the
/// registry lock across a blocking `wait`.
fn lookup_waitgroup(id: u64) -> Option<WaitGroup> {
    crate::stdlib::lock_or_fault(waitgroup_registry(), "waitgroup registry")
        .get(&id)
        .map(|wg| wg.clone_handle())
}

/// Create a new wait group and return its opaque handle id (R12.4).
pub fn waitgroup_create() -> u64 {
    let id = next_waitgroup_id();
    crate::stdlib::lock_or_fault(waitgroup_registry(), "waitgroup registry")
        .insert(id, WaitGroup::new());
    id
}

/// Add `n` to the wait group's counter. `n` is clamped to the valid range
/// `0..=65535` (R12.4) so out-of-range or negative inputs cannot corrupt the
/// counter.
pub fn wg_add(id: u64, n: i64) -> WgOutcome {
    let wg = match lookup_waitgroup(id) {
        Some(w) => w,
        None => return WgOutcome::InvalidHandle,
    };
    let n = n.clamp(0, 65535);
    wg.add(n);
    WgOutcome::Ok
}

/// Signal completion of one task. Returns `WgOutcome::Negative` (→ `E0610`) when
/// `done` is called more times than `add`, without underflowing the counter
/// (R12.5).
pub fn wg_done(id: u64) -> WgOutcome {
    let wg = match lookup_waitgroup(id) {
        Some(w) => w,
        None => return WgOutcome::InvalidHandle,
    };
    if wg.done() {
        WgOutcome::Ok
    } else {
        WgOutcome::Negative
    }
}

/// Block until the wait group's counter reaches zero, returning immediately when
/// it is already zero (R12.4).
pub fn wg_wait(id: u64) -> WgOutcome {
    let wg = match lookup_waitgroup(id) {
        Some(w) => w,
        None => return WgOutcome::InvalidHandle,
    };
    wg.wait();
    WgOutcome::Ok
}

// ============================================================================
// Ran-level shared state (`shared(v)` + lock-scoped access, R13.1–R13.5)
// ============================================================================
//
// Shared state lets several spawned threads observe and mutate the *same*
// runtime `Value` safely. The backing store is an `Arc<Mutex<Value>>` kept in a
// single process-global registry keyed by an opaque `u64` handle id (the same
// pattern as channels / thread handles / wait groups — see "Model handle
// (Shared)" in the design). Every thread holding the id refers to the one
// underlying mutex, so all access is serialized (R13.1, R13.2).
//
// Design choice — atomic lock-scoped operations instead of a `lock(s) { }`
// block. The design shows `lock(s) { }` as a block form, but the interpreter
// cannot run a Ran block while holding a Rust `MutexGuard` across the module
// dispatch boundary (the guard's lifetime cannot escape the dispatch call). So
// shared access is exposed as small operations that each *acquire → operate →
// release* atomically for the duration of that single operation:
//
//   * `shared_get(s)`      — acquire, clone the current value, release.
//   * `shared_set(s, v)`   — acquire, store `v`, release.
//   * `shared_add(s, n)`   — acquire, add `n` to the numeric value, release,
//                            returning the new value (a read-modify-write that
//                            holds the lock for the whole update, so concurrent
//                            increments never lose updates — R13.1, R13.2).
//
// Each operation satisfies "every read/write goes through synchronization"
// (R13.1), exclusive access while held (R13.2), and automatic release when the
// guard drops at the end of the operation (R13.5).
//
// Acquisition (R13.3/R13.4): std `Mutex` has no timed lock, so acquisition uses
// a `try_lock` loop with a 30-second deadline, sleeping a short interval between
// attempts. If the deadline elapses the operation gives up and reports
// `LockOutcome::TimedOut` (→ diagnostic `E0614`) without crashing the process.

/// Default acquisition deadline for shared-state locks (R13.3, R13.4).
const SHARED_LOCK_DEADLINE: Duration = Duration::from_secs(30);
/// Sleep between `try_lock` attempts while waiting for the lock.
const SHARED_LOCK_RETRY: Duration = Duration::from_millis(2);

/// Outcome of a lock-scoped operation on a shared value.
pub enum LockOutcome {
    /// The operation acquired the lock and produced this value (the current
    /// value for `get`, the new value for `add`, `Void` for `set`).
    Value(Value),
    /// Acquisition exceeded the 30-second deadline; the request was abandoned
    /// without crashing the process. Maps to diagnostic `E0614` (R13.4).
    TimedOut,
    /// The handle does not refer to a live shared value.
    InvalidHandle,
}

fn shared_registry() -> &'static Mutex<HashMap<u64, Arc<Mutex<Value>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<u64, Arc<Mutex<Value>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_shared_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Clone the `Arc` out of the registry so callers never hold the registry lock
/// across a (potentially long) acquisition loop.
fn lookup_shared(id: u64) -> Option<Arc<Mutex<Value>>> {
    crate::stdlib::lock_or_fault(shared_registry(), "shared-value registry").get(&id).cloned()
}

/// Acquire `mutex` via a `try_lock` loop bounded by `deadline`, sleeping `retry`
/// between attempts. Returns the guard on success or `None` once the deadline
/// elapses (R13.3/R13.4). A poisoned mutex (another thread panicked while
/// holding it) is recovered rather than propagated, so one faulting thread
/// cannot permanently wedge the shared value.
///
/// Factored out with injectable `deadline`/`retry` so the timeout path is
/// testable with a short deadline instead of waiting 30 real seconds.
fn acquire_with_deadline(
    mutex: &Mutex<Value>,
    deadline: Duration,
    retry: Duration,
) -> Option<MutexGuard<'_, Value>> {
    let start = Instant::now();
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() >= deadline {
                    return None;
                }
                thread::sleep(retry);
            }
        }
    }
}

/// Acquire the shared value's lock (with `deadline`/`retry`), run `f` while
/// holding it, then release (the guard drops at the end of this call — R13.5).
fn shared_with_lock<F>(id: u64, deadline: Duration, retry: Duration, f: F) -> LockOutcome
where
    F: FnOnce(&mut Value) -> Value,
{
    let mutex = match lookup_shared(id) {
        Some(m) => m,
        None => return LockOutcome::InvalidHandle,
    };
    let outcome = match acquire_with_deadline(&mutex, deadline, retry) {
        Some(mut guard) => LockOutcome::Value(f(&mut guard)),
        None => LockOutcome::TimedOut,
    };
    outcome
}

/// Create a shared value initialized to `initial` and return its opaque handle
/// id (R13.1). The value is wrapped in `Arc<Mutex<Value>>` so it can be observed
/// and mutated from multiple threads through synchronization.
pub fn shared_create(initial: Value) -> u64 {
    let id = next_shared_id();
    crate::stdlib::lock_or_fault(shared_registry(), "shared-value registry")
        .insert(id, Arc::new(Mutex::new(initial)));
    id
}

/// Acquire the lock, return a clone of the current value, release (R13.1/R13.5).
pub fn shared_get(id: u64) -> LockOutcome {
    shared_with_lock(id, SHARED_LOCK_DEADLINE, SHARED_LOCK_RETRY, |v| v.clone())
}

/// Acquire the lock, store `new`, release (R13.1/R13.2/R13.5). Returns `Void` on
/// success.
pub fn shared_set(id: u64, new: Value) -> LockOutcome {
    shared_with_lock(id, SHARED_LOCK_DEADLINE, SHARED_LOCK_RETRY, move |v| {
        *v = new;
        Value::Void
    })
}

/// Acquire the lock, add `delta` to the numeric value, release, and return the
/// new value (R13.1/R13.2/R13.5). Because the read-modify-write happens entirely
/// while the lock is held, concurrent increments from many threads never lose
/// updates. A non-numeric current value is treated as starting from `delta`.
pub fn shared_add(id: u64, delta: i64) -> LockOutcome {
    shared_with_lock(id, SHARED_LOCK_DEADLINE, SHARED_LOCK_RETRY, move |v| {
        let new = match v {
            Value::Int(n) => Value::Int(n.wrapping_add(delta)),
            Value::Float(f) => Value::Float(*f + delta as f64),
            _ => Value::Int(delta),
        };
        *v = new.clone();
        new
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_send_recv() {
        let (tx, rx) = channel::<i32>();
        tx.send(42).unwrap();
        assert_eq!(rx.recv().unwrap(), 42);
    }

    #[test]
    fn test_spawn_and_waitgroup() {
        let wg = WaitGroup::new();
        wg.add(2);

        let wg_clone = wg.clone_handle();
        spawn(move || {
            wg_clone.done();
        });

        let wg_clone2 = wg.clone_handle();
        spawn(move || {
            wg_clone2.done();
        });

        wg.wait();
        assert_eq!(wg.count(), 0);
    }

    // --- Ran-level wait groups ---------------------------------------------

    #[test]
    fn test_waitgroup_add_done_wait_reaches_zero() {
        // add(2) then two done() from spawned threads; wait() unblocks at zero.
        let id = waitgroup_create();
        assert_eq!(wg_add(id, 2), WgOutcome::Ok);

        let t1 = thread::spawn(move || wg_done(id));
        let t2 = thread::spawn(move || wg_done(id));
        assert_eq!(t1.join().unwrap(), WgOutcome::Ok);
        assert_eq!(t2.join().unwrap(), WgOutcome::Ok);

        // Counter is back to zero, so wait returns.
        assert_eq!(wg_wait(id), WgOutcome::Ok);
    }

    #[test]
    fn test_waitgroup_wait_returns_immediately_when_zero() {
        // N == 0: wait must return immediately without blocking (R12.4).
        let id = waitgroup_create();
        assert_eq!(wg_wait(id), WgOutcome::Ok);
    }

    #[test]
    fn test_waitgroup_done_exceeding_add_is_negative() {
        // done called more times than add -> counter would go negative (R12.5 -> E0610),
        // and the counter must NOT underflow.
        let id = waitgroup_create();
        assert_eq!(wg_add(id, 1), WgOutcome::Ok);
        assert_eq!(wg_done(id), WgOutcome::Ok);
        // One extra done with the counter already at zero is detected.
        assert_eq!(wg_done(id), WgOutcome::Negative);
        // A subsequent wait still returns immediately (counter pinned at zero).
        assert_eq!(wg_wait(id), WgOutcome::Ok);
    }

    #[test]
    fn test_waitgroup_done_with_no_add_is_negative() {
        // done before any add is immediately negative without underflow.
        let id = waitgroup_create();
        assert_eq!(wg_done(id), WgOutcome::Negative);
    }

    #[test]
    fn test_waitgroup_add_clamped_to_range() {
        // Out-of-range / negative add is clamped to 0..=65535 (R12.4).
        let id = waitgroup_create();
        assert_eq!(wg_add(id, -5), WgOutcome::Ok); // clamped to 0
        assert_eq!(wg_wait(id), WgOutcome::Ok); // still zero -> returns
    }

    #[test]
    fn test_waitgroup_invalid_handle() {
        // Unknown handles are reported as invalid for every op.
        assert_eq!(wg_add(123_456_789, 1), WgOutcome::InvalidHandle);
        assert_eq!(wg_done(123_456_789), WgOutcome::InvalidHandle);
        assert_eq!(wg_wait(123_456_789), WgOutcome::InvalidHandle);
    }

    // --- Ran-level Value channels ------------------------------------------

    #[test]
    fn test_value_channel_buffered_fifo() {
        // Buffered channel: sends do not block up to capacity; FIFO order (R11.2/R11.5).
        let ch = chan_create(4);
        assert_eq!(chan_send(ch, Value::Int(1)), SendOutcome::Ok);
        assert_eq!(chan_send(ch, Value::Int(2)), SendOutcome::Ok);
        assert_eq!(chan_send(ch, Value::Int(3)), SendOutcome::Ok);

        let mut got = Vec::new();
        for _ in 0..3 {
            match chan_recv(ch) {
                RecvOutcome::Value(Value::Int(n)) => got.push(n),
                other => panic!("expected value, got {:?}", other),
            }
        }
        assert_eq!(got, vec![1, 2, 3]);
    }

    #[test]
    fn test_value_channel_closed_indicator() {
        // After close with an empty buffer, recv reports Closed (R11.4).
        let ch = chan_create(1);
        assert!(chan_close(ch));
        assert!(matches!(chan_recv(ch), RecvOutcome::Closed));
    }

    #[test]
    fn test_value_channel_drains_then_closed() {
        // Buffered values remain receivable after close, then Closed (R11.4).
        let ch = chan_create(2);
        chan_send(ch, Value::Int(10));
        chan_send(ch, Value::Int(20));
        chan_close(ch);
        assert!(matches!(chan_recv(ch), RecvOutcome::Value(Value::Int(10))));
        assert!(matches!(chan_recv(ch), RecvOutcome::Value(Value::Int(20))));
        assert!(matches!(chan_recv(ch), RecvOutcome::Closed));
    }

    #[test]
    fn test_value_channel_send_after_close() {
        // Sending on a closed channel is rejected (R11.7 -> E0611).
        let ch = chan_create(1);
        chan_close(ch);
        assert_eq!(chan_send(ch, Value::Int(1)), SendOutcome::Closed);
    }

    #[test]
    fn test_value_channel_invalid_handle() {
        // Unknown handles: send is invalid, recv reports closed.
        assert_eq!(chan_send(999_999, Value::Int(1)), SendOutcome::InvalidHandle);
        assert!(matches!(chan_recv(999_999), RecvOutcome::Closed));
    }

    #[test]
    fn test_value_channel_rendezvous_cross_thread() {
        // Capacity 0 rendezvous: a spawned receiver unblocks the sender.
        let ch = chan_create(0);
        let handle = thread::spawn(move || match chan_recv(ch) {
            RecvOutcome::Value(Value::Str(s)) => s,
            other => panic!("expected value, got {:?}", other),
        });
        assert_eq!(chan_send(ch, Value::Str("hi".to_string())), SendOutcome::Ok);
        assert_eq!(handle.join().unwrap(), "hi");
    }

    // --- Ran-level thread handles ------------------------------------------

    #[test]
    fn test_thread_handle_join_returns_value() {
        // spawn returns a unique handle; join blocks and returns the result (R12.1/R12.2).
        let id = register_thread(thread::spawn(|| Value::Int(42)));
        match join_thread(id) {
            JoinOutcome::Value(Value::Int(n)) => assert_eq!(n, 42),
            _ => panic!("expected joined value 42"),
        }
    }

    #[test]
    fn test_thread_handles_are_unique() {
        let a = register_thread(thread::spawn(|| Value::Int(1)));
        let b = register_thread(thread::spawn(|| Value::Int(2)));
        assert_ne!(a, b, "each spawn must yield a unique handle id");
        let _ = join_thread(a);
        let _ = join_thread(b);
    }

    #[test]
    fn test_double_join_is_invalid() {
        // Re-joining an already-joined handle is rejected (R12.3 -> E0612).
        let id = register_thread(thread::spawn(|| Value::Int(7)));
        assert!(matches!(join_thread(id), JoinOutcome::Value(Value::Int(7))));
        assert!(matches!(join_thread(id), JoinOutcome::Invalid));
    }

    #[test]
    fn test_join_invalid_handle() {
        // An unknown handle is rejected without blocking (R12.3 -> E0612).
        assert!(matches!(join_thread(987_654_321), JoinOutcome::Invalid));
    }

    #[test]
    fn test_thread_error_value_delivered_to_joiner() {
        // A spawned thread whose body ends in a caught fault returns an error
        // map Value (the runtime renders RuntimeFault -> { error, code, message }).
        // join must deliver that exact value to the joiner unchanged (R12.6),
        // not as a Panicked outcome — the fault was handled, not a real panic.
        let id = register_thread(thread::spawn(|| {
            let mut m = HashMap::new();
            m.insert("error".to_string(), Value::Bool(true));
            m.insert("code".to_string(), Value::Str("E0614".to_string()));
            m.insert("message".to_string(), Value::Str("lock acquisition timed out".to_string()));
            Value::Map(m)
        }));
        match join_thread(id) {
            JoinOutcome::Value(Value::Map(m)) => {
                assert!(matches!(m.get("error"), Some(Value::Bool(true))));
                match m.get("code") {
                    Some(Value::Str(c)) => assert_eq!(c, "E0614"),
                    other => panic!("expected code E0614, got {:?}", other),
                }
                match m.get("message") {
                    Some(Value::Str(s)) => assert_eq!(s, "lock acquisition timed out"),
                    other => panic!("expected delivered message, got {:?}", other),
                }
            }
            other => panic!("expected the error map delivered to the joiner, got {:?}", debug_join(&other)),
        }
    }

    /// Small helper so the panic message above can name the unexpected outcome
    /// without `JoinOutcome` needing a `Debug` impl.
    fn debug_join(o: &JoinOutcome) -> &'static str {
        match o {
            JoinOutcome::Value(_) => "Value(non-map)",
            JoinOutcome::Invalid => "Invalid",
            JoinOutcome::Panicked => "Panicked",
        }
    }

    #[test]
    fn test_spawn_value_normal_body_returns_value_unchanged() {
        // A non-faulting body's value is delivered to the joiner unchanged (R3.6).
        let id = spawn_value(|| Value::Int(99));
        match join_thread(id) {
            JoinOutcome::Value(Value::Int(n)) => assert_eq!(n, 99),
            other => panic!("expected unchanged value 99, got {}", debug_join(&other)),
        }
    }

    #[test]
    fn test_spawn_value_fault_in_body_becomes_inspectable_error() {
        // A RuntimeFault raised inside the spawned body must be CAUGHT and turned
        // into an inspectable error map ({ error, code, message }) delivered to
        // the joiner — the process must NOT crash and join must NOT see Panicked
        // (R3.6 / R12.6).
        let id = spawn_value(|| {
            crate::runtime::runtime_error(
                "E1004",
                "division by zero in spawned thread",
                "Periksa pembagi sebelum membagi.",
            )
        });
        match join_thread(id) {
            JoinOutcome::Value(Value::Map(m)) => {
                assert!(matches!(m.get("error"), Some(Value::Bool(true))));
                match m.get("code") {
                    Some(Value::Str(c)) => assert_eq!(c, "E1004"),
                    other => panic!("expected code E1004, got {:?}", other),
                }
                match m.get("message") {
                    Some(Value::Str(s)) => {
                        assert_eq!(s, "division by zero in spawned thread")
                    }
                    other => panic!("expected the fault message, got {:?}", other),
                }
            }
            other => panic!(
                "faulting thread must surface an inspectable error value, got {}",
                debug_join(&other)
            ),
        }
    }

    // --- Ran-level shared state --------------------------------------------

    #[test]
    fn test_shared_create_get_returns_initial() {
        // A freshly created shared value reads back its initial value (R13.1).
        let id = shared_create(Value::Int(7));
        match shared_get(id) {
            LockOutcome::Value(Value::Int(n)) => assert_eq!(n, 7),
            _ => panic!("expected initial value 7"),
        }
    }

    #[test]
    fn test_shared_set_then_get() {
        // set then get observes the stored value through synchronization (R13.1/R13.2).
        let id = shared_create(Value::Int(1));
        assert!(matches!(shared_set(id, Value::Str("hi".into())), LockOutcome::Value(Value::Void)));
        match shared_get(id) {
            LockOutcome::Value(Value::Str(s)) => assert_eq!(s, "hi"),
            _ => panic!("expected stored value \"hi\""),
        }
    }

    #[test]
    fn test_shared_invalid_handle() {
        // Unknown handles are reported as invalid for every op.
        assert!(matches!(shared_get(987_654_321), LockOutcome::InvalidHandle));
        assert!(matches!(shared_set(987_654_321, Value::Int(0)), LockOutcome::InvalidHandle));
        assert!(matches!(shared_add(987_654_321, 1), LockOutcome::InvalidHandle));
    }

    #[test]
    fn test_shared_concurrent_increment_no_lost_updates() {
        // Many threads each increment the same shared counter; because each
        // read-modify-write holds the lock for its whole duration, the final
        // total is exact with no lost updates (R13.1, R13.2).
        let id = shared_create(Value::Int(0));
        const THREADS: i64 = 8;
        const PER_THREAD: i64 = 1000;
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            handles.push(thread::spawn(move || {
                for _ in 0..PER_THREAD {
                    assert!(matches!(shared_add(id, 1), LockOutcome::Value(_)));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        match shared_get(id) {
            LockOutcome::Value(Value::Int(n)) => assert_eq!(n, THREADS * PER_THREAD),
            _ => panic!("expected exact total"),
        }
    }

    #[test]
    fn test_shared_held_lock_blocks_second_acquire() {
        // While one holder owns the guard, a second try_lock cannot acquire it,
        // so an acquisition with a short deadline times out (R13.2 -> R13.4).
        let id = shared_create(Value::Int(0));
        let mutex = lookup_shared(id).expect("shared value should exist");
        let _held = mutex.lock().unwrap(); // hold the lock for the test duration

        // A second acquisition with a tiny injected deadline gives up rather
        // than blocking forever, returning TimedOut (-> E0614).
        let outcome = shared_with_lock(
            id,
            Duration::from_millis(30),
            Duration::from_millis(1),
            |v| v.clone(),
        );
        assert!(matches!(outcome, LockOutcome::TimedOut));
    }

    #[test]
    fn test_shared_acquire_deadline_returns_none_when_contended() {
        // The factored acquire helper itself returns None once the deadline
        // elapses on a contended mutex (testable timeout logic, R13.4).
        let mutex = Mutex::new(Value::Int(0));
        let _guard = mutex.lock().unwrap();
        let start = Instant::now();
        let got = acquire_with_deadline(&mutex, Duration::from_millis(25), Duration::from_millis(1));
        assert!(got.is_none(), "should time out while the lock is held");
        assert!(start.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn test_shared_acquire_succeeds_when_free() {
        // When the lock is free, acquisition succeeds well within the deadline.
        let mutex = Mutex::new(Value::Int(5));
        let got = acquire_with_deadline(&mutex, Duration::from_secs(30), Duration::from_millis(1));
        assert!(got.is_some());
        assert!(matches!(*got.unwrap(), Value::Int(5)));
    }
}

// ============================================================================
// Property test P6 — FIFO channel tanpa kehilangan/duplikasi.
// ============================================================================
//
// Property-based test memetakan Correctness Property 6 dari `design.md`:
// untuk satu pasangan pengirim/penerima, vektor yang diterima sama persis
// dengan vektor yang dikirim — terurut (FIFO), tanpa kehilangan, tanpa
// duplikasi — apapun kapasitas buffer (termasuk rendezvous kapasitas 0).
//
// Harness PBT std-only ada di `crate::support::pbt` (RNG seedable, ≥100 kasus
// via `RAN_PBT_CASES`, seed dicetak saat gagal untuk reproduksi).

#[cfg(test)]
mod fifo_channel_property {
    // Feature: enterprise-runtime-capabilities, Property 6: FIFO channel tanpa kehilangan/duplikasi
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// Satu kasus uji: urutan `Value` yang akan dikirim + kapasitas buffer acak.
    #[derive(Clone, Debug)]
    struct Case {
        sent: Vec<Value>,
        capacity: usize,
    }

    /// Hasilkan satu `Value` skalar yang mudah dibandingkan: `Int` atau `Str`.
    fn gen_value(rng: &mut Rng) -> Value {
        if rng.boolean() {
            Value::Int(rng.range_i64(-1_000_000, 1_000_000))
        } else {
            let len = rng.upto(6);
            let mut s = String::new();
            for _ in 0..len {
                s.push((b'a' + rng.below(26) as u8) as char);
            }
            Value::Str(s)
        }
    }

    /// Kesetaraan struktural untuk `Value` yang dipakai generator (`Int`/`Str`).
    /// `Value` tidak meng-`derive` `PartialEq`, jadi bandingkan eksplisit di sini.
    fn value_eq(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Str(x), Value::Str(y)) => x == y,
            _ => false,
        }
    }

    /// Generator: urutan `Value` dengan panjang acak + kapasitas acak `0..=len`
    /// (0 = rendezvous, >0 = buffered). Shrink: kosongkan, buang elemen, lalu
    /// perkecil kapasitas — selalu menjaga `capacity <= sent.len()`.
    fn case_gen() -> Gen<Case> {
        Gen::new(
            |rng: &mut Rng, size: usize| {
                let cap_hint = size.max(1).min(24) + 1;
                let len = rng.upto(cap_hint);
                let sent = (0..len).map(|_| gen_value(rng)).collect::<Vec<_>>();
                let capacity = rng.upto(len); // 0..=len inklusif
                Case { sent, capacity }
            },
            |case: &Case| {
                let mut out: Vec<Case> = Vec::new();
                let clamp_cap = |cap: usize, len: usize| cap.min(len);
                // Kandidat kosong.
                if !case.sent.is_empty() {
                    out.push(Case { sent: Vec::new(), capacity: 0 });
                }
                // Buang separuh awal / akhir.
                let half = case.sent.len() / 2;
                if half > 0 {
                    let first = case.sent[..half].to_vec();
                    let cap = clamp_cap(case.capacity, first.len());
                    out.push(Case { sent: first, capacity: cap });
                    let second = case.sent[half..].to_vec();
                    let cap = clamp_cap(case.capacity, second.len());
                    out.push(Case { sent: second, capacity: cap });
                }
                // Buang satu elemen pada tiap posisi.
                for i in 0..case.sent.len() {
                    let mut s = case.sent.clone();
                    s.remove(i);
                    let cap = clamp_cap(case.capacity, s.len());
                    out.push(Case { sent: s, capacity: cap });
                }
                // Perkecil kapasitas menuju rendezvous (0).
                if case.capacity > 0 {
                    out.push(Case { sent: case.sent.clone(), capacity: 0 });
                    let halfway = case.capacity / 2;
                    if halfway != 0 && halfway != case.capacity {
                        out.push(Case { sent: case.sent.clone(), capacity: halfway });
                    }
                }
                out
            },
        )
    }

    /// **Validates: Requirements 12.2, 12.5**
    ///
    /// Untuk setiap urutan `Value` dan kapasitas buffer acak (termasuk 0):
    /// jalankan produsen di thread terpisah yang mengirim semua nilai lalu
    /// menutup channel; penerima (thread utama) menerima sampai `Closed`.
    /// Vektor yang diterima HARUS sama persis dengan yang dikirim — urutan
    /// terjaga, tanpa kehilangan, tanpa duplikasi (FIFO). Produsen dijalankan
    /// di thread terpisah agar `send`/`recv` saling berinterleave sehingga
    /// channel berkapasitas/rendezvous tidak deadlock.
    #[test]
    fn prop_fifo_channel_no_loss_no_dup() {
        pbt::for_all("P6 FIFO channel tanpa kehilangan/duplikasi", &case_gen(), |case: &Case| {
            let ch = chan_create(case.capacity);

            // Produsen: kirim semua nilai berurutan lalu tutup channel.
            let to_send = case.sent.clone();
            let producer = thread::spawn(move || {
                for v in to_send {
                    if chan_send(ch, v) != SendOutcome::Ok {
                        return false; // pengiriman tak terduga gagal
                    }
                }
                chan_close(ch);
                true
            });

            // Penerima: kumpulkan nilai sampai indikator Closed.
            let mut received: Vec<Value> = Vec::new();
            loop {
                match chan_recv(ch) {
                    RecvOutcome::Value(v) => received.push(v),
                    RecvOutcome::Closed => break,
                }
            }

            let send_ok = producer.join().unwrap_or(false);

            // Oracle: setiap pengiriman sukses; jumlah sama (tanpa loss/dup);
            // dan setiap elemen cocok pada posisinya (urutan FIFO terjaga).
            send_ok
                && received.len() == case.sent.len()
                && received
                    .iter()
                    .zip(case.sent.iter())
                    .all(|(got, want)| value_eq(got, want))
        });
    }
}
