//! Admitted unit tests: platform-kernel host utilization and memory for the
//! per-minute history and forecast, using proc fixtures and native Mach calls.

use std::path::Path;

use super::super::usage::{CpuTicks, HostUsageReading, read_host_usage};

fn write_proc(root: &Path, stat: &str, meminfo: &str) {
    std::fs::write(root.join("stat"), stat).unwrap();
    std::fs::write(root.join("meminfo"), meminfo).unwrap();
    // The throttle's run-queue figure, which this reader must not use: load
    // 36 on 15 cores while utilization was about 95% (dk-server-1).
    std::fs::write(root.join("loadavg"), "36.00 30.00 25.00 40/900 12345\n").unwrap();
}

fn stat(user: u64, idle: u64, iowait: u64) -> String {
    // user nice system idle iowait irq softirq steal guest guest_nice
    let mut stat = format!("cpu  {user} 10 20 {idle} {iowait} 5 5 10 300 0\n");
    for cpu in 0..15 {
        stat.push_str(&format!("cpu{cpu} 1 0 0 1 0 0 0 0 0 0\n"));
    }
    stat.push_str("intr 1 2 3\nctxt 4\nbtime 5\nprocs_running 36\n");
    stat
}

const MEMINFO: &str = "MemTotal:       28311552 kB\nMemFree:          524288 kB\n\
                       MemAvailable:   12582912 kB\nCached:         9437184 kB\n";

/// CPU is busy cores from `/proc/stat` tick deltas: idle and iowait are idle
/// time, guest time is already inside user, and the count is the `cpuN`
/// lines. Memory is `MemTotal - MemAvailable`, in bytes.
#[test]
fn host_usage_is_tick_utilization_in_cores_and_unavailable_memory() {
    let root = tempfile::tempdir().unwrap();
    write_proc(root.path(), &stat(1_000, 5_000, 100), MEMINFO);
    let before = read_host_usage(root.path()).expect("reading");
    assert_eq!(before.cpu_count, 15);
    assert_eq!(
        before.ticks,
        CpuTicks::Proc {
            busy: 1_000 + 10 + 20 + 5 + 5 + 10,
            total: 1_000 + 10 + 20 + 5_000 + 100 + 5 + 5 + 10
        }
    );
    assert_eq!(before.memory_total_bytes, 28_311_552 * 1024);
    assert_eq!(before.memory_used_bytes, (28_311_552 - 12_582_912) * 1024);

    // One second on 15 cores at 100 Hz is 1,500 ticks: 1,425 busy (95%),
    // 50 idle and 25 iowait.
    write_proc(root.path(), &stat(2_425, 5_050, 125), MEMINFO);
    let after = read_host_usage(root.path()).expect("reading");
    let (busy, total) = after.cpu_delta(before).unwrap();
    assert_eq!(total, 1_500);
    let cores = busy as f64 / total as f64 * f64::from(after.cpu_count);
    assert!((cores - 14.25).abs() < 1e-9, "{cores} cores, not load 36");

    // Mach's per-state 32-bit ticks wrap separately. Both native consumers
    // must use these deltas, rather than subtracting aggregate counter sums.
    for (old, new, expected) in [
        ([100, 20, 200, 10], [200, 40, 240, 20], Some((130, 170))),
        (
            [u32::MAX - 4, 20, 200, 10],
            [5, 40, 240, 20],
            Some((40, 80)),
        ),
        ([100, 20, 200, 10], [100, 20, 200, 10], None),
    ] {
        let old = HostUsageReading {
            ticks: CpuTicks::Mach(old),
            ..before
        };
        let new = HostUsageReading {
            ticks: CpuTicks::Mach(new),
            ..before
        };
        assert_eq!(new.cpu_delta(old), expected);
        assert_eq!(
            HostUsageReading {
                cpu_count: 16,
                ..new
            }
            .cpu_delta(old),
            None
        );
    }
    assert_eq!(before.cpu_delta(after), None, "proc counters reset");
    assert_eq!(after.cpu_delta(after), None, "stalled is unknown");

    // Admission uses the native reader, not a missing `/proc` on macOS.
    // Keep real successful and failed Mach calls in this admitted kernel
    // matrix, without a load generator or job fixture.
    #[cfg(target_os = "macos")]
    {
        use super::super::forecast::HostLoadProbe;
        use super::super::platform::{host_cpu_ticks, host_memory_bytes};

        assert!(host_cpu_ticks(libc::MACH_PORT_NULL as libc::mach_port_t).is_none());
        assert!(host_memory_bytes(libc::MACH_PORT_NULL as libc::mach_port_t).is_none());
        let reading = read_host_usage(Path::new("/proc")).expect("native host read");
        assert!(reading.cpu_count > 0);
        assert!(reading.memory_total_bytes > 0);
        assert!(reading.memory_used_bytes <= reading.memory_total_bytes);
        let probe = HostLoadProbe::default();
        probe.prepare();
        let load = probe.measure().expect("native forecast host load");
        let cores = load
            .cpu_cores
            .expect("ticks advance beyond XNU cache window");
        assert!(cores.is_finite() && (0.0..=f64::from(load.cpu_count)).contains(&cores));
        assert_eq!(load.cpu_count, reading.cpu_count);
        assert_eq!(load.memory_total_bytes, reading.memory_total_bytes);
        assert!(load.memory_bytes <= load.memory_total_bytes);
        println!(
            "native host: cpu_cores={cores}, cpu_count={}, memory_used_bytes={}, memory_total_bytes={}",
            load.cpu_count, load.memory_bytes, load.memory_total_bytes
        );
    }
}

/// An unreadable or malformed source is unknown, never zero use.
#[test]
fn malformed_proc_files_read_as_unknown() {
    let root = tempfile::tempdir().unwrap();
    assert!(read_host_usage(root.path()).is_none(), "missing files");
    for (stat, meminfo) in [
        (stat(1, 1, 1), "MemTotal: 100 kB\n".to_string()),
        (
            stat(1, 1, 1),
            "MemTotal: 100 kB\nMemAvailable: 200 kB\n".to_string(),
        ),
        (
            stat(1, 1, 1),
            "MemTotal: 100 MB\nMemAvailable: 50 MB\n".to_string(),
        ),
        ("cpu  1 2 x 4 5\ncpu0 1\n".to_string(), MEMINFO.to_string()),
        ("cpu  1 2 3 4 5 0 0 0\n".to_string(), MEMINFO.to_string()),
    ] {
        write_proc(root.path(), &stat, &meminfo);
        assert!(
            read_host_usage(root.path()).is_none(),
            "{stat:?} / {meminfo:?}"
        );
    }
}
