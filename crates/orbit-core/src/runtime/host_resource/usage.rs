//! Host CPU and memory in absolute units, for the per-minute resource history.
//!
//! The throttle reads CPU pressure from `/proc/loadavg` (`platform.rs`), which
//! counts the run queue and runs far past 100%: dk-server-1 showed load 36 on
//! 15 cores at about 95% utilization. The history needs utilization in cores,
//! the unit worker scopes report, so Linux reads `/proc/stat` tick counters.
//! macOS reads aggregate Mach CPU ticks and VM statistics. Memory uses the
//! throttle's accounting on each platform, in bytes. Injected proc roots
//! always read fixture files, including on macOS.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// How often a delivery worker samples its scope and the host. A run minute's
/// `samples` count therefore bounds how much of the minute it covered.
pub(crate) const RESOURCE_SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// One reading of the host's cumulative CPU ticks and current memory use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostUsageReading {
    pub(super) ticks: CpuTicks,
    /// Online host CPUs (the `cpuN` count for procfs).
    pub(crate) cpu_count: u32,
    pub(crate) memory_used_bytes: u64,
    pub(crate) memory_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CpuTicks {
    Proc {
        busy: u64,
        total: u64,
    },
    #[cfg(any(target_os = "macos", test))]
    Mach([u32; 4]),
}

impl HostUsageReading {
    /// Busy and total ticks in one usable interval. Mach counters wrap
    /// individually at 32 bits; procfs counters must advance monotonically.
    /// Stalled counters or a changed CPU count cannot measure utilization.
    pub(crate) fn cpu_delta(self, before: Self) -> Option<(u64, u64)> {
        if self.cpu_count != before.cpu_count {
            return None;
        }
        let (busy, total) = match (self.ticks, before.ticks) {
            (
                CpuTicks::Proc { busy, total },
                CpuTicks::Proc {
                    busy: old_busy,
                    total: old_total,
                },
            ) => (busy.checked_sub(old_busy)?, total.checked_sub(old_total)?),
            #[cfg(any(target_os = "macos", test))]
            (CpuTicks::Mach(current), CpuTicks::Mach(old)) => {
                let delta =
                    std::array::from_fn::<_, 4, _>(|i| u64::from(current[i].wrapping_sub(old[i])));
                let total = delta.iter().sum::<u64>();
                (total - delta[2], total)
            }
            #[cfg(any(target_os = "macos", test))]
            _ => return None,
        };
        (total > 0 && busy <= total).then_some((busy, total))
    }
}

/// Read native macOS statistics for the default `/proc` root, otherwise
/// `<proc_root>/{stat,meminfo}`. A failed source is unknown, never zero use.
pub(crate) fn read_host_usage(proc_root: &Path) -> Option<HostUsageReading> {
    #[cfg(target_os = "macos")]
    if proc_root == Path::new("/proc") {
        return super::platform::with_mach_host(|host| {
            let ticks = super::platform::host_cpu_ticks(host)?;
            // SAFETY: sysconf has no pointer arguments and counts online CPUs.
            let cpu_count = u32::try_from(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) })
                .ok()
                .filter(|count| *count > 0)?;
            let (memory_used_bytes, memory_total_bytes) = super::platform::host_memory_bytes(host)?;
            Some(HostUsageReading {
                ticks: CpuTicks::Mach(ticks),
                cpu_count,
                memory_used_bytes,
                memory_total_bytes,
            })
        });
    }
    let (busy_ticks, total_ticks, cpu_count) = parse_stat(&read(&proc_root.join("stat"))?)?;
    let (memory_used_bytes, memory_total_bytes) =
        parse_meminfo(&read(&proc_root.join("meminfo"))?)?;
    Some(HostUsageReading {
        ticks: CpuTicks::Proc {
            busy: busy_ticks,
            total: total_ticks,
        },
        cpu_count,
        memory_used_bytes,
        memory_total_bytes,
    })
}

fn read(path: &Path) -> Option<String> {
    let mut data = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(1 << 20)
        .read_to_string(&mut data)
        .ok()?;
    Some(data)
}

/// The aggregate `cpu` line's busy and total ticks, and the `cpuN` count.
/// `guest` and `guest_nice` are already inside `user` and `nice`, so only the
/// first eight fields count.
fn parse_stat(data: &str) -> Option<(u64, u64, u32)> {
    let mut aggregate = None;
    let mut cpu_count = 0u32;
    for line in data.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("cpu") => {
                let ticks = fields
                    .take(8)
                    .map(|field| field.parse::<u64>().ok())
                    .collect::<Option<Vec<_>>>()?;
                // user nice system idle iowait irq softirq steal
                if ticks.len() < 5 {
                    return None;
                }
                let total: u64 = ticks.iter().sum();
                let busy = total - ticks[3] - ticks[4];
                aggregate = Some((busy, total));
            }
            Some(name)
                if name
                    .strip_prefix("cpu")
                    .is_some_and(|index| index.parse::<u32>().is_ok()) =>
            {
                cpu_count += 1;
            }
            _ => {}
        }
    }
    let (busy, total) = aggregate?;
    (cpu_count > 0).then_some((busy, total, cpu_count))
}

/// `MemTotal - MemAvailable` and `MemTotal`, in bytes.
fn parse_meminfo(data: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut available = None;
    for line in data.lines() {
        let mut parts = line.split_whitespace();
        let slot = match parts.next() {
            Some("MemTotal:") => &mut total,
            Some("MemAvailable:") => &mut available,
            _ => continue,
        };
        let value = parts.next()?.parse::<u64>().ok()?;
        if parts.next() != Some("kB") {
            return None;
        }
        *slot = Some(value.checked_mul(1024)?);
    }
    let (total, available) = (total?, available?);
    (total > 0 && available <= total).then(|| (total - available, total))
}
