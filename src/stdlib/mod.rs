pub mod net;
pub mod hardware;
pub mod concurrency;
pub mod db;

/// Lock a `std::sync::Mutex<T>`, recovering gracefully from a poisoned lock.
///
/// A mutex becomes *poisoned* when a thread panics while holding its guard.
/// The standard `.lock().unwrap()` / `.expect(..)` idiom turns that poison into
/// a fresh panic, which — in a long-running server — cascades one worker's
/// failure into a process-wide crash. Memory safety and crash hardening are
/// first-class goals here, so instead of panicking we raise a *recoverable*
/// runtime fault (`E0511`, a `RuntimeFault` that unwinds to the nearest catch
/// boundary). The top-level runner reports it cleanly and the HTTP server turns
/// it into a 500 without killing the process (R4.4, R4.6).
///
/// `what` names the resource being locked (e.g. `"db connection pool"`) and is
/// woven into the diagnostic message so the fault points at the right place.
///
/// Used by `stdlib/db.rs` and `stdlib/concurrency.rs` (wired in task 3.2).
#[allow(dead_code)]
pub fn lock_or_fault<'a, T>(
    m: &'a std::sync::Mutex<T>,
    what: &str,
) -> std::sync::MutexGuard<'a, T> {
    match m.lock() {
        Ok(guard) => guard,
        Err(_poisoned) => crate::runtime::runtime_error(
            "E0511",
            &format!(
                "internal lock for `{}` was poisoned by an earlier failure",
                what
            ),
            "Operasi sebelumnya gagal di tengah; state dipulihkan — coba ulangi transaksi.",
        ),
    }
}

// ============================================================================
// Property 4 — Poisoned mutex recovers as a fault (R4.4).
// ============================================================================
#[cfg(test)]
mod poisoned_mutex_property {
    // Feature: memory-safe-self-hosting, Property 4: Poisoned mutex recovers as a fault
    use super::lock_or_fault;
    use crate::runtime::catch_fault;
    use crate::support::pbt::{self, Gen, Rng};
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Poison `m` by having a freshly spawned thread panic *while holding the
    /// guard*, then join it. The poisoning panic is intentional, so we silence
    /// the panic hook around it to keep test output clean, and we swallow the
    /// thread's `Err` join result (that error is exactly the panic we caused).
    ///
    /// `payload_kind` selects the panic payload type so the property exercises
    /// poisoning from arbitrary failure shapes (string slice, owned string,
    /// integer, and a custom value via `panic_any`).
    fn poison_mutex(m: &Arc<Mutex<i64>>, payload_kind: u8) {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let m2 = Arc::clone(m);
        let handle = thread::spawn(move || {
            let _guard = m2.lock().expect("fresh mutex must lock");
            match payload_kind % 4 {
                0 => panic!("boom: &str payload"),
                1 => panic!("{}", String::from("boom: owned String payload")),
                2 => std::panic::panic_any(42i32),
                _ => std::panic::panic_any((7u8, "tuple payload")),
            }
        });
        // The join returns Err carrying the panic payload — expected; discard it.
        let _ = handle.join();
        panic::set_hook(prev);
    }

    /// One generated scenario: an arbitrary resource label `what` and a payload
    /// kind used to poison the lock.
    #[derive(Clone, Debug)]
    struct Case {
        what: String,
        payload_kind: u8,
    }

    fn case_gen() -> Gen<Case> {
        let label_gen = pbt::string(24);
        Gen::new(
            move |rng: &mut Rng, size| Case {
                what: label_gen.generate(rng, size),
                payload_kind: (rng.below(4)) as u8,
            },
            |_| Vec::new(),
        )
    }

    /// Run one case: poison a mutex, then call `lock_or_fault` inside
    /// `catch_fault` and assert it surfaces a *catchable* `E0511` fault (rather
    /// than a process-killing panic) with a non-empty message. Reaching the
    /// assertion at all proves the process stayed alive.
    fn check_case(case: &Case) -> bool {
        let m = Arc::new(Mutex::new(0i64));
        poison_mutex(&m, case.payload_kind);

        let result = catch_fault(AssertUnwindSafe(|| {
            // Hold the guard briefly to ensure the call path completes.
            let g = lock_or_fault(&m, &case.what);
            *g
        }));

        match result {
            Err(fault) => fault.code == "E0511" && !fault.message.is_empty(),
            Ok(_) => false, // a poisoned lock must NOT silently succeed
        }
    }

    /// Property 4: for any resource label and any poisoning failure shape,
    /// locking a poisoned mutex through `lock_or_fault` yields a recoverable
    /// `E0511` `RuntimeFault` (caught by `catch_fault`) and the process stays
    /// alive — never an uncatchable panic/crash.
    ///
    /// Validates: Requirement 4.4
    #[test]
    fn prop_poisoned_mutex_recovers_as_fault() {
        pbt::for_all(
            "P4 poisoned mutex recovers as E0511 fault",
            &case_gen(),
            check_case,
        );
    }

    /// Focused deterministic companion: a single poisoned mutex maps to a
    /// catchable `E0511` with a message that names the resource.
    #[test]
    fn poisoned_mutex_maps_to_e0511() {
        let m = Arc::new(Mutex::new(123i64));
        poison_mutex(&m, 0);

        let result = catch_fault(AssertUnwindSafe(|| {
            let g = lock_or_fault(&m, "db connection pool");
            *g
        }));

        match result {
            Err(fault) => {
                assert_eq!(fault.code, "E0511", "poison must map to E0511");
                assert!(
                    fault.message.contains("db connection pool"),
                    "message should name the resource, got: {}",
                    fault.message
                );
                assert!(!fault.help.is_empty(), "diagnostic must carry a help hint");
            }
            Ok(_) => panic!("poisoned lock unexpectedly succeeded instead of faulting"),
        }
    }
}
