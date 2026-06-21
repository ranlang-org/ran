//! System resource probing: OS, CPU count, and memory.
//!
//! Used so the toolchain can report the host and so the runtime can size itself
//! to the machine while always leaving a safety margin for the OS. Linux reads
//! `/proc/meminfo`; other platforms fall back to best-effort values.

/// OS name, e.g. "linux", "macos", "windows".
pub fn os_name() -> &'static str {
    std::env::consts::OS
}

/// CPU architecture, e.g. "x86_64", "aarch64".
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// Logical CPU count (>= 1).
pub fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Total physical RAM in bytes (0 if unknown).
pub fn mem_total() -> u64 {
    meminfo_field("MemTotal").unwrap_or(0)
}

/// Available RAM in bytes — memory that can be allocated without swapping
/// (0 if unknown). Prefer this over "free" for budgeting.
pub fn mem_available() -> u64 {
    meminfo_field("MemAvailable")
        .or_else(|| meminfo_field("MemFree"))
        .unwrap_or(0)
}

/// Read a `/proc/meminfo` field (value is in kB there) and return bytes.
#[cfg(target_os = "linux")]
fn meminfo_field(name: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            // e.g. "MemTotal:       16310456 kB"
            let kb: u64 = rest
                .trim_start_matches(':')
                .split_whitespace()
                .next()?
                .parse()
                .ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn meminfo_field(_name: &str) -> Option<u64> {
    None // best-effort: other platforms report 0 until a native probe is added
}

/// The number of bytes to keep free for the OS so the machine does not thrash
/// or OOM-kill us. Default: max(512 MiB, 12% of total). Override with the
/// `RAN_MEM_RESERVE_MB` environment variable.
pub fn os_reserve_bytes() -> u64 {
    if let Ok(v) = std::env::var("RAN_MEM_RESERVE_MB") {
        if let Ok(mb) = v.parse::<u64>() {
            return mb * 1024 * 1024;
        }
    }
    let total = mem_total();
    let pct = total / 8; // ~12.5%
    let floor = 512 * 1024 * 1024;
    pct.max(floor)
}

/// A memory budget the process should try to stay within: available memory
/// minus the OS reserve (0 if memory is unknown).
pub fn memory_budget_bytes() -> u64 {
    let avail = mem_available();
    if avail == 0 {
        return 0; // unknown -> no enforced budget
    }
    avail.saturating_sub(os_reserve_bytes())
}

/// Human-readable byte size (e.g. "7.0 GiB").
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if n == 0 {
        return "unknown".to_string();
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{:.1} {}", v, UNITS[u])
}

// ---------------------------------------------------------------------------
// Per-platform memory probing (Kelompok D — Build_Resource_Manager, R15)
// ---------------------------------------------------------------------------

use std::time::Duration;

/// Hard upper bound on how long a memory probe may take (R15.1). Even if the
/// caller passes a larger timeout we never wait longer than this.
const PROBE_CAP: Duration = Duration::from_millis(2000);

/// A point-in-time view of host memory, in bytes.
///
/// `ok == false` means the probe failed, timed out, or returned inconsistent
/// metrics (e.g. `available > total`, or `total == 0`). In that case the caller
/// (the build resource manager, task 10.2/10.3) should emit the **`E0702`**
/// warning diagnostic and fall back to a conservative memory budget rather than
/// trusting these numbers. When `ok == true` the invariant `available <= total`
/// holds and `used == total - available`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Total physical RAM in bytes.
    pub total: u64,
    /// Memory currently in use (`total - available`) in bytes.
    pub used: u64,
    /// Memory that can be allocated without swapping, in bytes.
    pub available: u64,
    /// Whether the probe succeeded and produced consistent metrics.
    pub ok: bool,
}

impl MemorySnapshot {
    /// A snapshot representing a failed/timed-out probe: all zero, `ok = false`.
    fn failed() -> Self {
        MemorySnapshot {
            total: 0,
            used: 0,
            available: 0,
            ok: false,
        }
    }
}

/// Probe host memory, never blocking longer than `min(timeout, 2000 ms)` (R15.1).
///
/// The actual platform probe runs on a worker thread; we wait on it with
/// `recv_timeout`. If the probe does not answer in time (or the worker dies),
/// we return a `failed()` snapshot (`ok = false`) so the caller can degrade to a
/// conservative budget and emit `E0702` (R15.2). Inconsistent metrics
/// (`available > total`, or `total == 0`) likewise yield `ok = false` (R15.3).
/// On success, `available <= total` and `used == total - available` (R15.4).
pub fn probe_memory(timeout: Duration) -> MemorySnapshot {
    // Enforce the 2000 ms cap regardless of what the caller requested.
    let effective = if timeout > PROBE_CAP { PROBE_CAP } else { timeout };

    let (tx, rx) = std::sync::mpsc::channel::<Option<(u64, u64)>>();
    // Detached worker: if it overruns the deadline we simply stop waiting for it.
    std::thread::spawn(move || {
        let _ = tx.send(raw_probe());
    });

    match rx.recv_timeout(effective) {
        Ok(Some((total, available))) => build_snapshot(total, available),
        // Probe returned but a metric was unreadable -> treat as failed (R15.2).
        Ok(None) => MemorySnapshot::failed(),
        // Timed out (>2000 ms or the requested cap) or worker disconnected (R15.2).
        Err(_) => MemorySnapshot::failed(),
    }
}

/// Validate raw `(total, available)` metrics for consistency (R15.3).
///
/// Pure helper, factored out for testing. Returns `true` only when the numbers
/// make physical sense: total memory is known (> 0) and available memory does
/// not exceed total. (Values are unsigned, so the "negative metric" case from
/// R15.3 cannot be represented and is implicitly rejected at the FFI boundary.)
fn metrics_consistent(total: u64, available: u64) -> bool {
    total != 0 && available <= total
}

/// Turn raw `(total, available)` metrics into a validated [`MemorySnapshot`].
fn build_snapshot(total: u64, available: u64) -> MemorySnapshot {
    if !metrics_consistent(total, available) {
        // Keep the observed numbers for diagnostics but mark the probe failed.
        return MemorySnapshot {
            total,
            used: 0,
            available,
            ok: false,
        };
    }
    MemorySnapshot {
        total,
        used: total - available,
        available,
        ok: true,
    }
}

/// Raw, platform-specific probe returning `(total_bytes, available_bytes)`.
/// `None` means a metric could not be read. All implementations are std-only
/// (Linux reads `/proc/meminfo`; macOS/Windows use C-ABI FFI to system libs).

#[cfg(target_os = "linux")]
fn raw_probe() -> Option<(u64, u64)> {
    let total = meminfo_field("MemTotal")?;
    let available = meminfo_field("MemAvailable").or_else(|| meminfo_field("MemFree"))?;
    Some((total, available))
}

#[cfg(target_os = "macos")]
fn raw_probe() -> Option<(u64, u64)> {
    use std::os::raw::{c_char, c_int, c_void};

    // Linked automatically on macOS (libSystem). C-ABI, std-only.
    extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
        fn mach_host_self() -> u32;
        fn host_page_size(host: u32, out_page_size: *mut usize) -> c_int;
        fn host_statistics64(
            host: u32,
            flavor: c_int,
            host_info_out: *mut c_void,
            host_info_count: *mut u32,
        ) -> c_int;
    }

    // vm_statistics64 layout (mach/vm_statistics.h). natural_t == u32.
    #[repr(C)]
    #[derive(Default)]
    struct VmStatistics64 {
        free_count: u32,
        active_count: u32,
        inactive_count: u32,
        wire_count: u32,
        zero_fill_count: u64,
        reactivations: u64,
        pageins: u64,
        pageouts: u64,
        faults: u64,
        cow_faults: u64,
        lookups: u64,
        hits: u64,
        purges: u64,
        purgeable_count: u32,
        speculative_count: u32,
        decompressions: u64,
        compressions: u64,
        swapins: u64,
        swapouts: u64,
        compressor_page_count: u32,
        throttled_count: u32,
        external_page_count: u32,
        internal_page_count: u32,
        total_uncompressed_pages_in_compressor: u64,
    }

    const HOST_VM_INFO64: c_int = 4;

    // Total physical memory via sysctl("hw.memsize").
    let mut total: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let name = b"hw.memsize\0";
    let rc = unsafe {
        sysctlbyname(
            name.as_ptr() as *const c_char,
            &mut total as *mut u64 as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || total == 0 {
        return None;
    }

    // Available memory ~= (free + inactive) pages * page_size — a documented,
    // conservative heuristic. If it ends up inconsistent the caller marks ok=false.
    let host = unsafe { mach_host_self() };
    let mut page_size: usize = 0;
    if unsafe { host_page_size(host, &mut page_size) } != 0 || page_size == 0 {
        return None;
    }

    let mut stats = VmStatistics64::default();
    // count is in units of integer_t (4 bytes).
    let mut count = (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<u32>()) as u32;
    let rc = unsafe {
        host_statistics64(
            host,
            HOST_VM_INFO64,
            &mut stats as *mut VmStatistics64 as *mut c_void,
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }

    let free_pages = stats.free_count as u64 + stats.inactive_count as u64;
    let available = free_pages.saturating_mul(page_size as u64);
    Some((total, available))
}

#[cfg(target_os = "windows")]
fn raw_probe() -> Option<(u64, u64)> {
    // MEMORYSTATUSEX (sysinfoapi.h), repr(C). Filled by GlobalMemoryStatusEx.
    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }

    // kernel32 is linked automatically on Windows. "system" == the WinAPI
    // calling convention (stdcall on x86, plain on x86_64).
    extern "system" {
        fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
    }

    let mut status: MemoryStatusEx = unsafe { std::mem::zeroed() };
    status.dw_length = std::mem::size_of::<MemoryStatusEx>() as u32;
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 {
        return None;
    }
    Some((status.ull_total_phys, status.ull_avail_phys))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn raw_probe() -> Option<(u64, u64)> {
    None // unsupported platform: caller falls back to a conservative budget (E0702)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_at_least_one() {
        assert!(cpu_count() >= 1);
    }

    #[test]
    fn human_readable() {
        assert_eq!(human_bytes(0), "unknown");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn reserve_has_a_floor() {
        // With RAN_MEM_RESERVE_MB unset, reserve is at least 512 MiB.
        std::env::remove_var("RAN_MEM_RESERVE_MB");
        assert!(os_reserve_bytes() >= 512 * 1024 * 1024);
    }

    #[test]
    fn metrics_consistent_rejects_nonsense() {
        // total == 0 is unusable.
        assert!(!metrics_consistent(0, 0));
        // available must not exceed total (R15.3).
        assert!(!metrics_consistent(1000, 1001));
        // a sane reading.
        assert!(metrics_consistent(1000, 400));
        // available may equal total.
        assert!(metrics_consistent(1000, 1000));
    }

    #[test]
    fn build_snapshot_marks_inconsistent_metrics_not_ok() {
        // available > total -> ok = false (R15.3); observed numbers preserved.
        let snap = build_snapshot(1000, 2000);
        assert!(!snap.ok);
        assert_eq!(snap.total, 1000);
        assert_eq!(snap.available, 2000);

        // total == 0 -> ok = false.
        assert!(!build_snapshot(0, 0).ok);
    }

    #[test]
    fn build_snapshot_computes_used_when_consistent() {
        let snap = build_snapshot(1000, 400);
        assert!(snap.ok);
        assert_eq!(snap.total, 1000);
        assert_eq!(snap.available, 400);
        assert_eq!(snap.used, 600); // used == total - available (R15.4)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_memory_succeeds_on_linux() {
        let snap = probe_memory(Duration::from_millis(2000));
        // On a Linux host /proc/meminfo is present, so the probe should succeed.
        assert!(snap.ok, "expected a successful probe on Linux");
        assert!(snap.total > 0, "total memory should be positive");
        assert!(
            snap.available <= snap.total,
            "available ({}) must not exceed total ({})",
            snap.available,
            snap.total
        );
        assert_eq!(snap.used, snap.total - snap.available);
    }

    #[test]
    fn probe_memory_honors_the_2000ms_cap() {
        // A larger requested timeout must still return promptly (cap = 2000 ms).
        let start = std::time::Instant::now();
        let _ = probe_memory(Duration::from_secs(60));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "probe_memory ignored the 2000 ms cap"
        );
    }
}
