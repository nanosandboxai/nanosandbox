//! Host capacity detection and admission control.
//!
//! One implementation shared by the CLI, the TUI, the supervisor spawn path,
//! and the headless `nanosb capacity` command. Admission is enabled by default
//! with budgets auto-derived from the host; every budget can be overridden via
//! environment, and enforcement can be disabled with `NANOSB_ADMISSION=off`.

use crate::supervisor::sandboxes_root;

/// Detected host resources. Every field is best-effort.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostCapacity {
    pub cpu_count: usize,
    pub memory_total_bytes: Option<u64>,
    pub memory_available_bytes: Option<u64>,
    pub disk_free_bytes: Option<u64>,
}

impl HostCapacity {
    pub fn detect() -> Self {
        Self {
            cpu_count: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0),
            memory_total_bytes: total_memory_bytes(),
            memory_available_bytes: available_memory_bytes(),
            disk_free_bytes: disk_free_bytes(),
        }
    }
}

/// Admission budgets. `None` means "not enforced for this dimension".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostBudget {
    pub enabled: bool,
    pub max_vms: Option<usize>,
    pub memory_mb: Option<u64>,
    pub cpus: Option<u32>,
    pub disk_min_free_mb: Option<u64>,
    pub sandbox_disk_mb: Option<u64>,
}

impl HostBudget {
    /// Read `NANOSB_MAX_VMS` / `NANOSB_MEMORY_BUDGET_MB` / `NANOSB_CPU_BUDGET` /
    /// `NANOSB_DISK_BUDGET_MB`, falling back to auto-derived budgets. Set
    /// `NANOSB_ADMISSION=off` to disable enforcement entirely.
    pub fn resolve() -> Self {
        let enabled = !matches!(
            std::env::var("NANOSB_ADMISSION")
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str(),
            "off" | "0" | "false" | "no"
        );
        let auto = Self::auto(&HostCapacity::detect());
        Self {
            enabled,
            max_vms: env_u64("NANOSB_MAX_VMS")
                .map(|v| v as usize)
                .or(auto.max_vms),
            memory_mb: env_u64("NANOSB_MEMORY_BUDGET_MB").or(auto.memory_mb),
            cpus: env_u64("NANOSB_CPU_BUDGET").map(|v| v as u32).or(auto.cpus),
            disk_min_free_mb: env_u64("NANOSB_DISK_BUDGET_MB").or(auto.disk_min_free_mb),
            sandbox_disk_mb: sandbox_disk_quota_mb(),
        }
    }

    fn auto(cap: &HostCapacity) -> Self {
        let cpus = cap.cpu_count.max(1) as u32;
        let memory_mb = cap.memory_total_bytes.map(|b| b / 1024 / 1024 * 80 / 100);
        Self {
            enabled: true,
            max_vms: Some(cpus as usize * 2),
            memory_mb,
            cpus: Some(cpus * 2),
            disk_min_free_mb: Some(2048),
            sandbox_disk_mb: None,
        }
    }
}

impl Default for HostBudget {
    fn default() -> Self {
        Self {
            enabled: true,
            max_vms: None,
            memory_mb: None,
            cpus: None,
            disk_min_free_mb: None,
            sandbox_disk_mb: None,
        }
    }
}

/// Per-sandbox disk cap from `NANOSB_SANDBOX_DISK_MB` (>0); `None` = unlimited.
pub fn sandbox_disk_quota_mb() -> Option<u64> {
    env_u64("NANOSB_SANDBOX_DISK_MB").filter(|&v| v > 0)
}

/// Resources currently committed to live supervisors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub vms: usize,
    pub memory_mb: u64,
    pub cpus: u32,
}

/// Sum the requested cpus/memory of every sandbox whose supervisor is alive,
/// plus fresh reservations that have not yet started.
///
/// Best-effort upper bound (mirrors the containerd shim's guard): stale state
/// directories left by dead supervisors are skipped.
pub fn scan_usage() -> Usage {
    let mut usage = Usage::default();
    let Ok(entries) = std::fs::read_dir(sandboxes_root()) else {
        return usage;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let path = entry.path();
        if crate::supervisor::client::SupervisorClient::new(&name).is_running() {
            usage.vms += 1;
            if let Ok(raw) = std::fs::read_to_string(path.join("config.json")) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                    usage.memory_mb += v.get("memory_mb").and_then(|x| x.as_u64()).unwrap_or(0);
                    usage.cpus += v.get("cpus").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                }
            }
        } else if let Some((mb, cpus)) = fresh_reservation(&path) {
            usage.vms += 1;
            usage.memory_mb += mb;
            usage.cpus += cpus;
        }
    }
    usage
}

const RESERVATION_FILE: &str = ".reserving";
const RESERVATION_TTL: std::time::Duration = std::time::Duration::from_secs(120);

fn reservation_path(name: &str) -> std::path::PathBuf {
    sandboxes_root().join(name).join(RESERVATION_FILE)
}

fn fresh_reservation(dir: &std::path::Path) -> Option<(u64, u32)> {
    let path = dir.join(RESERVATION_FILE);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    if modified.elapsed().ok()? > RESERVATION_TTL {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    let mut parts = raw.split_whitespace();
    let mb = parts.next()?.parse().ok()?;
    let cpus = parts.next()?.parse().ok()?;
    Some((mb, cpus))
}

#[cfg(unix)]
fn lock_admission() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let path = nanosb_home().join("admission.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return None;
    }
    Some(f)
}

#[cfg(not(unix))]
fn lock_admission() -> Option<std::fs::File> {
    None
}

/// Admit `name` and atomically record a short-lived reservation so concurrent
/// spawns cannot both pass the same budget check. The reservation is cleared by
/// [`release_reservation`] on failure or expires after [`RESERVATION_TTL`].
pub fn reserve(name: &str, need_mb: u64, need_cpus: u32) -> Result<(), AdmissionError> {
    let budget = HostBudget::resolve();
    if !budget.enabled {
        return Ok(());
    }
    let _guard = lock_admission();
    check_disk(&budget, &HostCapacity::detect())?;
    admit(&budget, scan_usage(), need_mb, need_cpus)?;
    let path = reservation_path(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{need_mb} {need_cpus}"));
    Ok(())
}

/// Drop a reservation created by [`reserve`] (e.g. when the spawn fails).
pub fn release_reservation(name: &str) {
    let _ = std::fs::remove_file(reservation_path(name));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    VmCapReached { current: usize, max: usize },
    MemoryBudget { need_mb: u64, available_mb: u64 },
    CpuBudget { need: u32, available: u32 },
    DiskLow { free_mb: u64, min_free_mb: u64 },
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VmCapReached { current, max } => write!(
                f,
                "sandbox VM cap reached ({current}/{max} running); free a sandbox or raise NANOSB_MAX_VMS"
            ),
            Self::MemoryBudget {
                need_mb,
                available_mb,
            } => write!(
                f,
                "host memory budget exceeded (need {need_mb} MiB, {available_mb} MiB available); \
                 free memory or raise NANOSB_MEMORY_BUDGET_MB"
            ),
            Self::CpuBudget { need, available } => write!(
                f,
                "host CPU budget exceeded (need {need}, {available} available); raise NANOSB_CPU_BUDGET"
            ),
            Self::DiskLow {
                free_mb,
                min_free_mb,
            } => write!(
                f,
                "host disk too full ({free_mb} MiB free, need at least {min_free_mb} MiB); free disk space"
            ),
        }
    }
}

impl std::error::Error for AdmissionError {}

/// Whether one more sandbox needing `need_mb` MiB and `need_cpus` fits.
pub fn admit(
    budget: &HostBudget,
    usage: Usage,
    need_mb: u64,
    need_cpus: u32,
) -> Result<(), AdmissionError> {
    if !budget.enabled {
        return Ok(());
    }
    if let Some(max) = budget.max_vms {
        if usage.vms + 1 > max {
            return Err(AdmissionError::VmCapReached {
                current: usage.vms,
                max,
            });
        }
    }
    if let Some(budget_mb) = budget.memory_mb {
        let available_mb = budget_mb.saturating_sub(usage.memory_mb);
        if need_mb > available_mb {
            return Err(AdmissionError::MemoryBudget {
                need_mb,
                available_mb,
            });
        }
    }
    if let Some(budget_cpus) = budget.cpus {
        let available = budget_cpus.saturating_sub(usage.cpus);
        if need_cpus > available {
            return Err(AdmissionError::CpuBudget {
                need: need_cpus,
                available,
            });
        }
    }
    Ok(())
}

/// Disk-headroom guard, independent of per-sandbox reservations.
pub fn check_disk(budget: &HostBudget, cap: &HostCapacity) -> Result<(), AdmissionError> {
    if !budget.enabled {
        return Ok(());
    }
    if let (Some(min), Some(free)) = (budget.disk_min_free_mb, cap.disk_free_bytes) {
        let free_mb = free / 1024 / 1024;
        if free_mb < min {
            return Err(AdmissionError::DiskLow {
                free_mb,
                min_free_mb: min,
            });
        }
    }
    Ok(())
}

/// Resolve the budget, scan live usage, and admit a new sandbox. This is the
/// single entry point every spawn path should call.
pub fn check_admission(need_mb: u64, need_cpus: u32) -> Result<(), AdmissionError> {
    let budget = HostBudget::resolve();
    if !budget.enabled {
        return Ok(());
    }
    check_disk(&budget, &HostCapacity::detect())?;
    admit(&budget, scan_usage(), need_mb, need_cpus)
}

/// Format a byte count compactly: `512B`, `2K`, `820M`, `16G`.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    fn large(v: f64, unit: &str) -> String {
        if v < 10.0 {
            format!("{v:.1}{unit}")
        } else {
            format!("{v:.0}{unit}")
        }
    }
    if bytes >= TIB {
        large(bytes as f64 / TIB as f64, "T")
    } else if bytes >= GIB {
        large(bytes as f64 / GIB as f64, "G")
    } else if bytes >= MIB {
        format!("{:.0}M", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.0}K", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes}B")
    }
}

fn env_u64(key: &str) -> Option<u64> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

fn nanosb_home() -> std::path::PathBuf {
    crate::supervisor::nanosb_home()
}

fn disk_free_bytes() -> Option<u64> {
    let path = nanosb_home();
    let target = if path.exists() {
        path
    } else {
        path.parent()?.to_path_buf()
    };
    let out = std::process::Command::new("df")
        .arg("-P")
        .arg("-k")
        .arg(&target)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let avail_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(avail_kb.saturating_mul(1024))
}

#[cfg(target_os = "macos")]
fn total_memory_bytes() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(target_os = "linux")]
fn total_memory_bytes() -> Option<u64> {
    meminfo_kb("MemTotal:")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn total_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn available_memory_bytes() -> Option<u64> {
    meminfo_kb("MemAvailable:")
}

#[cfg(not(target_os = "linux"))]
fn available_memory_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn meminfo_kb(prefix: &str) -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(max_vms: usize, memory_mb: u64, cpus: u32) -> HostBudget {
        HostBudget {
            enabled: true,
            max_vms: Some(max_vms),
            memory_mb: Some(memory_mb),
            cpus: Some(cpus),
            disk_min_free_mb: None,
            sandbox_disk_mb: None,
        }
    }

    #[test]
    fn admits_within_budget() {
        assert!(admit(&budget(4, 8192, 8), Usage::default(), 4096, 2).is_ok());
    }

    #[test]
    fn rejects_vm_cap() {
        let u = Usage {
            vms: 2,
            ..Default::default()
        };
        assert_eq!(
            admit(&budget(2, 8192, 8), u, 1024, 1),
            Err(AdmissionError::VmCapReached { current: 2, max: 2 })
        );
    }

    #[test]
    fn rejects_memory_overcommit() {
        let u = Usage {
            memory_mb: 6000,
            ..Default::default()
        };
        assert_eq!(
            admit(&budget(8, 8192, 8), u, 3000, 1),
            Err(AdmissionError::MemoryBudget {
                need_mb: 3000,
                available_mb: 2192
            })
        );
    }

    #[test]
    fn rejects_cpu_overcommit() {
        let u = Usage {
            cpus: 7,
            ..Default::default()
        };
        assert_eq!(
            admit(&budget(8, 8192, 8), u, 512, 2),
            Err(AdmissionError::CpuBudget {
                need: 2,
                available: 1
            })
        );
    }

    #[test]
    fn disabled_admits_anything() {
        let mut b = budget(0, 0, 0);
        b.enabled = false;
        assert!(admit(
            &b,
            Usage {
                vms: 99,
                memory_mb: 10_000,
                cpus: 99
            },
            10_000,
            64
        )
        .is_ok());
    }

    #[test]
    fn disk_headroom_enforced() {
        let cap = HostCapacity {
            disk_free_bytes: Some(1024 * 1024 * 512),
            ..Default::default()
        };
        let b = HostBudget {
            disk_min_free_mb: Some(2048),
            ..Default::default()
        };
        assert_eq!(
            check_disk(&b, &cap),
            Err(AdmissionError::DiskLow {
                free_mb: 512,
                min_free_mb: 2048
            })
        );
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(2048), "2K");
        assert_eq!(format_bytes(1024 * 1024), "1M");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0G");
        assert_eq!(format_bytes(16 * 1024 * 1024 * 1024), "16G");
        assert_eq!(format_bytes(120 * 1024 * 1024 * 1024), "120G");
        assert_eq!(format_bytes(1024_u64.pow(4)), "1.0T");
    }

    #[test]
    fn detect_does_not_panic() {
        let cap = HostCapacity::detect();
        assert!(cap.cpu_count >= 1 || cfg!(not(any(target_os = "macos", target_os = "linux"))));
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_reads_env_overrides() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NANOSB_ADMISSION", "off");
        std::env::set_var("NANOSB_MAX_VMS", "3");
        std::env::set_var("NANOSB_MEMORY_BUDGET_MB", "4096");
        std::env::set_var("NANOSB_CPU_BUDGET", "5");
        std::env::set_var("NANOSB_SANDBOX_DISK_MB", "512");
        let b = HostBudget::resolve();
        std::env::remove_var("NANOSB_ADMISSION");
        std::env::remove_var("NANOSB_MAX_VMS");
        std::env::remove_var("NANOSB_MEMORY_BUDGET_MB");
        std::env::remove_var("NANOSB_CPU_BUDGET");
        std::env::remove_var("NANOSB_SANDBOX_DISK_MB");
        assert!(!b.enabled);
        assert_eq!(b.max_vms, Some(3));
        assert_eq!(b.memory_mb, Some(4096));
        assert_eq!(b.cpus, Some(5));
        assert_eq!(b.sandbox_disk_mb, Some(512));
    }

    #[test]
    fn fresh_reservation_parses() {
        let dir = std::env::temp_dir().join(format!(
            "nsb-cap-resv-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(fresh_reservation(&dir), None);
        std::fs::write(dir.join(RESERVATION_FILE), "2048 2").unwrap();
        assert_eq!(fresh_reservation(&dir), Some((2048, 2)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
