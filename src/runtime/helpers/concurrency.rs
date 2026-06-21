//! Concurrency handle/error value constructors (extracted from mod.rs).
//! Part of the `Environment` inherent impl (microkernel-style split).

use super::super::*;
use std::collections::HashMap;

impl Environment {
    pub(crate) fn channel_closed_indicator() -> Value {
        let mut m = HashMap::new();
        m.insert("__closed__".to_string(), Value::Bool(true));
        Value::Map(m)
    }

    /// Whether `v` is the channel closed indicator (used by `concurrency.is_closed`).
    pub(crate) fn is_channel_closed_indicator(v: &Value) -> bool {
        matches!(v, Value::Map(m) if matches!(m.get("__closed__"), Some(Value::Bool(true))))
    }

    /// Emit the handleable `E0611` diagnostic for a send on a closed channel /
    /// dropped receiver and return it as a catchable Ran error value
    /// (R11.7). The runtime keeps running.
    pub(crate) fn concurrency_send_closed(id: u64) -> Value {
        let msg = format!(
            "cannot send on channel {}: channel is closed or all receivers were dropped",
            id
        );
        let hint = crate::support::diagnostics::code_severity_hint("E0611")
            .map(|(_, h)| h)
            .unwrap_or("Channel tertutup. Hentikan pengiriman atau buat channel baru.");
        eprintln!("\x1b[31;1merror\x1b[0m[E0611]: {}", msg);
        eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0611".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Render a caught `RuntimeFault` from a spawned thread body as a catchable
    /// Ran error value, so the joiner receives the fault as a value instead of
    /// the process crashing (R12.6). No diagnostic is printed here — the value
    /// carries the fault's original code/message for the joiner to inspect.
    pub(crate) fn thread_fault_value(fault: &RuntimeFault) -> Value {
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str(fault.code.clone()));
        m.insert("message".to_string(), Value::Str(fault.message.clone()));
        Value::Map(m)
    }

    /// Emit the handleable `E0612` diagnostic for a `join` on a handle that was
    /// already joined or is invalid, and return it as a catchable Ran error
    /// value (R12.3). The join is rejected without blocking; the runtime keeps
    /// running.
    pub(crate) fn concurrency_join_invalid(id: u64) -> Value {
        let msg = format!(
            "cannot join thread handle {}: it was already joined or is invalid",
            id
        );
        let hint = crate::support::diagnostics::code_severity_hint("E0612")
            .map(|(_, h)| h)
            .unwrap_or("Handle thread sudah di-join atau tidak valid. Simpan hasil join pertama.");
        eprintln!("\x1b[31;1merror\x1b[0m[E0612]: {}", msg);
        eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0612".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Render a genuine (non-fault) thread panic as a catchable error value so a
    /// joiner is not itself brought down. Faults are handled separately via
    /// `thread_fault_value`; this only covers unexpected panics.
    pub(crate) fn concurrency_thread_panicked(id: u64) -> Value {
        let msg = format!("joined thread {} ended abnormally (panicked)", id);
        eprintln!("\x1b[31;1merror\x1b[0m[E0612]: {}", msg);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0612".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Emit the handleable `E0610` diagnostic for a wait group whose counter
    /// went negative (`done` called more times than `add`) and return it as a
    /// catchable Ran error value (R12.5). The counter is not underflowed and the
    /// runtime keeps running.
    pub(crate) fn concurrency_waitgroup_negative(id: u64) -> Value {
        let msg = format!(
            "wait group {} counter went negative: `done` was called more times than `add`",
            id
        );
        let hint = crate::support::diagnostics::code_severity_hint("E0610")
            .map(|(_, h)| h)
            .unwrap_or("`done` melebihi `add`. Pastikan jumlah `done` tidak lebih dari thread yang ditambahkan.");
        eprintln!("\x1b[31;1merror\x1b[0m[E0610]: {}", msg);
        eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0610".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Emit a handleable `E0610` diagnostic for an operation on an unknown wait
    /// group handle and return it as a catchable Ran error value.
    pub(crate) fn concurrency_waitgroup_invalid(id: u64) -> Value {
        let msg = format!("wait group handle {} is invalid", id);
        let hint = crate::support::diagnostics::code_severity_hint("E0610")
            .map(|(_, h)| h)
            .unwrap_or("Handle wait group tidak valid. Buat wait group dengan `waitgroup()`.");
        eprintln!("\x1b[31;1merror\x1b[0m[E0610]: {}", msg);
        eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0610".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Emit the handleable `E0614` diagnostic for a shared-state lock acquisition
    /// that exceeded the 30-second deadline and return it as a catchable Ran
    /// error value (R13.4). The request is abandoned; the runtime keeps running.
    pub(crate) fn concurrency_lock_timeout(id: u64) -> Value {
        let msg = format!(
            "acquiring the lock on shared state {} exceeded the 30s deadline",
            id
        );
        let hint = crate::support::diagnostics::code_severity_hint("E0614")
            .map(|(_, h)| h)
            .unwrap_or("Akuisisi lock melebihi 30 detik. Periksa potensi deadlock atau perpendek seksi kritis.");
        eprintln!("\x1b[31;1merror\x1b[0m[E0614]: {}", msg);
        eprintln!("  \x1b[36m= help\x1b[0m: {}", hint);
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0614".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }

    /// Emit a handleable `E0614` diagnostic for an operation on an unknown shared
    /// handle and return it as a catchable Ran error value.
    pub(crate) fn concurrency_shared_invalid(id: u64) -> Value {
        let msg = format!("shared state handle {} is invalid", id);
        eprintln!("\x1b[31;1merror\x1b[0m[E0614]: {}", msg);
        eprintln!("  \x1b[36m= help\x1b[0m: Handle shared tidak valid. Buat shared state dengan `shared(v)`.");
        let mut m = HashMap::new();
        m.insert("error".to_string(), Value::Bool(true));
        m.insert("code".to_string(), Value::Str("E0614".to_string()));
        m.insert("message".to_string(), Value::Str(msg));
        Value::Map(m)
    }
}
