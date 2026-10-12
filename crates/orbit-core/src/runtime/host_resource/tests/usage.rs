//! Admitted unit tests: platform-kernel `/proc` parsing for the per-minute
//! host history, against fixture files.

use std::path::Path;

use super::super::usage::read_host_usage;

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
        before.total_ticks,
        1_000 + 10 + 20 + 5_000 + 100 + 5 + 5 + 10
    );
    assert_eq!(before.busy_ticks, 1_000 + 10 + 20 + 5 + 5 + 10);
    assert_eq!(before.memory_total_bytes, 28_311_552 * 1024);
    assert_eq!(before.memory_used_bytes, (28_311_552 - 12_582_912) * 1024);

    // One second on 15 cores at 100 Hz is 1,500 ticks: 1,425 busy (95%),
    // 50 idle and 25 iowait.
    write_proc(root.path(), &stat(2_425, 5_050, 125), MEMINFO);
    let after = read_host_usage(root.path()).expect("reading");
    let busy = (after.busy_ticks - before.busy_ticks) as f64;
    let total = (after.total_ticks - before.total_ticks) as f64;
    assert_eq!(total, 1_500.0);
    let cores = busy / total * f64::from(after.cpu_count);
    assert!((cores - 14.25).abs() < 1e-9, "{cores} cores, not load 36");
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
