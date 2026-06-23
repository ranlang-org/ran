//! Error reporting system - Rust-grade strict diagnostics.
//!
//! Features:
//! - Precise source location (file, line, column, span)
//! - Error codes (E0001, E0002, ...)
//! - Multi-line context display with underline markers
//! - Suggestions and help messages
//! - Warning vs Error vs Fatal distinction
//! - Color terminal output

use std::fmt;

/// Severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Help,
    Note,
    Warning,
    Error,
    Fatal,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Help => write!(f, "\x1b[36mhelp\x1b[0m"),
            Severity::Note => write!(f, "\x1b[34mnote\x1b[0m"),
            Severity::Warning => write!(f, "\x1b[33mwarning\x1b[0m"),
            Severity::Error => write!(f, "\x1b[31;1merror\x1b[0m"),
            Severity::Fatal => write!(f, "\x1b[31;1mfatal\x1b[0m"),
        }
    }
}

/// Source location
#[derive(Debug, Clone)]
pub struct SourceLoc {
    pub file: String,
    pub line: usize,
    pub col: usize,
    pub end_col: usize,
}

impl SourceLoc {
    pub fn new(file: &str, line: usize, col: usize, end_col: usize) -> Self {
        Self {
            file: file.to_string(),
            line,
            col,
            end_col,
        }
    }
}

/// A diagnostic (error, warning, note, etc.)
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
    pub loc: Option<SourceLoc>,
    pub labels: Vec<Label>,
    pub help: Option<String>,
    pub note: Option<String>,
}

/// A label pointing to a specific span in the source
#[derive(Debug, Clone)]
pub struct Label {
    pub loc: SourceLoc,
    pub message: String,
    pub is_primary: bool,
}

impl Diagnostic {
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            message: msg.into(),
            loc: None,
            labels: Vec::new(),
            help: None,
            note: None,
        }
    }

    pub fn warning(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: None,
            message: msg.into(),
            loc: None,
            labels: Vec::new(),
            help: None,
            note: None,
        }
    }

    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }

    /// Construct a diagnostic from a registered error code (see [`DIAGNOSTIC_CATALOG`]).
    ///
    /// The severity and a default fix hint are pre-filled from the catalog so call
    /// sites stay consistent with the project diagnostic standard (code `E####`,
    /// `file:line:col` location added via [`Diagnostic::with_loc`], and a fix hint).
    /// Unknown codes fall back to [`Severity::Error`] with no default hint.
    pub fn from_code(code: &str, message: impl Into<String>) -> Self {
        match code_info(code) {
            Some(info) => Self {
                severity: info.severity,
                code: Some(code.to_string()),
                message: message.into(),
                loc: None,
                labels: Vec::new(),
                help: Some(info.hint.to_string()),
                note: None,
            },
            None => Self::error(message).with_code(code),
        }
    }

    /// Builder variant of [`Diagnostic::from_code`]: attach a registered code to an
    /// existing diagnostic, adopting its catalog severity and (unless one was already
    /// set) its default fix hint. Unknown codes only set the code string.
    pub fn with_known_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        if let Some(info) = code_info(code) {
            self.severity = info.severity;
            if self.help.is_none() {
                self.help = Some(info.hint.to_string());
            }
        }
        self
    }

    pub fn with_loc(mut self, loc: SourceLoc) -> Self {
        self.loc = Some(loc);
        self
    }

    pub fn with_label(mut self, loc: SourceLoc, msg: impl Into<String>) -> Self {
        self.labels.push(Label {
            loc,
            message: msg.into(),
            is_primary: self.labels.is_empty(),
        });
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Render the diagnostic to stderr
    pub fn emit(&self, source: &str) {
        let lines: Vec<&str> = source.lines().collect();

        // Header: error[E0001]: message
        if let Some(ref code) = self.code {
            eprint!("{}[{}]", self.severity, code);
        } else {
            eprint!("{}", self.severity);
        }
        eprintln!(": {}", self.message);

        // Location arrow
        if let Some(ref loc) = self.loc {
            eprintln!(
                "  \x1b[34m-->\x1b[0m {}:{}:{}",
                loc.file, loc.line, loc.col
            );
        }

        // Source context with labels
        for label in &self.labels {
            let line_idx = label.loc.line.saturating_sub(1);
            let line_num_width = format!("{}", label.loc.line).len();

            eprintln!("{:>width$} \x1b[34m|\x1b[0m", "", width = line_num_width);

            if line_idx < lines.len() {
                eprintln!(
                    "\x1b[34m{:>width$}\x1b[0m \x1b[34m|\x1b[0m {}",
                    label.loc.line,
                    lines[line_idx],
                    width = line_num_width
                );

                // Underline
                let padding = label.loc.col.saturating_sub(1);
                let underline_len = label.loc.end_col.saturating_sub(label.loc.col).max(1);
                let color = if label.is_primary { "\x1b[31;1m" } else { "\x1b[34m" };
                let marker = if label.is_primary { "^" } else { "-" };

                eprintln!(
                    "{:>width$} \x1b[34m|\x1b[0m {:>pad$}{}{}{} \x1b[0m{}",
                    "",
                    "",
                    color,
                    marker.repeat(underline_len),
                    "",
                    label.message,
                    width = line_num_width,
                    pad = padding,
                );
            }
        }

        // Help suggestion
        if let Some(ref help) = self.help {
            eprintln!("  \x1b[36m= help\x1b[0m: {}", help);
        }

        // Note
        if let Some(ref note) = self.note {
            eprintln!("  \x1b[34m= note\x1b[0m: {}", note);
        }

        eprintln!();
    }
}

/// Diagnostics collector
pub struct DiagnosticEngine {
    pub diagnostics: Vec<Diagnostic>,
    pub source: String,
    pub file: String,
    pub has_errors: bool,
}

impl DiagnosticEngine {
    pub fn new(file: &str, source: &str) -> Self {
        Self {
            diagnostics: Vec::new(),
            source: source.to_string(),
            file: file.to_string(),
            has_errors: false,
        }
    }

    pub fn report(&mut self, diag: Diagnostic) {
        if diag.severity >= Severity::Error {
            self.has_errors = true;
        }
        self.diagnostics.push(diag);
    }

    pub fn emit_all(&self) {
        for diag in &self.diagnostics {
            diag.emit(&self.source);
        }
        if self.has_errors {
            let err_count = self
                .diagnostics
                .iter()
                .filter(|d| d.severity >= Severity::Error)
                .count();
            let warn_count = self
                .diagnostics
                .iter()
                .filter(|d| d.severity == Severity::Warning)
                .count();
            eprint!("\x1b[31;1merror\x1b[0m: aborting due to");
            if err_count > 0 {
                eprint!(" {} error{}", err_count, if err_count > 1 { "s" } else { "" });
            }
            if warn_count > 0 {
                eprint!("; {} warning{}", warn_count, if warn_count > 1 { "s" } else { "" });
            }
            eprintln!(" emitted");
        }
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity >= Severity::Error)
            .count()
    }

    /// Number of stored `Warning`-severity diagnostics (used to decide whether
    /// to flush warnings on an otherwise-clean build).
    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }
}

/// Emit syntax diagnostics (from the lexer/parser) and abort the process if any
/// are present. The file name is patched into each diagnostic's locations so the
/// `-->` line points at the right source. Returns normally only when empty.
pub fn abort_on_syntax_errors(diags: Vec<Diagnostic>, file: &str, source: &str) {
    if diags.is_empty() {
        return;
    }
    let mut engine = DiagnosticEngine::new(file, source);
    for mut d in diags {
        if let Some(loc) = d.loc.as_mut() {
            loc.file = file.to_string();
        }
        for label in &mut d.labels {
            label.loc.file = file.to_string();
        }
        engine.report(d);
    }
    engine.emit_all();
    std::process::exit(1);
}

// ============================================================================
// Diagnostic code catalog (Enterprise Runtime Capabilities)
// ============================================================================
//
// Central registry mapping each new error code to its severity and a default
// fix hint, derived from the design "Error Handling" tables. Call sites use
// `Diagnostic::from_code(code, msg)` / `Diagnostic::with_known_code(code)` so
// every emitted diagnostic carries a consistent `E####` code, location
// (`file:line:col` via `with_loc`), and a single fix hint.
//
// Severity rules (per design):
//   - "error" and "error (handleable)" map to `Severity::Error`. Handleable
//     errors are still errors at the diagnostic level; the runtime additionally
//     surfaces them as catchable Ran error values (Map{error,code,message}).
//   - Build resource warnings (E0701/E0702/E0703) map to `Severity::Warning`
//     (non-blocking, build continues conservatively).
//   - E0704 (build aborted on memory pressure) maps to `Severity::Error`.

/// Static metadata for a registered diagnostic code.
#[derive(Debug, Clone, Copy)]
pub struct CodeInfo {
    /// The `E####` code string.
    pub code: &'static str,
    /// Diagnostic severity (error vs warning).
    pub severity: Severity,
    /// Whether this is a "handleable" runtime error: still `Severity::Error`,
    /// but additionally returned to Ran programs as a catchable error value
    /// without crashing the runtime. `false` for compile-time/abort errors and
    /// for build warnings.
    pub handleable: bool,
    /// Default fix hint (Indonesian), matching the design tables. Hints may
    /// contain `<...>` placeholders that call sites can refine via `with_help`.
    pub hint: &'static str,
}

/// The complete catalog of diagnostic codes introduced by the Enterprise
/// Runtime Capabilities and memory-safe-self-hosting features, grouped by
/// capability range: `E02xx` ownership/borrow, `E04xx` penyajian web native,
/// `E05xx` SQLite/`db` plus the poisoned-mutex recovery code (`E0511`),
/// `E06xx` native AOT codegen (`E0601`–`E0606`) and concurrency
/// (`E0610`–`E0614`), `E07xx` resource-aware build, and the Phase A crash
/// hardening faults `E10xx` (recursion guard `E1007`, bounded VM `E1008`/`E1009`,
/// checked arithmetic `E1010`/`E1011`, bounds-safe indexing `E1012`, and the
/// recoverable library-code assertion fault `E1013`).
pub const DIAGNOSTIC_CATALOG: &[CodeInfo] = &[
    // -- Kelompok C: Ownership / borrow (E02xx) --------------------------------
    CodeInfo {
        code: "E0210",
        severity: Severity::Error,
        handleable: false,
        hint: "Nilai sudah dipindahkan di <lokasi move>. Klona nilai atau pinjam dengan `&`.",
    },
    CodeInfo {
        code: "E0212",
        severity: Severity::Error,
        handleable: false,
        hint: "Borrow bentrok dengan borrow aktif di <lokasi>. Persempit masa hidup borrow.",
    },
    CodeInfo {
        code: "E0214",
        severity: Severity::Error,
        handleable: false,
        hint: "Referensi hidup lebih lama dari nilainya. Kembalikan nilai berkepemilikan atau perpendek borrow.",
    },
    CodeInfo {
        code: "E0215",
        severity: Severity::Error,
        handleable: false,
        hint: "Tidak bisa memindahkan nilai yang masih dipinjam di <lokasi borrow>. Akhiri borrow dulu.",
    },
    // -- Kelompok A: Penyajian Web Native (E04xx) ------------------------------
    CodeInfo {
        code: "E0401",
        severity: Severity::Error,
        handleable: false,
        hint: "Direktori akar web (Web_Root) tidak ditemukan atau tidak dapat dibaca. Pastikan direktori ada dan dapat diakses.",
    },
    CodeInfo {
        code: "E0402",
        severity: Severity::Error,
        handleable: true,
        hint: "Gagal membaca berkas aset. Periksa keberadaan dan izin berkas di dalam Web_Root.",
    },
    CodeInfo {
        code: "E0403",
        severity: Severity::Error,
        handleable: false,
        hint: "Permintaan mengakses berkas di luar Web_Root (path traversal). Gunakan path relatif yang berada di dalam akar.",
    },
    CodeInfo {
        code: "E0404",
        severity: Severity::Error,
        handleable: false,
        hint: "Perintah build frontend gagal. Perbaiki perintah build sebelum menyajikan aset.",
    },
    CodeInfo {
        code: "E0405",
        severity: Severity::Warning,
        handleable: false,
        hint: "Ekstensi aset tidak dikenal; disajikan sebagai application/octet-stream. Tambahkan pemetaan tipe bila diperlukan.",
    },
    // -- Kelompok B: SQLite / `db` (E05xx) -------------------------------------
    CodeInfo {
        code: "E0501",
        severity: Severity::Error,
        handleable: false,
        hint: "Periksa izin file/direktori database pada path tersebut.",
    },
    CodeInfo {
        code: "E0502",
        severity: Severity::Error,
        handleable: false,
        hint: "File bukan database SQLite yang valid. Periksa path atau hapus file rusak.",
    },
    CodeInfo {
        code: "E0503",
        severity: Severity::Error,
        handleable: false,
        hint: "Handle koneksi tidak valid. Gunakan handle dari `db.connect` yang masih terbuka.",
    },
    CodeInfo {
        code: "E0504",
        severity: Severity::Error,
        handleable: false,
        hint: "Perbaiki SQL: <pesan dari SQLite>.",
    },
    CodeInfo {
        code: "E0505",
        severity: Severity::Error,
        handleable: true,
        hint: "Perintah melanggar constraint: <pesan dari SQLite>. Sesuaikan data.",
    },
    CodeInfo {
        code: "E0506",
        severity: Severity::Error,
        handleable: false,
        hint: "Jumlah parameter (<n>) tidak sama dengan placeholder (<m>). Samakan jumlahnya.",
    },
    CodeInfo {
        code: "E0507",
        severity: Severity::Error,
        handleable: false,
        hint: "Tipe kolom <tipe> tidak didukung. Pilih INTEGER/REAL/TEXT/NULL atau simpan sebagai TEXT.",
    },
    CodeInfo {
        code: "E0508",
        severity: Severity::Error,
        handleable: false,
        hint: "Nilai moneter tidak dapat di-parse sebagai decimal. Simpan sebagai TEXT desimal yang valid.",
    },
    CodeInfo {
        code: "E0509",
        severity: Severity::Error,
        handleable: false,
        hint: "Transaksi sudah aktif. Commit/rollback dulu sebelum `db.begin` lagi.",
    },
    CodeInfo {
        code: "E0510",
        severity: Severity::Error,
        handleable: false,
        hint: "Tidak ada transaksi aktif. Panggil `db.begin` sebelum commit/rollback.",
    },
    // -- Phase D: Native AOT codegen (E06xx, 0601–0606) ------------------------
    CodeInfo {
        // R10.6: no system C compiler available for the native build pipeline.
        code: "E0601",
        severity: Severity::Error,
        handleable: false,
        hint: "Tidak ada kompiler C (`cc`/`$CC`). Pasang gcc/clang atau set `$CC` ke path-nya.",
    },
    CodeInfo {
        // R10.6: lowering the checked program to C source failed.
        code: "E0602",
        severity: Severity::Error,
        handleable: false,
        hint: "Emit kode C gagal. Tambahkan anotasi tipe, atau build tanpa `--native` untuk memakai binary berbasis interpreter.",
    },
    CodeInfo {
        // R10.6: the system C compiler rejected the generated source.
        code: "E0603",
        severity: Severity::Error,
        handleable: false,
        hint: "Kompilasi C gagal (lihat keluaran `cc`). Ini bug codegen — laporkan.",
    },
    CodeInfo {
        // R10.6: linking the native binary against the Ran runtime failed.
        code: "E0604",
        severity: Severity::Error,
        handleable: false,
        hint: "Penautan (link) binary native gagal (lihat keluaran `cc`). Ini bug codegen — laporkan.",
    },
    CodeInfo {
        // R10.5: `--link-static` requested but a required static library is missing.
        code: "E0605",
        severity: Severity::Error,
        handleable: false,
        hint: "Pustaka statik yang dibutuhkan `--link-static` tidak ditemukan. Pasang arsip `.a`-nya atau tautkan dinamis.",
    },
    CodeInfo {
        // R10.1: a construct outside the supported native subset; a hard build
        // error — native codegen never silently falls back to the interpreter.
        code: "E0606",
        severity: Severity::Error,
        handleable: false,
        hint: "Konstruksi ini di luar subset native saat ini. Build tanpa `--native`, atau tunggu iterasi native berikutnya (D2+).",
    },
    // -- Kelompok C: Concurrency runtime (E06xx) -------------------------------
    CodeInfo {
        code: "E0610",
        severity: Severity::Error,
        handleable: true,
        hint: "`done` melebihi `add`. Pastikan jumlah `done` tidak lebih dari thread yang ditambahkan.",
    },
    CodeInfo {
        code: "E0611",
        severity: Severity::Error,
        handleable: true,
        hint: "Channel tertutup/penerima lepas. Hentikan pengiriman atau buat channel baru.",
    },
    CodeInfo {
        code: "E0612",
        severity: Severity::Error,
        handleable: true,
        hint: "Handle thread sudah di-join atau tidak valid. Simpan hasil join pertama.",
    },
    CodeInfo {
        code: "E0613",
        severity: Severity::Error,
        handleable: false,
        hint: "Akses bersama tak tersinkron. Bungkus dengan `shared`/`lock` atau kirim lewat channel.",
    },
    CodeInfo {
        code: "E0614",
        severity: Severity::Error,
        handleable: true,
        hint: "Akuisisi lock melebihi 30 detik. Periksa potensi deadlock atau perpendek seksi kritis.",
    },
    // -- Kelompok D: Resource-aware build (E07xx) ------------------------------
    CodeInfo {
        code: "E0701",
        severity: Severity::Warning,
        handleable: false,
        hint: "OS host tak dikenali; memakai anggaran memori konservatif (\u{2264}512 MiB).",
    },
    CodeInfo {
        code: "E0702",
        severity: Severity::Warning,
        handleable: false,
        hint: "Probing memori gagal (<metrik>). Memakai anggaran memori konservatif.",
    },
    CodeInfo {
        code: "E0703",
        severity: Severity::Warning,
        handleable: false,
        hint: "Batas memori tidak dipakai (tak valid atau memori tersedia \u{2264} cadangan aman). Memakai anggaran terhitung/konservatif.",
    },
    CodeInfo {
        code: "E0704",
        severity: Severity::Error,
        handleable: false,
        hint: "Build dihentikan karena memori tidak cukup. Tutup aplikasi lain atau naikkan memori/limit.",
    },
    // -- Phase A: Crash hardening — poisoned mutex (E05xx) ----------------------
    CodeInfo {
        // R4.4: a stdlib mutex was poisoned by a panicking thread; recovered as a
        // RuntimeFault instead of cascading the panic into a process crash.
        code: "E0511",
        severity: Severity::Error,
        handleable: true,
        hint: "Operasi sebelumnya gagal di tengah; state dipulihkan — coba ulangi transaksi.",
    },
    // -- Phase A: Crash hardening — runtime safety faults (E10xx) ---------------
    CodeInfo {
        // R1.2/R1.6: recursion-depth guard tripped at the function-call boundary
        // before the next frame is allocated (prevents an uncatchable SIGSEGV).
        code: "E1007",
        severity: Severity::Error,
        handleable: true,
        hint: "Kurangi rekursi atau naikkan batas dengan `--max-depth=<N>`; kemungkinan rekursi tak-berbatas (base case tak tercapai).",
    },
    CodeInfo {
        // R6.1: bounded Bytecode_VM exceeded its instruction (step) budget; a
        // recoverable error so the `--vm` path can fall back to the interpreter.
        code: "E1008",
        severity: Severity::Error,
        handleable: true,
        hint: "Eksekusi VM melampaui anggaran langkah (step budget). Periksa loop tak-berujung atau jalankan ulang lewat interpreter.",
    },
    CodeInfo {
        // R6.2: bounded Bytecode_VM exceeded its value-stack capacity; recoverable
        // so execution can fall back to the interpreter instead of growing forever.
        code: "E1009",
        severity: Severity::Error,
        handleable: true,
        hint: "Kedalaman value stack VM melampaui kapasitas. Periksa rekursi/ekspresi sangat dalam atau jalankan lewat interpreter.",
    },
    CodeInfo {
        // R7.1: checked integer arithmetic detected an overflow instead of wrapping
        // or producing an undefined value.
        code: "E1010",
        severity: Severity::Error,
        handleable: true,
        hint: "Operasi integer melebihi rentang (overflow). Gunakan nilai lebih kecil atau tipe decimal untuk perhitungan besar.",
    },
    CodeInfo {
        // R7.2: integer division/modulo by zero.
        code: "E1011",
        severity: Severity::Error,
        handleable: true,
        hint: "Pembagian/modulo dengan nol. Pastikan pembagi bukan nol sebelum operasi.",
    },
    CodeInfo {
        // R7.3: array/string index out of bounds; the message carries the offending
        // index and the length.
        code: "E1012",
        severity: Severity::Error,
        handleable: true,
        hint: "Indeks di luar batas (<indeks> dari panjang <panjang>). Periksa rentang sebelum mengakses elemen.",
    },
    CodeInfo {
        // R3.1/R4.3: a library-code `assert` failure (outside `ran test` mode) is
        // raised as a recoverable RuntimeFault that unwinds to the nearest catch
        // boundary instead of calling `process::exit`, so a failed assertion can
        // be caught rather than killing the process.
        code: "E1013",
        severity: Severity::Error,
        handleable: true,
        hint: "Periksa kondisi `assert`; tangani dengan `try`/recover bila kegagalan dapat dipulihkan.",
    },
];

/// Look up the catalog metadata for a diagnostic code, if registered.
pub fn code_info(code: &str) -> Option<&'static CodeInfo> {
    DIAGNOSTIC_CATALOG.iter().find(|info| info.code == code)
}

/// Convenience accessor: the (severity, fix hint) pair for a registered code.
pub fn code_severity_hint(code: &str) -> Option<(Severity, &'static str)> {
    code_info(code).map(|info| (info.severity, info.hint))
}

/// Whether a registered code is a "handleable" runtime error (surfaced to Ran
/// programs as a catchable error value in addition to the diagnostic).
pub fn is_handleable(code: &str) -> bool {
    code_info(code).map(|info| info.handleable).unwrap_or(false)
}

#[cfg(test)]
mod catalog_tests {
    use super::*;

    #[test]
    fn every_expected_code_is_registered() {
        let expected = [
            // ownership/borrow
            "E0210", "E0212", "E0214", "E0215",
            // Penyajian Web Native
            "E0401", "E0402", "E0403", "E0404", "E0405",
            // SQLite/db
            "E0501", "E0502", "E0503", "E0504", "E0505", "E0506", "E0507", "E0508",
            "E0509", "E0510",
            // Phase D native AOT codegen
            "E0601", "E0602", "E0603", "E0604", "E0605", "E0606",
            // concurrency
            "E0610", "E0611", "E0612", "E0613", "E0614",
            // build
            "E0701", "E0702", "E0703", "E0704",
            // Phase A crash hardening: poisoned mutex + runtime safety faults
            "E0511", "E1007", "E1008", "E1009", "E1010", "E1011", "E1012", "E1013",
        ];
        for code in expected {
            assert!(
                code_info(code).is_some(),
                "diagnostic code {code} missing from catalog"
            );
        }
        // Catalog should contain exactly the expected codes (no extras, no dups).
        assert_eq!(DIAGNOSTIC_CATALOG.len(), expected.len());
    }

    #[test]
    fn no_duplicate_codes() {
        for (i, a) in DIAGNOSTIC_CATALOG.iter().enumerate() {
            for b in &DIAGNOSTIC_CATALOG[i + 1..] {
                assert_ne!(a.code, b.code, "duplicate code {} in catalog", a.code);
            }
        }
    }

    #[test]
    fn every_code_has_a_nonempty_hint() {
        for info in DIAGNOSTIC_CATALOG {
            assert!(
                !info.hint.trim().is_empty(),
                "code {} has an empty fix hint",
                info.code
            );
        }
    }

    #[test]
    fn build_warnings_are_warning_severity() {
        for code in ["E0701", "E0702", "E0703"] {
            assert_eq!(
                code_info(code).unwrap().severity,
                Severity::Warning,
                "{code} should be a warning"
            );
        }
        // E0704 is an abort-level error, not a warning.
        assert_eq!(code_info("E0704").unwrap().severity, Severity::Error);
    }

    #[test]
    fn non_build_codes_are_error_severity() {
        for info in DIAGNOSTIC_CATALOG {
            // E0701/E0702/E0703 (build) and E0405 (unknown web asset type) are warnings.
            if matches!(info.code, "E0701" | "E0702" | "E0703" | "E0405") {
                continue;
            }
            assert_eq!(
                info.severity,
                Severity::Error,
                "{} should be error severity",
                info.code
            );
        }
    }

    #[test]
    fn handleable_codes_match_design() {
        let handleable = [
            "E0402", // Web: gagal baca aset (handleable)
            "E0505", // DB handleable (constraint)
            "E0610", "E0611", "E0612", "E0614", // concurrency handleable
            // Phase A crash hardening: recoverable runtime faults
            "E0511", "E1007", "E1008", "E1009", "E1010", "E1011", "E1012", "E1013",
        ];
        for code in handleable {
            assert!(is_handleable(code), "{code} should be handleable");
            // Handleable codes are still error severity.
            assert_eq!(code_info(code).unwrap().severity, Severity::Error);
        }
        // A couple of non-handleable spot checks.
        for code in ["E0210", "E0403", "E0613", "E0701", "E0704"] {
            assert!(!is_handleable(code), "{code} should not be handleable");
        }
    }

    #[test]
    fn from_code_prefills_severity_and_hint() {
        let d = Diagnostic::from_code("E0401", "web root missing");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code.as_deref(), Some("E0401"));
        assert_eq!(d.message, "web root missing");
        assert_eq!(d.help.as_deref(), code_info("E0401").map(|i| i.hint));

        // Warning-level code retains its warning severity.
        let w = Diagnostic::from_code("E0701", "unknown OS");
        assert_eq!(w.severity, Severity::Warning);
    }

    #[test]
    fn from_code_unknown_falls_back_to_error() {
        let d = Diagnostic::from_code("E9999", "mystery");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code.as_deref(), Some("E9999"));
        assert!(d.help.is_none());
    }

    #[test]
    fn with_known_code_adopts_severity_and_keeps_explicit_help() {
        // Adopts warning severity from catalog.
        let d = Diagnostic::error("probe failed").with_known_code("E0702");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.help.as_deref(), code_info("E0702").map(|i| i.hint));

        // An explicitly-set help is preserved, not overwritten by the default.
        let d2 = Diagnostic::error("probe failed")
            .with_help("custom hint")
            .with_known_code("E0702");
        assert_eq!(d2.help.as_deref(), Some("custom hint"));
        assert_eq!(d2.severity, Severity::Warning);
    }
}

// ============================================================================
// Property 19 — Diagnostics are consistent across the toolchain
// (R16.1 code, R16.2 one help line, R16.3 file:line:col when known).
// ============================================================================
#[cfg(test)]
mod toolchain_consistency_property {
    // Feature: memory-safe-self-hosting, Property 19: Diagnostics are consistent across the toolchain (E#### code, one help line, file:line:col when source location is known)
    use super::*;
    use crate::support::pbt::{self, Gen, Rng};

    /// One generated diagnostic request: a registered catalog code, a free-form
    /// user-facing message, and an optional source location. Driving the
    /// diagnostic through the real `Diagnostic::from_code` / `with_loc` builders
    /// is what makes this a faithful toolchain check — every emitted diagnostic
    /// flows through the same path.
    #[derive(Clone, Debug)]
    struct DiagInput {
        /// Index into [`DIAGNOSTIC_CATALOG`] so the property covers every code.
        code_idx: usize,
        /// Arbitrary message text (may contain Unicode / newlines).
        message: String,
        /// Optional `(file, line, col, end_col)` source location.
        loc: Option<(String, usize, usize, usize)>,
    }

    /// Generate a catalog code index, a random message, and an optional random
    /// `SourceLoc`. Shrinks by dropping the location first, then emptying the
    /// message — keeping any counterexample minimal and easy to read.
    fn input_gen() -> Gen<DiagInput> {
        let msg_gen = pbt::string(40);
        let file_gen = pbt::string(24);
        Gen::new(
            move |rng: &mut Rng, size| {
                let code_idx = rng.below(DIAGNOSTIC_CATALOG.len() as u64) as usize;
                let message = msg_gen.generate(rng, size);
                let loc = if rng.boolean() {
                    let file = {
                        let f = file_gen.generate(rng, size);
                        if f.trim().is_empty() {
                            "input.ran".to_string()
                        } else {
                            f
                        }
                    };
                    let line = 1 + rng.upto(10_000);
                    let col = 1 + rng.upto(500);
                    let end_col = col + rng.upto(80);
                    Some((file, line, col, end_col))
                } else {
                    None
                };
                DiagInput {
                    code_idx,
                    message,
                    loc,
                }
            },
            |inp: &DiagInput| {
                let mut out = Vec::new();
                // Prefer the simpler no-location variant.
                if inp.loc.is_some() {
                    out.push(DiagInput {
                        loc: None,
                        ..inp.clone()
                    });
                }
                // Then collapse the message.
                if !inp.message.is_empty() {
                    out.push(DiagInput {
                        message: String::new(),
                        ..inp.clone()
                    });
                }
                out
            },
        )
    }

    /// The invariant: a diagnostic built from any registered code carries the
    /// `E####` code, exactly one non-empty help line, and a well-formed
    /// `file:line:col` location exactly when (and only when) one was attached.
    fn diagnostic_is_consistent(inp: &DiagInput) -> bool {
        let info = &DIAGNOSTIC_CATALOG[inp.code_idx];

        let mut d = Diagnostic::from_code(info.code, inp.message.clone());
        if let Some((file, line, col, end_col)) = &inp.loc {
            d = d.with_loc(SourceLoc::new(file, *line, *col, *end_col));
        }

        // R16.1 — the diagnostic carries its `E####` code.
        if d.code.as_deref() != Some(info.code) {
            return false;
        }

        // R16.2 — exactly one help line, and it is non-empty.
        match &d.help {
            Some(h) => {
                if h.trim().is_empty() {
                    return false;
                }
                if h.lines().count() != 1 {
                    return false;
                }
            }
            None => return false,
        }

        // R16.3 — `file:line:col` is present iff a source location is set, and
        // the structured location round-trips the values we supplied.
        match (&inp.loc, &d.loc) {
            (Some((file, line, col, _)), Some(loc)) => {
                if &loc.file != file || loc.line != *line || loc.col != *col {
                    return false;
                }
                // The rendered `file:line:col` segment (see `Diagnostic::emit`)
                // is well-formed: at least the two value separators are present.
                let rendered = format!("{}:{}:{}", loc.file, loc.line, loc.col);
                if rendered.matches(':').count() < 2 {
                    return false;
                }
            }
            (None, None) => {}
            // Location appeared/disappeared unexpectedly.
            _ => return false,
        }

        true
    }

    /// Property 19: for every registered diagnostic code and arbitrary
    /// message/location, a `Diagnostic::from_code(..).with_loc(..)` carries a
    /// consistent `E####` code, a single help line, and `file:line:col` exactly
    /// when a source location is known. Covers the whole catalog via random
    /// code selection.
    ///
    /// Validates: Requirements 16.1, 16.2, 16.3 (design refs 1.6, 2.6, 4.6, 5.6)
    #[test]
    fn prop_diagnostics_consistent_across_toolchain() {
        pbt::for_all(
            "P19 diagnostics consistent across the toolchain",
            &input_gen(),
            diagnostic_is_consistent,
        );
    }
}
