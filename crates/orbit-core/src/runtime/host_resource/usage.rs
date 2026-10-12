//! Host CPU and memory in absolute units, for the per-minute resource history.
//!
//! The throttle reads CPU pressure from `/proc/loadavg` (`platform.rs`), which
//! counts the run queue and runs far past 100%: dk-server-1 showed load 36 on
//! 15 cores at about 95% utilization. The history needs utilization in cores,
//! the unit worker scopes report, so it reads `/proc/stat` tick counters
//! instead and leaves the throttle alone. Memory is `MemTotal - MemAvailable`,
//! the same non-reclaimable measure the throttle uses, in bytes.

use std::io::Read;
use std::path::Path;

/// One reading of the host's cumulative CPU ticks and current memory use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostUsageReading {
    /// Ticks spent outside idle and iowait, summed over every CPU.
    pub(crate) busy_ticks: u64,
    /// All ticks, summed over every CPU.
    pub(crate) total_ticks: u64,
    /// CPUs counted in `/proc/stat`.
    pub(crate) cpu_count: u32,
    pub(crate) memory_used_bytes: u64,
    pub(crate) memory_total_bytes: u64,
}

/// Read `<proc_root>/stat` and `<proc_root>/meminfo`. `None` when either is
/// missing or malformed: an unknown reading is never zero use.
pub(crate) fn read_host_usage(proc_root: &Path) -> Option<HostUsageReading> {
    let (busy_ticks, total_ticks, cpu_count) = parse_stat(&read(&proc_root.join("stat"))?)?;
    let (memory_used_bytes, memory_total_bytes) =
        parse_meminfo(&read(&proc_root.join("meminfo"))?)?;
    Some(HostUsageReading {
        busy_ticks,
        total_ticks,
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
