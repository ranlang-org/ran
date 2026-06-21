//! Fine-grained `Environment` helper modules (microkernel-style: one concern
//! per file, not a monolith). Each submodule adds methods to the `Environment`
//! inherent impl and is reachable across the `runtime` tree via `pub(crate)`.

mod concurrency;
mod db;
