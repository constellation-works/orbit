//! Worker containment limits [ORB-12903].

use orbit_config::{MemoryLimit, MemoryUnit, WorkerContainmentSettings};

use crate::application::job::pipeline::worker::scope::WorkerLimits;

fn settings(memory_high: MemoryLimit, memory_max: MemoryLimit) -> WorkerContainmentSettings {
    WorkerContainmentSettings {
        enabled: true,
        strict: false,
        memory_high,
        memory_max,
        tasks_max: 512,
    }
}

/// `None` means "containment off" and launches the worker unbounded, which is
/// how one run took the host down (2026-09-23 OOM outage). Every shape an
/// admitted limit can take must therefore yield limits [ORB-12913].
#[test]
fn enabled_containment_always_yields_limits() {
    let mut shapes = vec![
        MemoryLimit::Infinity,
        MemoryLimit::Percent(1),
        MemoryLimit::Percent(100),
        MemoryLimit::Bytes {
            amount: u64::MAX,
            unit: None,
        },
    ];
    shapes.extend(
        [MemoryUnit::K, MemoryUnit::M, MemoryUnit::G, MemoryUnit::T].map(|unit| {
            MemoryLimit::Bytes {
                amount: 1,
                unit: Some(unit),
            }
        }),
    );
    for high in &shapes {
        for max in &shapes {
            assert!(
                WorkerLimits::from_settings(&settings(*high, *max)).is_some(),
                "enabled containment dropped limits for {high} / {max}"
            );
        }
    }
}
