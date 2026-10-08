//! Host resource observations through the composed runtime's public injection boundary.
use chrono::{Duration, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::host_resource::{
    DiskSample, HostResourceProbe, HostResourceSample, ResourceSeverity,
    default_host_resource_probe,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Fixture(Mutex<HostResourceSample>);
impl HostResourceProbe for Fixture {
    fn sample(&self, paths: &[PathBuf]) -> HostResourceSample {
        let mut sample = self.0.lock().unwrap().clone();
        sample.disks = paths
            .iter()
            .map(|path| DiskSample {
                path: path.clone(),
                used_percent: Some(10.0),
            })
            .collect();
        sample
    }
}

#[test]
fn runtime_injection_shares_hysteresis_and_reports_unknown_and_stale() {
    if !super::dispatch_admission::isolated(
        "host_resources::runtime_injection_shares_hysteresis_and_reports_unknown_and_stale",
    ) {
        return;
    }
    let probe = Arc::new(Fixture(Mutex::new(HostResourceSample {
        sampled_at: Utc::now() - Duration::seconds(10),
        cpu_percent: Some(99.0),
        memory_percent: Some(25.0),
        disks: vec![],
    })));
    let runtime = OrbitRuntime::in_memory()
        .unwrap()
        .with_host_resource_probe(probe.clone());
    let first = runtime.host_resource_status();
    assert!(!first.throttle);
    assert_eq!(first.disk.as_ref().unwrap().reading.percent, Some(10.0));
    assert_eq!(first.cpu.severity, ResourceSeverity::Critical);
    let clone = runtime.clone();
    probe.0.lock().unwrap().sampled_at = Utc::now() - Duration::seconds(5);
    assert!(!clone.host_resource_status().throttle);
    probe.0.lock().unwrap().sampled_at = Utc::now();
    let held = runtime.host_resource_status();
    assert!(held.throttle);
    assert!(
        held.disks
            .iter()
            .any(|disk| disk.path == runtime.global_root())
    );
    {
        let mut data = probe.0.lock().unwrap();
        data.cpu_percent = None;
    }
    let unknown = clone.host_resource_status();
    assert!(!unknown.throttle);
    assert_eq!(unknown.cpu.severity, ResourceSeverity::Unknown);
    probe.0.lock().unwrap().sampled_at = Utc::now() - Duration::seconds(20);
    let stale = runtime.host_resource_status();
    assert!(stale.stale);
    assert!(stale.disk.is_none());
    assert_eq!(stale.memory.severity, ResourceSeverity::Unknown);
}

#[test]
fn native_sampler_observes_the_host_and_missing_directory_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vec![dir.path().join("not-created/worktrees")];
    let probe = default_host_resource_probe();
    let sample = probe.sample(&paths);
    assert!(
        sample.disks[0]
            .used_percent
            .is_some_and(|value| (0.0..=100.0).contains(&value))
    );
    #[cfg(target_os = "linux")]
    {
        assert!(sample.cpu_percent.is_some_and(|value| value >= 0.0));
        assert!(
            sample
                .memory_percent
                .is_some_and(|value| (0.0..=100.0).contains(&value))
        );
    }
    #[cfg(target_os = "macos")]
    assert!(
        sample
            .memory_percent
            .is_some_and(|value| (0.0..=100.0).contains(&value))
    );
    assert_eq!(probe.sample(&paths).sampled_at, sample.sampled_at);
    #[cfg(target_os = "macos")]
    {
        // The first Mach observation establishes the tick baseline. A fresh
        // sample must expose a usable aggregate CPU delta, not remain unknown.
        std::thread::sleep(
            orbit_core::runtime::host_resource::RESOURCE_CACHE_TTL
                + std::time::Duration::from_millis(30),
        );
        let fresh = probe.sample(&paths);
        assert!(fresh.sampled_at > sample.sampled_at);
        assert!(
            fresh
                .cpu_percent
                .is_some_and(|value| (0.0..=100.0).contains(&value)),
            "native Mach tick deltas must report aggregate host CPU after warmup"
        );
    }
}

#[test]
fn first_admission_in_a_fresh_process_reads_host_cpu() {
    if !super::dispatch_admission::isolated(
        "host_resources::first_admission_in_a_fresh_process_reads_host_cpu",
    ) {
        return;
    }
    // The native probe is process-global: only a fresh child process makes
    // this runtime's first observation the host's first observation.
    let runtime = OrbitRuntime::in_memory().unwrap();
    let started = std::time::Instant::now();
    let status = runtime.host_resource_status();
    let elapsed = started.elapsed();
    assert!(
        status.cpu.percent.is_some_and(|value| value >= 0.0),
        "first observation must carry host CPU, got {:?}",
        status.cpu
    );
    assert!(
        elapsed
            <= orbit_core::runtime::host_resource::RESOURCE_CACHE_TTL
                + std::time::Duration::from_millis(500),
        "first observation took {elapsed:?}; admission may wait about RESOURCE_CACHE_TTL"
    );
    let admission = runtime.resource_admission();
    assert!(
        !admission
            .unknown
            .iter()
            .any(|reason| reason.starts_with("cpu")),
        "first admission must not report CPU unknown: {:?}",
        admission.unknown
    );
}
