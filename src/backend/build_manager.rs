//! Resource-aware build manager (Kelompok D, R14–R17).
//!
//! `ran build` should make full use of the host's memory while always leaving a
//! safety reserve so the OS is not pushed into thrashing or an OOM kill. This
//! module is the entry point for that behaviour:
//!
//! 1. **Detect the host OS** (R14) from `std::env::consts::OS`. This is a
//!    compile-time constant, so detection is instant and trivially within the
//!    2-second budget — there is no probing or I/O involved. An unrecognised OS
//!    yields the `E0701` warning and a conservative budget (≤ 512 MiB).
//! 2. **Probe memory** (R15) via [`crate::support::sysinfo::probe_memory`],
//!    which already enforces the 2000 ms cap and reports `ok == false` on
//!    failure/timeout/inconsistent metrics.
//! 3. **Compute an adaptive budget** with a safety reserve (R16): see
//!    [`compute_budget`], the pure numeric core that the property test (P11,
//!    task 10.6) exercises directly.
//! 4. **Degrade gracefully** under memory pressure (R17): [`BuildResourceManager::tick`]
//!    walks the [`Degradation`] ladder (`Normal → ReduceParallelism → Serialize →
//!    Delay → Abort`) driven by the pure [`next_degradation`] policy, and
//!    [`BuildResourceManager::finish`] releases build-management state (R17.5).
//!
//! Diagnostics are produced via the shared catalog
//! ([`crate::support::diagnostics`]). Because the build runs without a concrete
//! source `file:line:col`, build-time warnings are emitted as location-less
//! diagnostics (the project diagnostic renderer simply omits the `-->` line).
//! They are collected in [`BuildResourceManager::warnings`] so the `cmd_build`
//! integration (task 10.4) can surface them.

#![allow(dead_code)] // tick/finish and several accessors are wired up by tasks 10.3/10.4.

use crate::support::diagnostics::Diagnostic;
use crate::support::sysinfo::{cpu_count, probe_memory, MemorySnapshot};
use std::time::Duration;

/// Conservative fallback budget used whenever we cannot trust the host metrics:
/// unknown OS (R14.2), failed/inconsistent probe (R15.2/R15.3), or available
/// memory at or below the safety reserve (R16.5). 512 MiB.
pub const CONSERVATIVE_DEFAULT: u64 = 512 * 1024 * 1024;

/// Floor for the safety reserve: never reserve less than 512 MB (R16.2).
const SAFETY_FLOOR: u64 = 512 * 1024 * 1024;

/// How long the memory probe is allowed to take. The probe itself also enforces
/// a hard 2000 ms cap (R15.1); we pass the same value for clarity.
const PROBE_TIMEOUT: Duration = Duration::from_millis(2000);

// ---------------------------------------------------------------------------
// Host OS detection (R14)
// ---------------------------------------------------------------------------

/// The host operating system, as far as the build manager needs to distinguish
/// it. Each known variant maps one-to-one to a memory probing method (R14.3);
/// `Unknown` triggers the conservative fallback (R14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOs {
    Windows,
    MacOs,
    Linux,
    Unknown,
}

impl HostOs {
    /// Detect the host OS from `std::env::consts::OS`.
    ///
    /// This is a compile-time constant lookup: it is deterministic and instant,
    /// so it always completes well within the 2-second budget of R14.1, and it
    /// runs *before* any memory probing.
    pub fn detect() -> Self {
        Self::from_os_str(std::env::consts::OS)
    }

    /// Pure mapping from an OS identifier string to a [`HostOs`]. Factored out so
    /// the classification can be unit-tested without depending on the build host.
    pub fn from_os_str(os: &str) -> Self {
        match os {
            "windows" => HostOs::Windows,
            "macos" => HostOs::MacOs,
            "linux" => HostOs::Linux,
            _ => HostOs::Unknown,
        }
    }

    /// Whether this OS is one of the three explicitly supported platforms.
    pub fn is_known(self) -> bool {
        !matches!(self, HostOs::Unknown)
    }
}

// ---------------------------------------------------------------------------
// Pure budget computation (R16) — exercised by property test P11 (task 10.6)
// ---------------------------------------------------------------------------

/// A non-fatal reason the memory budget was adjusted. Both map to the `E0703`
/// warning, but carrying the cause lets [`BuildResourceManager::init`] produce a
/// specific, helpful message while keeping [`compute_budget`] pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetWarning {
    /// Available memory is at or below the safety reserve, so the conservative
    /// default budget is used instead of an adaptive one (R16.5).
    AvailableBelowReserve,
    /// The user's explicit memory limit was invalid (`== 0` or `> total`) and
    /// was therefore ignored in favour of the computed budget (R16.6).
    InvalidUserLimit,
}

impl BudgetWarning {
    /// The diagnostic code for this warning (always `E0703`).
    pub fn code(self) -> &'static str {
        "E0703"
    }
}

/// The safety reserve for a host with `total` physical bytes: the larger of 10%
/// of total and 512 MB (R16.2).
pub fn safety_reserve(total: u64) -> u64 {
    (total / 10).max(SAFETY_FLOOR)
}

/// Compute the memory budget for a **consistent** memory snapshot.
///
/// This is the pure numeric core of the build manager (the algorithm from the
/// design's "Model Build_Resource_Manager"):
///
/// ```text
/// reserve = max(total/10, 512 MB)                                   (R16.2)
/// budget  = if available > reserve { available - reserve }
///           else { CONSERVATIVE_DEFAULT }                           (R16.1/16.5)
/// budget  = min(budget, user_limit)   when user_limit is valid      (R16.4)
/// ```
///
/// A `user_limit` of `0` or one greater than `total` is invalid and is ignored
/// (R16.6). Returns `(reserve, budget, warnings)` where `warnings` lists the
/// reasons (if any) the budget deviated from a plain adaptive value. Keeping
/// this free of I/O and diagnostics makes it reusable by the P11 property test.
pub fn compute_budget(
    total: u64,
    available: u64,
    user_limit: Option<u64>,
) -> (u64, u64, Vec<BudgetWarning>) {
    let mut warnings = Vec::new();
    let reserve = safety_reserve(total);

    let mut budget = if available > reserve {
        available - reserve
    } else {
        // Not enough headroom to carve out the reserve: fall back conservatively.
        warnings.push(BudgetWarning::AvailableBelowReserve);
        CONSERVATIVE_DEFAULT
    };

    if let Some(limit) = user_limit {
        if limit == 0 || limit > total {
            warnings.push(BudgetWarning::InvalidUserLimit);
        } else {
            budget = budget.min(limit);
        }
    }

    (reserve, budget, warnings)
}

// ---------------------------------------------------------------------------
// Degradation strategy (R17) — fully implemented by task 10.3
// ---------------------------------------------------------------------------

/// The action the build loop should take on each [`BuildResourceManager::tick`]
/// to keep memory usage within budget (R17). Defined here; the policy that
/// chooses between the variants is implemented in task 10.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Degradation {
    /// Proceed at full parallelism.
    Normal,
    /// Reduce build parallelism to the given number of concurrent jobs (R17.1).
    ReduceParallelism(usize),
    /// Run at most one job at a time (R17.2).
    Serialize,
    /// Delay starting the next job for the given duration (R17.3).
    Delay(Duration),
    /// Abort the build: memory pressure could not be relieved (R17.4, `E0704`).
    Abort,
}

// ---------------------------------------------------------------------------
// Degradation ladder state machine (R17) — pure, snapshot-injectable
// ---------------------------------------------------------------------------

/// The longest the build may delay starting the next job before it must abort
/// (R17.3 default delay budget: 300 s).
pub const MAX_DELAY: Duration = Duration::from_secs(300);

/// Per-tick backoff increment once the build is fully serialized and still over
/// budget. Successive over-pressure ticks accumulate this until [`MAX_DELAY`] is
/// reached, at which point the build aborts (R17.3 → R17.4).
const DELAY_STEP_MS: u64 = 60_000; // 60 s

/// [`MAX_DELAY`] expressed in milliseconds, for the accumulator arithmetic.
const MAX_DELAY_MS: u64 = 300_000;

/// Where on the degradation ladder the build currently sits (R17). This is the
/// manager's internal memory between ticks: sustained pressure walks it *down*
/// (`Full → Reduced → Serial → Delayed → Aborted`) one rung per tick, and relief
/// walks it back *up* toward `Full`. `Aborted` is terminal.
///
/// Kept separate from the public [`Degradation`] (which is what a single `tick`
/// *reports*) so the ladder can carry extra state — the current job count while
/// reducing parallelism, and the accumulated delay while backing off — without
/// leaking it into the build loop's API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStage {
    /// Full parallelism, no pressure.
    Full,
    /// Reduced to `n` concurrent jobs, where `2 <= n < base_parallelism` (R17.1).
    Reduced(usize),
    /// At most one job at a time (R17.2).
    Serial,
    /// Backing off: the accumulated delay so far, in milliseconds (R17.3).
    Delayed(u64),
    /// Build aborted under sustained pressure (R17.4); terminal.
    Aborted,
}

impl BuildStage {
    /// The [`Degradation`] a build loop should act on for this stage. The delay
    /// is clamped to [`MAX_DELAY`] so a reported delay never exceeds 300 s (R17.3).
    pub fn degradation(self) -> Degradation {
        match self {
            BuildStage::Full => Degradation::Normal,
            BuildStage::Reduced(n) => Degradation::ReduceParallelism(n),
            BuildStage::Serial => Degradation::Serialize,
            BuildStage::Delayed(ms) => {
                Degradation::Delay(Duration::from_millis(ms.min(MAX_DELAY_MS)))
            }
            BuildStage::Aborted => Degradation::Abort,
        }
    }
}

/// Move one rung *down* the ladder under continued over-budget pressure.
fn step_down(stage: BuildStage, base: usize) -> BuildStage {
    match stage {
        // From full parallelism, shed one job if there is room to (need at least
        // 3 jobs to land on a `Reduced(>=2)` rung); otherwise serialize directly.
        BuildStage::Full => {
            if base >= 3 {
                BuildStage::Reduced(base - 1)
            } else {
                BuildStage::Serial
            }
        }
        // Shed one more job, bottoming out at a single job (Serial == 1 job).
        BuildStage::Reduced(n) => {
            if n > 2 {
                BuildStage::Reduced(n - 1)
            } else {
                BuildStage::Serial
            }
        }
        // Already serialized: start delaying the next job (R17.3).
        BuildStage::Serial => BuildStage::Delayed(DELAY_STEP_MS),
        // Keep backing off until the delay budget is exhausted, then abort (R17.4).
        BuildStage::Delayed(ms) => {
            let next = ms + DELAY_STEP_MS;
            if next >= MAX_DELAY_MS {
                BuildStage::Aborted
            } else {
                BuildStage::Delayed(next)
            }
        }
        // Terminal.
        BuildStage::Aborted => BuildStage::Aborted,
    }
}

/// Move one rung *up* the ladder when pressure relents, recovering toward full
/// parallelism (R17.1). `Aborted` is terminal and never recovers.
fn step_up(stage: BuildStage, base: usize) -> BuildStage {
    match stage {
        BuildStage::Full => BuildStage::Full,
        BuildStage::Reduced(n) => {
            if n + 1 >= base {
                BuildStage::Full
            } else {
                BuildStage::Reduced(n + 1)
            }
        }
        BuildStage::Serial => {
            if base >= 3 {
                BuildStage::Reduced(2)
            } else {
                BuildStage::Full
            }
        }
        // Relief while delaying: stop backing off, return to serialized work.
        BuildStage::Delayed(_) => BuildStage::Serial,
        BuildStage::Aborted => BuildStage::Aborted,
    }
}

/// Decide the next ladder stage from the current one and a memory reading.
///
/// This is the **pure, snapshot-injectable** core of the degradation policy
/// (the unit tests for task 10.5 drive it with synthetic `available`/`reserve`
/// values, no real memory pressure required). The trigger follows R17.1: a build
/// is "under pressure" while available memory falls into the margin just above
/// the safety reserve (default ≤ 10% above `reserve`). A small hysteresis band
/// (recover only once available climbs back above 20% over `reserve`) keeps the
/// ladder from oscillating tick-to-tick.
///
/// - `available <= reserve + reserve/10` → step **down** (tighten, R17.1–17.4).
/// - `available >= reserve + reserve/5`  → step **up** (recover toward Normal).
/// - in between → hold the current stage.
///
/// Returns the new stage paired with the [`Degradation`] to report this tick.
pub fn next_degradation(
    stage: BuildStage,
    available: u64,
    reserve: u64,
    base_parallelism: usize,
) -> (BuildStage, Degradation) {
    let base = base_parallelism.max(1);
    let down_threshold = reserve + reserve / 10; // within 10% above reserve (R17.1)
    let up_threshold = reserve + reserve / 5; // 20% above reserve: recovery (hysteresis)

    let new_stage = if available <= down_threshold {
        step_down(stage, base)
    } else if available >= up_threshold {
        step_up(stage, base)
    } else {
        stage
    };

    (new_stage, new_stage.degradation())
}

// ---------------------------------------------------------------------------
// Build resource manager
// ---------------------------------------------------------------------------

/// Coordinates resource-aware building: OS detection, memory probing, budget
/// computation, and (via task 10.3) graceful degradation.
pub struct BuildResourceManager {
    host_os: HostOs,
    snapshot: MemorySnapshot,
    safety_reserve: u64,
    budget: u64,
    user_limit: Option<u64>,
    /// Full (no-pressure) parallelism, seeded from the logical CPU count; the
    /// ceiling the degradation ladder reduces from and recovers back to (R17.1).
    base_parallelism: usize,
    /// Current rung on the degradation ladder (R17); advanced by [`tick`](Self::tick).
    stage: BuildStage,
    /// Set once [`finish`](Self::finish) has released build-management state, so
    /// teardown is idempotent (R17.5).
    finished: bool,
    /// Non-fatal diagnostics gathered during [`init`](BuildResourceManager::init),
    /// to be surfaced by the `cmd_build` integration (task 10.4).
    warnings: Vec<Diagnostic>,
}

impl BuildResourceManager {
    /// Initialise the manager for a build (R14, R15, R16).
    ///
    /// Order matters: the OS is detected first (R14.1, before any probing), then
    /// memory is probed, then the budget is computed. Any of the following lead
    /// to the conservative default budget plus a warning, without aborting:
    /// unknown OS (`E0701`), failed/inconsistent probe (`E0702`), available
    /// memory ≤ safety reserve, or an invalid user limit (`E0703`).
    pub fn init(user_limit: Option<u64>) -> Self {
        // Step 1: detect the host OS *before* probing (R14.1).
        let host_os = HostOs::detect();

        // Step 2: probe memory (R15). The probe self-limits to 2000 ms.
        let snapshot = probe_memory(PROBE_TIMEOUT);

        let mut warnings = Vec::new();

        // Step 3: compute reserve + budget.
        let (safety_reserve, budget) = if !host_os.is_known() {
            // R14.2: unknown OS -> conservative budget (≤ 512 MiB) + E0701.
            warnings.push(Self::os_warning());
            let reserve = safety_reserve(snapshot.total);
            let budget = Self::clamp_conservative(snapshot.total, user_limit, &mut warnings);
            (reserve, budget)
        } else if !snapshot.ok {
            // R15.2/R15.3: probe failed/inconsistent -> conservative budget + E0702.
            warnings.push(Self::probe_warning(&snapshot));
            let reserve = safety_reserve(snapshot.total);
            let budget = Self::clamp_conservative(snapshot.total, user_limit, &mut warnings);
            (reserve, budget)
        } else {
            // R16: known OS + consistent metrics -> adaptive budget.
            let (reserve, budget, budget_warnings) =
                compute_budget(snapshot.total, snapshot.available, user_limit);
            for w in budget_warnings {
                warnings.push(Self::budget_warning(w, &snapshot, user_limit));
            }
            (reserve, budget)
        };

        BuildResourceManager {
            host_os,
            snapshot,
            safety_reserve,
            budget,
            user_limit,
            base_parallelism: cpu_count(),
            stage: BuildStage::Full,
            finished: false,
            warnings,
        }
    }

    /// The memory budget the build should stay within, in bytes (R16).
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// The detected host OS (R14).
    pub fn host_os(&self) -> HostOs {
        self.host_os
    }

    /// The safety reserve held back for the OS, in bytes (R16.2).
    pub fn safety_reserve(&self) -> u64 {
        self.safety_reserve
    }

    /// The memory snapshot the budget was based on (R15).
    pub fn snapshot(&self) -> &MemorySnapshot {
        &self.snapshot
    }

    /// The explicit user memory limit, if one was supplied (R16.4/16.6).
    pub fn user_limit(&self) -> Option<u64> {
        self.user_limit
    }

    /// Non-fatal diagnostics collected during [`init`](Self::init), for the
    /// `cmd_build` integration (task 10.4) to emit.
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    /// Monitor memory usage and decide how to degrade (R16.3, R17).
    ///
    /// Designed to be cheap enough to call on a ≤ 1 s cadence (R16.3): it takes a
    /// single memory reading (the probe self-caps well under a second on the
    /// common path) and delegates the actual decision to the pure
    /// [`next_degradation`] ladder. Under sustained pressure successive ticks walk
    /// down `Normal → ReduceParallelism → Serialize → Delay → Abort`; when memory
    /// frees up they walk back toward `Normal` (R17.1–17.4).
    ///
    /// If the manager has already aborted or been [`finish`](Self::finish)ed, or
    /// the probe cannot produce a reliable reading, the current stage is reported
    /// unchanged rather than escalating on bad data.
    pub fn tick(&mut self) -> Degradation {
        if self.finished || matches!(self.stage, BuildStage::Aborted) {
            return self.stage.degradation();
        }

        let snapshot = probe_memory(PROBE_TIMEOUT);
        if !snapshot.ok {
            // Can't trust the reading (R15.2/15.3): hold steady, don't escalate.
            return self.stage.degradation();
        }

        self.observe(snapshot.available)
    }

    /// Advance the degradation ladder for an observed `available`-memory reading
    /// and report the resulting [`Degradation`].
    ///
    /// Split out from [`tick`](Self::tick) so tests (and the build loop) can drive
    /// the ladder with injected readings without real memory pressure. The first
    /// transition into [`Degradation::Abort`] records the `E0704` error diagnostic
    /// exactly once (R17.4) so `cmd_build` (task 10.4) can surface it.
    pub fn observe(&mut self, available: u64) -> Degradation {
        let was_aborted = matches!(self.stage, BuildStage::Aborted);
        let (new_stage, degradation) =
            next_degradation(self.stage, available, self.safety_reserve, self.base_parallelism);
        self.stage = new_stage;

        if matches!(self.stage, BuildStage::Aborted) && !was_aborted {
            self.warnings.push(Self::abort_diagnostic());
        }

        degradation
    }

    /// The current rung of the degradation ladder (R17).
    pub fn stage(&self) -> BuildStage {
        self.stage
    }

    /// Whether the build has been aborted under sustained memory pressure (R17.4).
    pub fn is_aborted(&self) -> bool {
        matches!(self.stage, BuildStage::Aborted)
    }

    /// Release all resources held for build management (R17.5).
    ///
    /// The current design holds no OS handles or background threads dedicated to
    /// build management — degradation state is plain in-memory data and the memory
    /// probe spawns only short-lived, self-terminating workers. So teardown is a
    /// clean reset of the ladder state; there is nothing external to close. It is
    /// idempotent: calling it again after the first time is a no-op.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.stage = BuildStage::Full;
    }

    // -- internal diagnostic builders --------------------------------------

    /// Build the `E0704` **error** diagnostic emitted when the build aborts under
    /// sustained memory pressure after the delay budget is exhausted (R17.4).
    /// `from_code` prefills the catalog severity (Error) and fix hint.
    fn abort_diagnostic() -> Diagnostic {
        Diagnostic::from_code(
            "E0704",
            format!(
                "Build dihentikan karena kendala memori setelah batas waktu penundaan (\u{2264}{} detik) terlampaui; tanpa alokasi memori build yang tertunda.",
                MAX_DELAY.as_secs()
            ),
        )
    }

    /// Build the `E0701` warning for an unrecognised host OS (R14.2).
    fn os_warning() -> Diagnostic {
        Diagnostic::from_code(
            "E0701",
            format!(
                "OS host '{}' tak dikenali; memakai anggaran memori konservatif (\u{2264}512 MiB).",
                std::env::consts::OS
            ),
        )
    }

    /// Build the `E0702` warning for a failed/inconsistent memory probe
    /// (R15.2/R15.3).
    fn probe_warning(snapshot: &MemorySnapshot) -> Diagnostic {
        let detail = if snapshot.total == 0 {
            "total memori tidak terbaca".to_string()
        } else if snapshot.available > snapshot.total {
            format!(
                "metrik tak konsisten (available {} > total {})",
                snapshot.available, snapshot.total
            )
        } else {
            "probing gagal atau melebihi batas waktu".to_string()
        };
        Diagnostic::from_code(
            "E0702",
            format!(
                "probing memori gagal ({}); memakai anggaran memori konservatif.",
                detail
            ),
        )
    }

    /// Build the `E0703` warning for a budget adjustment (R16.5/16.6).
    fn budget_warning(
        warning: BudgetWarning,
        snapshot: &MemorySnapshot,
        user_limit: Option<u64>,
    ) -> Diagnostic {
        let msg = match warning {
            BudgetWarning::AvailableBelowReserve => format!(
                "memori tersedia ({} B) \u{2264} cadangan aman; memakai anggaran konservatif.",
                snapshot.available
            ),
            BudgetWarning::InvalidUserLimit => format!(
                "batas memori pengguna ({} B) tidak valid (\u{2264}0 atau >{} B total); memakai anggaran terhitung.",
                user_limit.unwrap_or(0),
                snapshot.total
            ),
        };
        Diagnostic::from_code(warning.code(), msg)
    }

    /// Conservative budget used when the host metrics cannot be trusted (unknown
    /// OS or failed probe): start from [`CONSERVATIVE_DEFAULT`] and clamp to a
    /// valid user limit (R16.4); invalid limits are ignored with an `E0703`
    /// warning (R16.6). `total == 0` means total is unknown, so the `> total`
    /// half of the validity check is skipped.
    fn clamp_conservative(
        total: u64,
        user_limit: Option<u64>,
        warnings: &mut Vec<Diagnostic>,
    ) -> u64 {
        let mut budget = CONSERVATIVE_DEFAULT;
        if let Some(limit) = user_limit {
            let exceeds_total = total != 0 && limit > total;
            if limit == 0 || exceeds_total {
                warnings.push(Diagnostic::from_code(
                    "E0703",
                    format!(
                        "batas memori pengguna ({} B) tidak valid; memakai anggaran konservatif.",
                        limit
                    ),
                ));
            } else {
                budget = budget.min(limit);
            }
        }
        budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::diagnostics::Severity;

    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    // -- OS detection (R14) ------------------------------------------------

    #[test]
    fn os_detection_maps_known_platforms() {
        assert_eq!(HostOs::from_os_str("windows"), HostOs::Windows);
        assert_eq!(HostOs::from_os_str("macos"), HostOs::MacOs);
        assert_eq!(HostOs::from_os_str("linux"), HostOs::Linux);
    }

    #[test]
    fn os_detection_unknown_for_other_platforms() {
        assert_eq!(HostOs::from_os_str("freebsd"), HostOs::Unknown);
        assert_eq!(HostOs::from_os_str(""), HostOs::Unknown);
        assert!(!HostOs::from_os_str("solaris").is_known());
        assert!(HostOs::from_os_str("linux").is_known());
    }

    #[test]
    fn detect_returns_a_variant_for_this_host() {
        // Whatever the build host is, detection must terminate with a variant.
        let _ = HostOs::detect();
    }

    // -- safety reserve (R16.2) -------------------------------------------

    #[test]
    fn safety_reserve_uses_floor_for_small_hosts() {
        // 10% of 1 GiB = ~102 MiB, below the 512 MiB floor.
        assert_eq!(safety_reserve(GIB), SAFETY_FLOOR);
        // total == 0 still yields the floor.
        assert_eq!(safety_reserve(0), SAFETY_FLOOR);
    }

    #[test]
    fn safety_reserve_uses_ten_percent_for_large_hosts() {
        // 10% of 16 GiB = 1.6 GiB, well above the floor.
        assert_eq!(safety_reserve(16 * GIB), (16 * GIB) / 10);
    }

    // -- budget computation, normal case (R16.1) --------------------------

    #[test]
    fn budget_normal_subtracts_reserve() {
        let total = 16 * GIB;
        let available = 8 * GIB;
        let (reserve, budget, warnings) = compute_budget(total, available, None);

        assert_eq!(reserve, total / 10); // 1.6 GiB
        assert_eq!(budget, available - reserve);
        assert!(warnings.is_empty());

        // Design invariants for the consistent, available>reserve case.
        assert!(budget <= available - reserve);
        assert!(available - budget >= reserve);
    }

    // -- available <= reserve -> CONSERVATIVE_DEFAULT (R16.5) --------------

    #[test]
    fn budget_conservative_when_available_at_or_below_reserve() {
        let total = 4 * GIB;
        let available = 256 * MIB; // below the 512 MiB floor reserve
        let (reserve, budget, warnings) = compute_budget(total, available, None);

        assert_eq!(reserve, SAFETY_FLOOR);
        assert_eq!(budget, CONSERVATIVE_DEFAULT);
        assert_eq!(warnings, vec![BudgetWarning::AvailableBelowReserve]);
    }

    #[test]
    fn budget_conservative_when_available_equals_reserve() {
        let total = 8 * GIB;
        let reserve = safety_reserve(total); // 800 MiB
        let (_r, budget, warnings) = compute_budget(total, reserve, None);
        // available == reserve is *not* strictly greater, so conservative.
        assert_eq!(budget, CONSERVATIVE_DEFAULT);
        assert_eq!(warnings, vec![BudgetWarning::AvailableBelowReserve]);
    }

    // -- invalid user limit ignored -> E0703 (R16.6) ----------------------

    #[test]
    fn invalid_user_limit_zero_is_ignored() {
        let total = 16 * GIB;
        let available = 8 * GIB;
        let (reserve, budget, warnings) = compute_budget(total, available, Some(0));
        assert_eq!(budget, available - reserve); // unchanged
        assert_eq!(warnings, vec![BudgetWarning::InvalidUserLimit]);
    }

    #[test]
    fn invalid_user_limit_exceeding_total_is_ignored() {
        let total = 16 * GIB;
        let available = 8 * GIB;
        let (reserve, budget, warnings) = compute_budget(total, available, Some(total + 1));
        assert_eq!(budget, available - reserve); // unchanged
        assert_eq!(warnings, vec![BudgetWarning::InvalidUserLimit]);
    }

    // -- valid user limit clamps budget (R16.4) ---------------------------

    #[test]
    fn valid_user_limit_clamps_budget() {
        let total = 16 * GIB;
        let available = 8 * GIB;
        let limit = 1 * GIB;
        let (_reserve, budget, warnings) = compute_budget(total, available, Some(limit));
        assert_eq!(budget, limit); // min(8GiB-1.6GiB, 1GiB) == 1GiB
        assert!(warnings.is_empty());
    }

    #[test]
    fn valid_user_limit_above_computed_does_not_raise_budget() {
        let total = 16 * GIB;
        let available = 8 * GIB;
        let computed = available - safety_reserve(total);
        // A limit larger than the computed budget (but <= total) leaves it alone.
        let (_r, budget, warnings) = compute_budget(total, available, Some(total));
        assert_eq!(budget, computed);
        assert!(warnings.is_empty());
    }

    // -- init smoke test ---------------------------------------------------

    #[test]
    fn init_produces_a_positive_budget() {
        let mgr = BuildResourceManager::init(None);
        assert!(mgr.budget() > 0, "budget should be positive");
        assert!(mgr.safety_reserve() >= SAFETY_FLOOR);
        // `tick()` reflects LIVE memory pressure, so its degradation level is not
        // deterministic here (under a heavily parallel test run the host may
        // genuinely be under pressure). Just exercise tick()/finish() for the
        // no-panic path; the degradation thresholds are covered by dedicated
        // tests with controlled snapshots.
        let mut mgr = mgr;
        let _ = mgr.tick();
        mgr.finish();
    }

    #[test]
    fn init_with_invalid_limit_records_e0703() {
        let mgr = BuildResourceManager::init(Some(0));
        // On any host an invalid limit of 0 must be flagged with E0703.
        assert!(
            mgr.warnings()
                .iter()
                .any(|d| d.code.as_deref() == Some("E0703")),
            "expected an E0703 warning for an invalid user limit"
        );
    }

    // -- degradation ladder (R17) -----------------------------------------

    /// Build a manager with deterministic reserve / parallelism for ladder tests,
    /// bypassing real probing. The snapshot is irrelevant here because the ladder
    /// is driven via [`observe`](BuildResourceManager::observe).
    fn ladder_manager(reserve: u64, base_parallelism: usize) -> BuildResourceManager {
        BuildResourceManager {
            host_os: HostOs::Linux,
            snapshot: MemorySnapshot {
                total: 0,
                used: 0,
                available: 0,
                ok: true,
            },
            safety_reserve: reserve,
            budget: 0,
            user_limit: None,
            base_parallelism,
            stage: BuildStage::Full,
            finished: false,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn ladder_steps_down_under_sustained_pressure() {
        // reserve 1000 -> down_threshold 1100; available 500 is sustained pressure.
        let reserve = 1000u64;
        let base = 4usize;
        let under_pressure = 500u64;

        let mut stage = BuildStage::Full;
        let mut seen = Vec::new();
        // Walk the ladder until it aborts (bounded so a bug can't loop forever).
        for _ in 0..32 {
            let (next, degr) = next_degradation(stage, under_pressure, reserve, base);
            seen.push(degr.clone());
            stage = next;
            if stage == BuildStage::Aborted {
                break;
            }
        }

        // Must pass strictly through each rung in order before aborting.
        assert_eq!(
            seen,
            vec![
                Degradation::ReduceParallelism(3),
                Degradation::ReduceParallelism(2),
                Degradation::Serialize,
                Degradation::Delay(Duration::from_secs(60)),
                Degradation::Delay(Duration::from_secs(120)),
                Degradation::Delay(Duration::from_secs(180)),
                Degradation::Delay(Duration::from_secs(240)),
                Degradation::Abort,
            ]
        );
    }

    #[test]
    fn ladder_delay_never_exceeds_300s() {
        let reserve = 1000u64;
        let base = 2usize;
        let mut stage = BuildStage::Full;
        for _ in 0..64 {
            let (next, degr) = next_degradation(stage, 0, reserve, base);
            if let Degradation::Delay(d) = degr {
                assert!(d <= MAX_DELAY, "delay {:?} exceeded the 300 s cap", d);
            }
            stage = next;
            if stage == BuildStage::Aborted {
                break;
            }
        }
        assert_eq!(stage, BuildStage::Aborted, "sustained pressure must abort");
    }

    #[test]
    fn ladder_single_core_serializes_then_delays_then_aborts() {
        // base_parallelism == 1: there is no Reduced rung, Full collapses to Serial.
        let reserve = 1000u64;
        let (s1, d1) = next_degradation(BuildStage::Full, 0, reserve, 1);
        assert_eq!(s1, BuildStage::Serial);
        assert_eq!(d1, Degradation::Serialize);
        let (s2, d2) = next_degradation(s1, 0, reserve, 1);
        assert_eq!(s2, BuildStage::Delayed(60_000));
        assert_eq!(d2, Degradation::Delay(Duration::from_secs(60)));
    }

    #[test]
    fn ladder_recovers_toward_normal_when_pressure_relents() {
        let reserve = 1000u64;
        let base = 4usize;
        let relief = 5000u64; // well above up_threshold (1200)

        // Recover from the bottom of the parallelism range back up to Full.
        let (s, d) = next_degradation(BuildStage::Serial, relief, reserve, base);
        assert_eq!(s, BuildStage::Reduced(2));
        assert_eq!(d, Degradation::ReduceParallelism(2));

        let (s, _) = next_degradation(s, relief, reserve, base);
        assert_eq!(s, BuildStage::Reduced(3));

        let (s, d) = next_degradation(s, relief, reserve, base);
        assert_eq!(s, BuildStage::Full);
        assert_eq!(d, Degradation::Normal);

        // Delaying recovers to serialized work first.
        let (s, d) = next_degradation(BuildStage::Delayed(120_000), relief, reserve, base);
        assert_eq!(s, BuildStage::Serial);
        assert_eq!(d, Degradation::Serialize);
    }

    #[test]
    fn ladder_holds_in_hysteresis_band() {
        // reserve 1000: down_threshold 1100, up_threshold 1200. 1150 is in between.
        let reserve = 1000u64;
        let (s, d) = next_degradation(BuildStage::Reduced(2), 1150, reserve, 4);
        assert_eq!(s, BuildStage::Reduced(2), "stage must hold in the dead band");
        assert_eq!(d, Degradation::ReduceParallelism(2));
    }

    #[test]
    fn abort_is_terminal_and_does_not_recover() {
        let reserve = 1000u64;
        let (s, d) = next_degradation(BuildStage::Aborted, 99_999, reserve, 8);
        assert_eq!(s, BuildStage::Aborted, "an aborted build never recovers");
        assert_eq!(d, Degradation::Abort);
    }

    #[test]
    fn observe_records_e0704_once_on_abort() {
        let mut mgr = ladder_manager(1000, 2);
        // Drive sustained pressure until it aborts.
        let mut aborted = false;
        for _ in 0..64 {
            if mgr.observe(0) == Degradation::Abort {
                aborted = true;
                break;
            }
        }
        assert!(aborted, "sustained pressure should abort the build");
        assert!(mgr.is_aborted());

        // Exactly one E0704 error diagnostic, even after further ticks.
        mgr.observe(0);
        mgr.observe(0);
        let e0704: Vec<_> = mgr
            .warnings()
            .iter()
            .filter(|d| d.code.as_deref() == Some("E0704"))
            .collect();
        assert_eq!(e0704.len(), 1, "E0704 must be recorded exactly once");
    }

    #[test]
    fn finish_is_idempotent() {
        let mut mgr = ladder_manager(1000, 4);
        // Push it partway down the ladder.
        mgr.observe(0);
        assert_ne!(mgr.stage(), BuildStage::Full);

        mgr.finish();
        assert_eq!(mgr.stage(), BuildStage::Full, "finish resets ladder state");
        // Calling finish repeatedly must not panic or change anything further.
        mgr.finish();
        mgr.finish();
        assert_eq!(mgr.stage(), BuildStage::Full);
    }

    #[test]
    fn tick_after_finish_reports_normal_without_escalating() {
        let mut mgr = ladder_manager(1000, 4);
        mgr.finish();
        // Even though tick would probe, a finished manager just reports its stage.
        assert_eq!(mgr.tick(), Degradation::Normal);
    }

    // -- unknown OS -> E0701 (R14.2) --------------------------------------

    #[test]
    fn unknown_os_emits_e0701_warning() {
        // The build host can't be forced to an unrecognised OS, so verify the two
        // halves of init's unknown-OS branch: the classification predicate that
        // selects it, and the diagnostic builder that records E0701.
        assert!(
            !HostOs::from_os_str("freebsd").is_known(),
            "an unrecognised OS must take the conservative/E0701 path"
        );

        let d = BuildResourceManager::os_warning();
        assert_eq!(d.code.as_deref(), Some("E0701"));
        assert_eq!(
            d.severity,
            Severity::Warning,
            "unknown OS is non-fatal: build continues conservatively"
        );
    }

    #[test]
    fn unknown_os_budget_is_conservative_and_respects_valid_limit() {
        // Models the budget side of init's unknown-OS branch (clamp_conservative).
        let total = 16 * GIB;
        let mut warnings = Vec::new();
        // No user limit: budget is exactly the conservative default (<= 512 MiB).
        let budget = BuildResourceManager::clamp_conservative(total, None, &mut warnings);
        assert_eq!(budget, CONSERVATIVE_DEFAULT);
        assert!(budget <= 512 * MIB, "conservative budget must be <= 512 MiB");
        assert!(warnings.is_empty());

        // A valid, smaller user limit clamps the conservative budget further.
        let mut warnings = Vec::new();
        let budget = BuildResourceManager::clamp_conservative(total, Some(128 * MIB), &mut warnings);
        assert_eq!(budget, 128 * MIB);
        assert!(warnings.is_empty());
    }

    #[test]
    fn unknown_os_invalid_limit_records_e0703() {
        // An invalid user limit on the conservative path still flags E0703 (R16.6).
        let mut warnings = Vec::new();
        let budget = BuildResourceManager::clamp_conservative(8 * GIB, Some(0), &mut warnings);
        assert_eq!(budget, CONSERVATIVE_DEFAULT, "invalid limit is ignored");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code.as_deref(), Some("E0703"));
    }

    // -- probe failed / inconsistent -> E0702 (R15.2/R15.3) ----------------

    #[test]
    fn probe_failure_emits_e0702_warning() {
        // A probe that simply failed/timed out (ok == false, total unreadable).
        let failed = MemorySnapshot {
            total: 0,
            used: 0,
            available: 0,
            ok: false,
        };
        let d = BuildResourceManager::probe_warning(&failed);
        assert_eq!(d.code.as_deref(), Some("E0702"));
        assert_eq!(d.severity, Severity::Warning);
    }

    #[test]
    fn probe_inconsistent_metrics_emit_e0702_warning() {
        // Inconsistent metrics: available > total must be reported as E0702 and
        // the message should call out the inconsistency (R15.3).
        let inconsistent = MemorySnapshot {
            total: 4 * GIB,
            used: 0,
            available: 8 * GIB, // available > total is impossible -> inconsistent
            ok: false,
        };
        let d = BuildResourceManager::probe_warning(&inconsistent);
        assert_eq!(d.code.as_deref(), Some("E0702"));
        assert_eq!(d.severity, Severity::Warning);
        assert!(
            d.message.contains("konsisten"),
            "inconsistent-metrics probe should explain the inconsistency, got: {}",
            d.message
        );
    }

    // -- available <= reserve -> E0703 diagnostic emission (R16.5) ---------

    #[test]
    fn available_below_reserve_budget_warning_is_e0703() {
        // compute_budget's AvailableBelowReserve cause must render as an E0703
        // warning diagnostic on init's adaptive path.
        let snapshot = MemorySnapshot {
            total: 4 * GIB,
            used: 0,
            available: 256 * MIB,
            ok: true,
        };
        let d = BuildResourceManager::budget_warning(
            BudgetWarning::AvailableBelowReserve,
            &snapshot,
            None,
        );
        assert_eq!(d.code.as_deref(), Some("E0703"));
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(BudgetWarning::AvailableBelowReserve.code(), "E0703");
    }

    // -- full degradation ladder via observe()/injected readings (R17) -----

    #[test]
    fn observe_walks_full_ladder_parallelism_serialize_delay_abort() {
        // Drive observe() with a sustained over-budget reading and assert it walks
        // the complete ladder in order: parallelism down -> serialize -> delay ->
        // abort. reserve 1000 -> down_threshold 1100; available 500 is pressure.
        let mut mgr = ladder_manager(1000, 4);
        let mut seen = Vec::new();
        for _ in 0..32 {
            let degr = mgr.observe(500);
            seen.push(degr.clone());
            if degr == Degradation::Abort {
                break;
            }
        }

        assert_eq!(
            seen,
            vec![
                Degradation::ReduceParallelism(3),
                Degradation::ReduceParallelism(2),
                Degradation::Serialize,
                Degradation::Delay(Duration::from_secs(60)),
                Degradation::Delay(Duration::from_secs(120)),
                Degradation::Delay(Duration::from_secs(180)),
                Degradation::Delay(Duration::from_secs(240)),
                Degradation::Abort,
            ]
        );
        assert!(mgr.is_aborted());
        // The abort transition must have recorded exactly one E0704 error (R17.4).
        let e0704 = mgr
            .warnings()
            .iter()
            .filter(|d| d.code.as_deref() == Some("E0704"))
            .count();
        assert_eq!(e0704, 1);
    }
}

// ---------------------------------------------------------------------------
// Property test P11 — memory-budget invariants with Safety_Reserve (task 10.6)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::support::pbt::{self, Gen};

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A consistent memory snapshot (`available <= total`, both > 0) paired with
    /// an optional user limit that may be valid, absent, or invalid (`0` / `> total`).
    /// This is the exact input space of the pure [`compute_budget`].
    #[derive(Clone, Debug)]
    struct Case {
        total: u64,
        available: u64,
        user_limit: Option<u64>,
    }

    /// Generate a *consistent* snapshot plus an optional `user_limit`.
    ///
    /// `total` spans small hosts (< 1 GiB, where the reserve hits the 512 MB
    /// floor) and large hosts (up to 256 GiB, where the reserve is 10% of total);
    /// `available` is uniform in `0..=total` so both budget branches
    /// (`available > reserve` and `available <= reserve`) are exercised;
    /// `user_limit` is drawn from {None, valid `1..=total`, invalid `0`,
    /// invalid `> total`} so the clamp and the two ignore paths are all hit.
    fn case_gen() -> Gen<Case> {
        Gen::new(
            |rng, _size| {
                let total = match rng.below(3) {
                    0 => 1 + rng.below(GIB),        // small host: reserve = 512 MB floor
                    1 => GIB + rng.below(64 * GIB), // mid/large host: reserve = 10%
                    _ => 1 + rng.below(256 * GIB),  // wide range
                };
                let available = rng.below(total + 1); // 0..=total (consistent)
                let user_limit = match rng.below(4) {
                    0 => None,
                    1 => Some(1 + rng.below(total)),          // valid: 1..=total
                    2 => Some(0),                             // invalid: zero
                    _ => Some(total + 1 + rng.below(GIB)),    // invalid: > total
                };
                Case { total, available, user_limit }
            },
            |c: &Case| {
                // Shrink while preserving consistency (available <= total, total >= 1).
                let mut out = Vec::new();
                if c.user_limit.is_some() {
                    out.push(Case { user_limit: None, ..*c });
                }
                if c.available > 0 {
                    out.push(Case { available: c.available / 2, ..*c });
                }
                let smaller_total = (c.total / 2).max(c.available).max(1);
                if smaller_total < c.total {
                    out.push(Case { total: smaller_total, ..*c });
                }
                out
            },
        )
    }

    // Feature: enterprise-runtime-capabilities, Property 11: Invariant anggaran memori dengan Safety_Reserve terjaga
    /// Property 11: the memory-budget invariants hold for every consistent
    /// snapshot and any (in)valid user limit, driving the pure [`compute_budget`].
    ///
    /// Validates: Requirements 17.1, 17.2, 17.4, 17.5, 17.6
    ///
    /// Invariants checked per case:
    /// 1. `reserve == max(total/10, 512 MB)`.
    /// 2. when `available > reserve`: `budget <= available` and the reserve is
    ///    preserved (`available - budget >= reserve`), with no
    ///    `AvailableBelowReserve` warning on the unconstrained computation.
    /// 3. when `available <= reserve`: the unconstrained budget is exactly
    ///    `CONSERVATIVE_DEFAULT` and `AvailableBelowReserve` is warned.
    /// 4. a valid `user_limit` clamps (`budget == min(no_limit_budget, limit)`,
    ///    never raising it) with no `InvalidUserLimit` warning; an invalid limit
    ///    is ignored (`budget == no_limit_budget`) and yields `InvalidUserLimit`.
    #[test]
    fn prop_memory_budget_invariants() {
        pbt::for_all("P11 budget invariants", &case_gen(), |c: &Case| {
            let Case { total, available, user_limit } = *c;

            let (reserve, budget, warnings) = compute_budget(total, available, user_limit);
            // Reference computation with no user limit, to isolate the clamp effect.
            let (no_limit_reserve, no_limit_budget, no_limit_warnings) =
                compute_budget(total, available, None);

            let has = |ws: &[BudgetWarning], w: BudgetWarning| ws.contains(&w);

            // Invariant 1: reserve is the defined max(10% total, 512 MB), and the
            // user limit never changes the reserve.
            if reserve != safety_reserve(total) {
                return false;
            }
            if no_limit_reserve != reserve {
                return false;
            }

            // Invariants 2 & 3: branch on available vs reserve.
            if available > reserve {
                // Adaptive branch: budget stays within available and preserves reserve.
                if budget > available {
                    return false;
                }
                if available - budget < reserve {
                    return false;
                }
                // Unconstrained budget is exactly available - reserve, no scarcity warning.
                if no_limit_budget != available - reserve {
                    return false;
                }
                if has(&no_limit_warnings, BudgetWarning::AvailableBelowReserve) {
                    return false;
                }
            } else {
                // Conservative branch: fall back to the fixed default + warn.
                if no_limit_budget != CONSERVATIVE_DEFAULT {
                    return false;
                }
                if !has(&no_limit_warnings, BudgetWarning::AvailableBelowReserve) {
                    return false;
                }
            }

            // Invariant 4: user-limit handling.
            match user_limit {
                None => {
                    // No limit: identical to the reference computation.
                    if budget != no_limit_budget || warnings != no_limit_warnings {
                        return false;
                    }
                }
                Some(limit) if limit != 0 && limit <= total => {
                    // Valid limit clamps down, never up; no invalid-limit warning.
                    if budget != no_limit_budget.min(limit) {
                        return false;
                    }
                    if budget > limit {
                        return false;
                    }
                    if has(&warnings, BudgetWarning::InvalidUserLimit) {
                        return false;
                    }
                }
                Some(_) => {
                    // Invalid limit (0 or > total): ignored, budget unchanged, warned.
                    if budget != no_limit_budget {
                        return false;
                    }
                    if !has(&warnings, BudgetWarning::InvalidUserLimit) {
                        return false;
                    }
                }
            }

            true
        });
    }
}

// ---------------------------------------------------------------------------
// Property test P9 — build memory-budget invariants (memory-safe-self-hosting)
// ---------------------------------------------------------------------------
//
// This mirrors the budget-invariant property for the `memory-safe-self-hosting`
// feature (task 8.4, design Property 9). It is intentionally self-contained — a
// fresh generator over randomized (total, available) snapshots and (in)valid
// user limits — so it documents and pins the R5.1/R5.3 contract of
// `compute_budget` independently of the older `enterprise-runtime-capabilities`
// P11 module above (which is left untouched).

#[cfg(test)]
mod prop_tests_p9 {
    use super::*;
    use crate::support::pbt::{self, Gen};

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    /// A consistent memory snapshot (`available <= total`, `total >= 1`) plus an
    /// optional user limit which may be absent, valid (`1..=total`), or invalid
    /// (`0` or `> total`). This is exactly the input domain of [`compute_budget`].
    #[derive(Clone, Debug)]
    struct Snapshot {
        total: u64,
        available: u64,
        user_limit: Option<u64>,
    }

    /// Randomized snapshot + limit generator covering both budget branches and
    /// every user-limit class.
    ///
    /// `total` is drawn across small hosts (where the reserve hits the 512 MB
    /// floor) and large hosts (where the reserve is 10% of total); `available`
    /// is uniform in `0..=total` so both `available > reserve` and
    /// `available <= reserve` occur; `user_limit` is one of
    /// {None, valid, invalid-zero, invalid-over-total} so the clamp and both
    /// ignore paths are exercised.
    fn snapshot_gen() -> Gen<Snapshot> {
        Gen::new(
            |rng, _size| {
                let total = match rng.below(3) {
                    0 => 1 + rng.below(GIB),         // small host: reserve = 512 MB floor
                    1 => GIB + rng.below(64 * GIB),  // large host: reserve = 10% of total
                    _ => 1 + rng.below(256 * GIB),   // wide range
                };
                let available = rng.below(total + 1); // 0..=total (consistent)
                let user_limit = match rng.below(4) {
                    0 => None,
                    1 => Some(1 + rng.below(total)),       // valid: 1..=total
                    2 => Some(0),                          // invalid: zero
                    _ => Some(total + 1 + rng.below(GIB)), // invalid: > total
                };
                Snapshot { total, available, user_limit }
            },
            |s: &Snapshot| {
                // Shrink while preserving consistency (available <= total, total >= 1).
                let mut out = Vec::new();
                if s.user_limit.is_some() {
                    out.push(Snapshot { user_limit: None, ..*s });
                }
                if s.available > 0 {
                    out.push(Snapshot { available: s.available / 2, ..*s });
                }
                let smaller_total = (s.total / 2).max(s.available).max(1);
                if smaller_total < s.total {
                    out.push(Snapshot { total: smaller_total, ..*s });
                }
                out
            },
        )
    }

    // Feature: memory-safe-self-hosting, Property 9: Build memory budget invariants hold
    /// Property 9: for every consistent `(total, available)` snapshot and any
    /// user limit (valid, absent, or invalid), [`compute_budget`] maintains the
    /// safety-reserve budgeting contract.
    ///
    /// Validates: Requirements 5.1, 5.3
    ///
    /// Invariants checked per case:
    /// 1. `reserve == max(total/10, 512 MB)`, and a user limit never changes it.
    /// 2. when `available > reserve`: the adaptive budget never exceeds available
    ///    and preserves the reserve (`budget <= available - reserve`), with no
    ///    scarcity warning on the unconstrained computation.
    /// 3. when `available <= reserve`: the unconstrained budget is exactly
    ///    `CONSERVATIVE_DEFAULT` and `AvailableBelowReserve` is reported.
    /// 4. a valid `user_limit` clamps the budget down (`budget <= limit`, never
    ///    raising it) with no invalid-limit warning; an invalid limit (0 or
    ///    `> total`) is ignored (budget unchanged) and is reported as `E0703`
    ///    via `BudgetWarning::InvalidUserLimit`.
    #[test]
    fn prop_build_memory_budget_invariants() {
        pbt::for_all("P9 build budget invariants", &snapshot_gen(), |s: &Snapshot| {
            let Snapshot { total, available, user_limit } = *s;

            let (reserve, budget, warnings) = compute_budget(total, available, user_limit);
            // Reference budget with no user limit, to isolate the clamp effect.
            let (no_limit_reserve, no_limit_budget, no_limit_warnings) =
                compute_budget(total, available, None);

            let has = |ws: &[BudgetWarning], w: BudgetWarning| ws.contains(&w);

            // Invariant 1: reserve = max(total/10, 512 MB); limit never alters it.
            if reserve != safety_reserve(total) {
                return false;
            }
            if reserve != (total / 10).max(512 * MIB) {
                return false;
            }
            if no_limit_reserve != reserve {
                return false;
            }

            // Invariants 2 & 3: branch on available vs reserve.
            if available > reserve {
                // Adaptive: budget within available, reserve preserved, no scarcity warning.
                if no_limit_budget > available {
                    return false;
                }
                if no_limit_budget > available - reserve {
                    return false;
                }
                if no_limit_budget != available - reserve {
                    return false;
                }
                if has(&no_limit_warnings, BudgetWarning::AvailableBelowReserve) {
                    return false;
                }
            } else {
                // Conservative: fixed default budget + scarcity warning.
                if no_limit_budget != CONSERVATIVE_DEFAULT {
                    return false;
                }
                if !has(&no_limit_warnings, BudgetWarning::AvailableBelowReserve) {
                    return false;
                }
            }

            // Invariant 4: user-limit handling.
            match user_limit {
                None => {
                    if budget != no_limit_budget || warnings != no_limit_warnings {
                        return false;
                    }
                }
                Some(limit) if limit != 0 && limit <= total => {
                    // Valid limit clamps down, never up; no invalid-limit warning.
                    if budget != no_limit_budget.min(limit) {
                        return false;
                    }
                    if budget > limit {
                        return false;
                    }
                    if has(&warnings, BudgetWarning::InvalidUserLimit) {
                        return false;
                    }
                }
                Some(_) => {
                    // Invalid limit (0 or > total): ignored, budget unchanged, E0703 reported.
                    if budget != no_limit_budget {
                        return false;
                    }
                    if !has(&warnings, BudgetWarning::InvalidUserLimit) {
                        return false;
                    }
                    // The reported warning must carry the E0703 diagnostic code (R5.3).
                    if BudgetWarning::InvalidUserLimit.code() != "E0703" {
                        return false;
                    }
                }
            }

            true
        });
    }
}
