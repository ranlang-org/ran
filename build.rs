//! Build script: link the system C libraries Ran uses via FFI.
//!
//! Ran stays free of third-party *crates* (see docs/21-dependency-policy.md),
//! but links a few system libraries directly via FFI:
//!
//!   * OpenSSL (`libssl`/`libcrypto`) — TLS. Core capability, linked always.
//!   * SQLite3 (`libsqlite3`)        — native `db` module (Kelompok B).
//!
//! OpenSSL ships on essentially every server distro, so it is linked
//! unconditionally (matching the existing behavior). `libsqlite3`, however, may
//! be absent on a given build host. Emitting an unconditional
//! `cargo:rustc-link-lib` for a missing library makes the *whole* `cargo build`
//! fail at link time — which would block the rest of the project.
//!
//! To keep a stock `cargo build` working everywhere, the optional library is
//! only linked when one of the following holds:
//!   * it is explicitly forced on via `RAN_ENABLE_SQLITE`, or
//!   * it is auto-detected on the host's library search path.
//! Forcing a library *off* (e.g. `RAN_ENABLE_SQLITE=0`) always wins.
//!
//! Non-standard install locations are supported through `SQLITE_LIB_DIR`
//! (alongside the existing `OPENSSL_LIB_DIR`).
//!
//! Static linking: `ran build --link-static` cannot forward a flag into this
//! build script (Cargo does not pass custom CLI flags to build scripts), so the
//! mode is surfaced through the `RAN_LINK_STATIC` env var. When set, the
//! optional libraries are linked as static archives (`lib<x>.a`) instead of
//! shared objects, for fully self-contained distribution.

use std::env;
use std::path::PathBuf;

fn main() {
    // --- OpenSSL: always linked (TLS is a core capability) ------------------
    if let Ok(dir) = env::var("OPENSSL_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", dir);
    }
    println!("cargo:rustc-link-lib=ssl");
    println!("cargo:rustc-link-lib=crypto");

    // --- Static-link mode plumbing (`--link-static` -> RAN_LINK_STATIC) -----
    let link_static = env_flag("RAN_LINK_STATIC");

    // --- Optional system libraries (guarded so a stock build still works) ---
    // Kelompok B — SQLite native (`db` module).
    maybe_link("sqlite3", "SQLITE_LIB_DIR", "RAN_ENABLE_SQLITE", link_static);

    // --- Re-run triggers ----------------------------------------------------
    println!("cargo:rerun-if-changed=build.rs");
    for var in [
        "OPENSSL_LIB_DIR",
        "SQLITE_LIB_DIR",
        "RAN_ENABLE_SQLITE",
        "RAN_LINK_STATIC",
    ] {
        println!("cargo:rerun-if-env-changed={}", var);
    }
}

/// Decide whether (and how) to link an optional system library.
///
/// * `lib`        — base name, e.g. `"sqlite3"` (linked as `-lsqlite3`).
/// * `dir_var`    — env var with a non-standard library directory override.
/// * `enable_var` — env var to force the link on/off (tri-state, see below).
/// * `link_static`— when true, link the static archive (`lib<lib>.a`).
fn maybe_link(lib: &str, dir_var: &str, enable_var: &str, link_static: bool) {
    // Always honor an explicit override directory for the search path.
    let override_dir = env::var(dir_var).ok();
    if let Some(dir) = override_dir.as_deref() {
        println!("cargo:rustc-link-search=native={}", dir);
    }

    let should_link = match env_tristate(enable_var) {
        Some(true) => true,                  // forced on: link even if not found
        Some(false) => return,               // forced off: never link
        None => find_lib(lib, override_dir.as_deref(), link_static).is_some(), // auto
    };

    if !should_link {
        // Not found and not forced on. Skip linking so `cargo build` keeps
        // succeeding on hosts without this library. The runtime performs its
        // own availability check and emits a proper diagnostic on first use.
        println!(
            "cargo:warning=Ran: system library 'lib{lib}' not found on the link \
             search path; the corresponding module will be unavailable at \
             runtime. Install it, set {dir_var} to its directory, or set \
             {enable_var}=1 to force linking."
        );
        return;
    }

    if link_static {
        println!("cargo:rustc-link-lib=static={}", lib);
    } else {
        println!("cargo:rustc-link-lib={}", lib);
    }
}

/// Probe the common library search paths for a library file.
/// Returns the first matching path, or `None` if not found.
fn find_lib(lib: &str, override_dir: Option<&str>, link_static: bool) -> Option<PathBuf> {
    for dir in candidate_dirs(override_dir) {
        if !dir.is_dir() {
            continue;
        }
        if link_static {
            let archive = dir.join(format!("lib{}.a", lib));
            if archive.exists() {
                return Some(archive);
            }
            continue;
        }

        // Dynamic: exact `.so`/`.dylib` first.
        for name in [format!("lib{}.so", lib), format!("lib{}.dylib", lib)] {
            let candidate = dir.join(&name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        // Then versioned shared objects, e.g. `libsqlite3.so.0`.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let prefix = format!("lib{}.so.", lib);
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&prefix) {
                        return Some(entry.path());
                    }
                }
            }
        }
    }
    None
}

/// Build the ordered list of directories to probe for a library.
fn candidate_dirs(override_dir: Option<&str>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = override_dir {
        dirs.push(PathBuf::from(d));
    }
    // Respect the loader/compiler search-path env vars when present.
    for key in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "LIBRARY_PATH"] {
        if let Ok(val) = env::var(key) {
            for p in val.split(':').filter(|s| !s.is_empty()) {
                dirs.push(PathBuf::from(p));
            }
        }
    }
    // Common system locations across Linux distros and macOS (Homebrew).
    for d in [
        "/usr/lib",
        "/usr/local/lib",
        "/lib",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/lib64",
        "/opt/homebrew/lib",
        "/usr/local/opt",
    ] {
        dirs.push(PathBuf::from(d));
    }
    dirs
}

/// A simple boolean env flag: true for `1`/`true`/`on`/`yes` (case-insensitive).
fn env_flag(var: &str) -> bool {
    matches!(env_tristate(var), Some(true))
}

/// Tri-state env var: `Some(true)` to force on, `Some(false)` to force off,
/// `None` when unset (or set to an empty string) meaning "auto-detect".
fn env_tristate(var: &str) -> Option<bool> {
    let raw = env::var(var).ok()?;
    let val = raw.trim().to_ascii_lowercase();
    match val.as_str() {
        "" => None, // set-but-empty: treat as unset (auto)
        "0" | "false" | "off" | "no" => Some(false),
        "1" | "true" | "on" | "yes" => Some(true),
        // Any other non-empty value is treated as an opt-in.
        _ => Some(true),
    }
}
