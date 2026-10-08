//! Admitted unit tests: deterministic fault injection/cache concurrency and
//! combinatorial pure hysteresis. No mutable Orbit state or ambient host pressure.
use super::super::{DiskSample, HostResourceSample};
use chrono::{TimeZone, Utc};
use std::path::PathBuf;

pub(super) fn sample(
    second: i64,
    cpu: Option<f64>,
    memory: Option<f64>,
    disk: Option<f64>,
) -> HostResourceSample {
    HostResourceSample {
        sampled_at: Utc.timestamp_opt(1_700_000_000 + second, 0).unwrap(),
        cpu_percent: cpu,
        memory_percent: memory,
        disks: vec![DiskSample {
            path: PathBuf::from("/workspace"),
            used_percent: disk,
        }],
    }
}
