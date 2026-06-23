//! Native ahead-of-time (AOT) codegen — Phase D, iterations D1–D2.
//!
//! This is a REAL native backend: it lowers a checked Ran program (the D2
//! subset) to C, writes a precompiled minimal runtime (`ran_rt.c`/`ran_rt.h`)
//! next to it, invokes the system C compiler, and links a genuine native ELF
//! binary. The artifact carries NO embedded interpreter and NO `.ran` source —
//! unlike the legacy `codegen::compile_standalone` embed-source path, which
//! remains the default `ran build` until the native subset matures.
//!
//! D2 extends the D1 scalar core (functions/recursion, control flow, checked
//! int arithmetic, bool, strings + `echo`) to the data-type layer: a tagged,
//! reference-counted `RanValue` model (see `runtime/ran_rt.h`) backing exact
//! `decimal` money math, arrays (bounds-checked indexing + `len`), structs
//! (literal construction + field access), `float`, and `match`.
//!
//! Honest by construction: any construct outside the D2 subset is a hard build
//! error (`E0606`) raised by [`lower::supported`], never a silent fallback.
//!
//! Diagnostics:
//!   * `E0601` — no C compiler found (`cc` / `$CC`)
//!   * `E0602` — emit-C (lowering) failed
//!   * `E0603` — C compile step failed (includes `cc` stderr)
//!   * `E0604` — link step failed (includes `cc` stderr)
//!   * `E0606` — construct not yet supported by native codegen
//!
//! Dependency policy: the only external process is the system C compiler, a
//! documented build-time exception (see docs/21-dependency-policy.md). No cargo
//! crate is added; the toolchain stays std-only.

pub mod lower;

use crate::semantics::analyzer::CheckedProgram;
use crate::support::diagnostics::Diagnostic;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The precompiled minimal C runtime, embedded into the `ran` binary at build
/// time so native compilation needs no source tree on the host.
const RAN_RT_H: &str = include_str!("runtime/ran_rt.h");
const RAN_RT_C: &str = include_str!("runtime/ran_rt.c");

/// Options for a native build.
#[derive(Debug, Clone)]
pub struct AotOptions {
    /// Source file path, used for diagnostic locations.
    pub file: String,
    /// Statically link required libraries (D1: accepted but a no-op since the
    /// subset links nothing beyond the C runtime; full support is task 13.3).
    pub link_static: bool,
}

impl AotOptions {
    pub fn new(file: impl Into<String>) -> Self {
        AotOptions { file: file.into(), link_static: false }
    }
}

/// Locate the system C compiler: honor `$CC`, else default to `cc`.
fn detect_cc() -> String {
    std::env::var("CC").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "cc".to_string())
}

/// Confirm the chosen compiler can actually be spawned. Returns `E0601` if not.
fn ensure_cc_available(cc: &str, file: &str) -> Result<(), Diagnostic> {
    match Command::new(cc).arg("--version").output() {
        Ok(_) => Ok(()),
        Err(_) => Err(Diagnostic::from_code(
            "E0601",
            format!("no C compiler found (tried `{}`)", cc),
        )
        .with_help(format!(
            "install a C compiler (e.g. gcc or clang) or set $CC to its path; \
             source: {}",
            file
        ))),
    }
}

/// Compile a checked program to a native binary at `out`.
///
/// Pipeline: pre-flight subset check -> lower to C -> write `build/<name>.c`
/// plus the runtime -> compile each to an object file -> link to a temp path ->
/// atomically rename onto `out` (deleting the temp on any failure, so no
/// partially-executable artifact is ever left behind, R10.6).
pub fn compile_native(
    checked: &CheckedProgram,
    out: &str,
    opts: &AotOptions,
) -> Result<(), Diagnostic> {
    let file = opts.file.as_str();

    // 1. Pre-flight: hard-reject anything outside the D1 subset (E0606).
    lower::supported(checked, file)?;

    // Does the program use the `db` (SQLite) module? If so, the C runtime's db
    // section is compiled in (`-DRAN_ENABLE_SQLITE`) and the system `libsqlite3`
    // is linked (`-lsqlite3`). Programs that never import `db` link nothing extra
    // and the db section is not even compiled — keeping the artifact lean and
    // free of an unnecessary shared-library dependency.
    let uses_db = lower::program_uses_module(checked, "db");
    // Does the program use the `http` client? If so the runtime's http section
    // is compiled (`-DRAN_ENABLE_HTTP`) and the system OpenSSL (`-lssl -lcrypto`)
    // is linked for the `https://` transport. Plain `http://` needs only libc
    // sockets, but OpenSSL is linked whenever the module is imported so any URL
    // scheme works at runtime.
    let uses_http = lower::program_uses_module(checked, "http");

    // 2. Lower to C (E0602 on failure).
    let c_source = lower::lower(checked, file)?;

    // 3. Choose + verify the C compiler (E0601).
    let cc = detect_cc();
    ensure_cc_available(&cc, file)?;

    // 4. Materialize the build directory and write sources.
    let build_dir = PathBuf::from("build");
    fs::create_dir_all(&build_dir).map_err(|e| {
        Diagnostic::from_code("E0602", format!("cannot create build directory: {}", e))
            .with_help("ensure the current directory is writable")
    })?;

    let name = Path::new(out)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "program".to_string());

    let c_path = build_dir.join(format!("{}.c", name));
    let rt_h_path = build_dir.join("ran_rt.h");
    let rt_c_path = build_dir.join("ran_rt.c");
    let obj_prog = build_dir.join(format!("{}.o", name));
    let obj_rt = build_dir.join("ran_rt.o");

    write_file(&c_path, c_source.as_bytes(), file)?;
    write_file(&rt_h_path, RAN_RT_H.as_bytes(), file)?;
    write_file(&rt_c_path, RAN_RT_C.as_bytes(), file)?;

    // 5. Compile the generated program and the runtime to object files (E0603).
    //    When `db` is used, both are compiled with `-DRAN_ENABLE_SQLITE` so the
    //    runtime's SQLite section is included and `<sqlite3.h>` is in scope.
    let defines: &[&str] = if uses_db && uses_http {
        &["RAN_ENABLE_SQLITE", "RAN_ENABLE_HTTP"]
    } else if uses_db {
        &["RAN_ENABLE_SQLITE"]
    } else if uses_http {
        &["RAN_ENABLE_HTTP"]
    } else {
        &[]
    };
    compile_object(&cc, &c_path, &obj_prog, &build_dir, file, defines)?;
    compile_object(&cc, &rt_c_path, &obj_rt, &build_dir, file, defines)?;

    // 6. Link to a temp path, then atomically rename onto `out` (E0604).
    let tmp_out = build_dir.join(format!(".{}.tmp-{}", name, std::process::id()));
    let mut link_cmd = Command::new(&cc);
    link_cmd
        .arg("-O2")
        .arg("-flto")
        .arg(&obj_prog)
        .arg(&obj_rt)
        .arg("-o")
        .arg(&tmp_out)
        // D2's runtime uses libm (pow/fmod/isnan) for float + decimal display.
        .arg("-lm");
    if uses_db {
        // The db module is implemented in the C runtime via direct libsqlite3
        // FFI (`#include <sqlite3.h>`); link the system library. Dynamic linking
        // is used here (the same system library the interpreter reaches via
        // dlopen); a fully static archive is a later task (13.3). Placed after
        // the objects so the linker resolves the runtime's sqlite3_* references.
        link_cmd.arg("-lsqlite3");
    }
    if uses_http {
        // The http client's `https://` transport uses the system OpenSSL via
        // direct FFI (`#include <openssl/ssl.h>`). Linked after the objects so
        // the runtime's SSL_*/X509_* references resolve. Plain `http://` uses
        // only libc sockets and needs no extra library.
        link_cmd.arg("-lssl").arg("-lcrypto");
    }
    if opts.link_static {
        // D1 links nothing beyond the C runtime; `-static` is harmless here and
        // makes the intent explicit. Full static-lib wiring is task 13.3.
        link_cmd.arg("-static");
    }
    let link = link_cmd.output();
    match link {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let _ = fs::remove_file(&tmp_out);
            let stderr = String::from_utf8_lossy(&o.stderr);
            return Err(Diagnostic::from_code(
                "E0604",
                format!("linking the native binary failed:\n{}", stderr.trim()),
            )
            .with_help("the generated C linked against the Ran runtime failed; \
                        this is a codegen bug — please report it"));
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp_out);
            return Err(Diagnostic::from_code("E0604", format!("could not run the linker: {}", e))
                .with_help("ensure the C compiler can link executables"));
        }
    }

    // 7. Atomic publish: rename temp onto the final output path.
    if let Err(e) = fs::rename(&tmp_out, out) {
        let _ = fs::remove_file(&tmp_out);
        return Err(Diagnostic::from_code("E0604", format!("cannot place output binary `{}`: {}", out, e))
            .with_help("ensure the output path is writable"));
    }

    Ok(())
}

fn write_file(path: &Path, bytes: &[u8], file: &str) -> Result<(), Diagnostic> {
    fs::write(path, bytes).map_err(|e| {
        Diagnostic::from_code(
            "E0602",
            format!("cannot write `{}`: {}", path.display(), e),
        )
        .with_help(format!("ensure the build directory is writable; source: {}", file))
    })
}

fn compile_object(
    cc: &str,
    src: &Path,
    obj: &Path,
    build_dir: &Path,
    file: &str,
    defines: &[&str],
) -> Result<(), Diagnostic> {
    let mut cmd = Command::new(cc);
    cmd.arg("-O2")
        .arg("-flto")
        .arg("-std=c11")
        .arg(format!("-I{}", build_dir.display()));
    for d in defines {
        cmd.arg(format!("-D{}", d));
    }
    let out = cmd
        .arg("-c")
        .arg(src)
        .arg("-o")
        .arg(obj)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            Err(Diagnostic::from_code(
                "E0603",
                format!("compiling `{}` failed:\n{}", src.display(), stderr.trim()),
            )
            .with_help("the generated C did not compile; this is a codegen bug — please report it"))
        }
        Err(e) => Err(Diagnostic::from_code(
            "E0603",
            format!("could not run the C compiler `{}`: {}", cc, e),
        )
        .with_help(format!("ensure `{}` is a working C compiler; source: {}", cc, file))),
    }
}
