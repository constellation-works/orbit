//! Admitted unit tests: deterministic fault injection/cache concurrency and
//! combinatorial pure hysteresis. No mutable Orbit state or ambient host pressure.
use chrono::{TimeZone, Utc};
use orbit_core::runtime::host_resource::{DiskSample, HostResourceSample};
use std::path::PathBuf;

pub(crate) fn sample(
    second: i64,
    cpu: Option<f64>,
    memory: Option<f64>,
    disk: Option<f64>,
) -> HostResourceSample {
    HostResourceSample {
        sampled_at: Utc.timestamp_opt(1_700_000_000 + second, 0).unwrap(),
        cpu_percent: cpu,
        cpu_utilization_percent: None,
        memory_percent: memory,
        io_full_percent: None,
        disks: vec![DiskSample {
            path: PathBuf::from("/workspace"),
            used_percent: disk,
        }],
    }
}
