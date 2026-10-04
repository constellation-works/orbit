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

/// Security boundary: explicit scope environment assignments contain only
/// admitted ambient variables and deliberate worker edits [ORB-13963].
#[test]
fn scope_launch_carries_allowlisted_environment_and_honors_edits() {
    use super::super::scope::scoped_worker_command;
    use orbit_common::test_env;
    use std::process::Command;

    let _environment = test_env::scoped([
        ("PATH", Some("/launcher/tools:/usr/bin")),
        ("LANG", Some("C")),
        ("DATABASE_URL", Some("secret")),
        ("ORBIT_OPERATOR", Some("1")),
        ("ORBIT_ROOT", Some("/unrelated/root")),
    ]);
    let limits =
        WorkerLimits::from_settings(&settings(MemoryLimit::Infinity, MemoryLimit::Infinity))
            .unwrap();
    for path_override in [None, Some("/explicit/tools:/bin")] {
        let mut base = Command::new("/worker/orbit");
        base.arg("job").arg("run-pipeline-worker").arg("run-1");
        base.current_dir("/workspace").env_remove("ORBIT_ROOT");
        base.env("ORBIT_WORKER_CONTEXT_REQUIRED", "1");
        if let Some(path) = path_override {
            base.env("PATH", path);
        }
        let scoped = scoped_worker_command(&base, "orbit-worker-test.scope", &limits);
        let args = scoped
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        let separator = args.iter().position(|arg| *arg == "--").unwrap();
        let env = args[..separator]
            .iter()
            .filter_map(|arg| arg.strip_prefix("--setenv="))
            .filter_map(|entry| entry.split_once('='))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            env.get("PATH"),
            Some(&path_override.unwrap_or("/launcher/tools:/usr/bin")),
            "ORB-13963: a worker must not fall back to the user manager's PATH"
        );
        assert_eq!(env.get("LANG"), Some(&"C"));
        assert_eq!(env.get("ORBIT_WORKER_CONTEXT_REQUIRED"), Some(&"1"));
        for excluded in ["DATABASE_URL", "ORBIT_OPERATOR", "ORBIT_ROOT"] {
            assert!(
                !env.contains_key(excluded),
                "scope must not forward {excluded}"
            );
            assert!(
                !scoped
                    .get_envs()
                    .any(|(key, value)| key == excluded && value.is_some())
            );
        }
        assert_eq!(
            &args[separator + 1..],
            &["/worker/orbit", "job", "run-pipeline-worker", "run-1"]
        );
        assert_eq!(scoped.get_current_dir(), base.get_current_dir());
    }
}
