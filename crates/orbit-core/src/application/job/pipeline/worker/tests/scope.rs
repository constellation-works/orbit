//! Worker containment limits [ORB-12903].

use std::collections::BTreeMap;
use std::process::Command;

use orbit_config::{MemoryLimit, MemoryUnit, WorkerContainmentSettings};

use crate::application::job::pipeline::worker::command::WorkerCommandConfig;
use crate::application::job::pipeline::worker::scope::WorkerLimits;

fn settings(memory_high: MemoryLimit, memory_max: MemoryLimit) -> WorkerContainmentSettings {
    WorkerContainmentSettings {
        enabled: true,
        strict: false,
        memory_high,
        memory_max,
        tasks_max: 512,
        cpu_quota_percent: 0,
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
/// Those values stay in the child environment; `--setenv` carries the name
/// only, so they are absent from argv [ORB-15175].
#[test]
fn scope_launch_carries_allowlisted_environment_and_honors_edits() {
    use super::super::scope::scoped_worker_command;
    use orbit_common::test_env;

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
        let path = path_override.unwrap_or("/launcher/tools:/usr/bin");
        assert_scoped_environment(
            &scoped,
            &[
                ("PATH", path),
                ("LANG", "C"),
                ("ORBIT_WORKER_CONTEXT_REQUIRED", "1"),
            ],
            &["DATABASE_URL", "ORBIT_OPERATOR", "ORBIT_ROOT"],
            &["secret", path, "/unrelated/root"],
        );
        let args = command_args(&scoped);
        let separator = args.iter().position(|arg| *arg == "--").unwrap();
        for property in [
            "--property=MemoryHigh=infinity",
            "--property=MemoryMax=infinity",
            "--property=TasksMax=512",
            "--property=OOMPolicy=continue",
        ] {
            assert!(
                args[..separator].contains(&property),
                "scope dropped containment property {property}"
            );
        }
        assert_eq!(
            &args[separator + 1..],
            &["/worker/orbit", "job", "run-pipeline-worker", "run-1"]
        );
        assert_eq!(scoped.get_current_dir(), base.get_current_dir());
        // `machine.worker_cpu_quota` of 0 (the unset default) adds no CPU
        // property; a positive value adds `CPUQuota=<n>%` [ORB-15196].
        assert!(
            !args[..separator].iter().any(|arg| arg.contains("CPU")),
            "an unset CPU quota must not reach the scope: {args:?}"
        );
        let limited = WorkerLimits::from_settings(&WorkerContainmentSettings {
            cpu_quota_percent: 150,
            ..settings(MemoryLimit::Infinity, MemoryLimit::Infinity)
        })
        .unwrap();
        let limited = scoped_worker_command(&base, "orbit-worker-test.scope", &limited);
        assert!(
            command_args(&limited).contains(&"--property=CPUQuota=150%"),
            "a configured CPU quota must reach the scope"
        );
    }
}

/// Admitted ambient credentials and clock-file defaults reach a contained
/// worker through environment data, and their values are absent from
/// `systemd-run` and worker argv [ORB-15175].
#[test]
fn admitted_credentials_reach_the_scoped_worker_environment_and_stay_off_argv() {
    use orbit_common::test_env;

    const OAUTH: &str = "clock-oauth-canary";
    const API_KEY: &str = "clock-api-canary";
    const AMBIENT_GH: &str = "ambient-gh-canary";
    const CLOCK_GH: &str = "clock-gh-must-not-win";
    const DATABASE: &str = "postgres://not-admitted";
    const CLOCK_DATABASE: &str = "clock-db-must-not-apply";
    const EXPLICIT_PATH: &str = "/explicit/tools:/bin";

    let root = tempfile::tempdir().expect("policy root");
    std::fs::write(
        root.path().join("config.toml"),
        "[execution.env]\npass = [\"HOME\", \"PATH\", \"CLAUDE_CODE_OAUTH_TOKEN\", \"ANTHROPIC_API_KEY\", \"GH_TOKEN\"]\n",
    )
    .expect("policy config");
    let _environment = test_env::scoped([
        ("PATH", Some("/launcher/tools:/usr/bin")),
        ("LANG", Some("C")),
        ("HOME", Some("/home/worker")),
        ("GH_TOKEN", Some(AMBIENT_GH)),
        ("DATABASE_URL", Some(DATABASE)),
        ("ORBIT_OPERATOR", Some("1")),
        ("ORBIT_ROOT", Some("/unrelated/root")),
        ("CLAUDE_CODE_OAUTH_TOKEN", None),
        ("ANTHROPIC_API_KEY", None),
    ]);
    let policy =
        orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(root.path()))
            .expect("policy loads")
            .execution_env
            .with_defaults(&[
                ("CLAUDE_CODE_OAUTH_TOKEN".to_string(), OAUTH.to_string()),
                ("ANTHROPIC_API_KEY".to_string(), API_KEY.to_string()),
                ("GH_TOKEN".to_string(), CLOCK_GH.to_string()),
                ("DATABASE_URL".to_string(), CLOCK_DATABASE.to_string()),
            ]);
    let limits =
        WorkerLimits::from_settings(&settings(MemoryLimit::Infinity, MemoryLimit::Infinity))
            .expect("enabled containment yields limits");
    let mut base = Command::new("/worker/orbit");
    base.arg("job")
        .arg("run-pipeline-worker")
        .arg("run-1")
        .current_dir("/workspace")
        .env_remove("ORBIT_ROOT")
        .env("PATH", EXPLICIT_PATH)
        .env("ORBIT_WORKER_CONTEXT_REQUIRED", "1");
    let scoped = WorkerCommandConfig::for_prepared_worker(Some(limits), policy)
        .contain_prepared(base, "run-1", Ok(()))
        .expect("injected scope availability contains the worker");

    assert_scoped_environment(
        &scoped,
        &[
            ("PATH", EXPLICIT_PATH),
            ("LANG", "C"),
            ("CLAUDE_CODE_OAUTH_TOKEN", OAUTH),
            ("ANTHROPIC_API_KEY", API_KEY),
            ("GH_TOKEN", AMBIENT_GH),
            ("ORBIT_WORKER_CONTEXT_REQUIRED", "1"),
        ],
        &["DATABASE_URL", "ORBIT_OPERATOR", "ORBIT_ROOT"],
        &[
            OAUTH,
            API_KEY,
            AMBIENT_GH,
            CLOCK_GH,
            DATABASE,
            CLOCK_DATABASE,
            EXPLICIT_PATH,
            "/launcher/tools:/usr/bin",
            "/unrelated/root",
        ],
    );
    let args = command_args(&scoped);
    let separator = args.iter().position(|arg| *arg == "--").unwrap();
    assert!(
        args[..separator].iter().any(|arg| {
            arg.starts_with("--unit=orbit-worker-")
                && arg.contains("run-1")
                && arg.ends_with(".scope")
        }),
        "contained launch dropped its worker scope unit: {args:?}"
    );
    assert_eq!(
        &args[separator + 1..],
        &["/worker/orbit", "job", "run-pipeline-worker", "run-1"]
    );
}

fn command_args(command: &Command) -> Vec<&str> {
    command
        .get_args()
        .map(|arg| arg.to_str().expect("scope arguments are utf-8"))
        .collect()
}

fn assert_scoped_environment(
    scoped: &Command,
    present: &[(&str, &str)],
    absent: &[&str],
    absent_from_argv: &[&str],
) {
    let args = command_args(scoped);
    let separator = args
        .iter()
        .position(|arg| *arg == "--")
        .expect("scope command separates systemd-run options from the worker");
    let setenv = args[..separator]
        .iter()
        .filter_map(|arg| arg.strip_prefix("--setenv="))
        .collect::<Vec<_>>();
    for entry in &setenv {
        assert!(
            !entry.contains('='),
            "ORB-15175: scope --setenv must be name-only so credential values stay off argv, got {entry}"
        );
    }
    let assigned = scoped
        .get_envs()
        .filter_map(|(key, value)| Some((key.to_str()?.to_string(), value?.to_str()?.to_string())))
        .collect::<BTreeMap<_, _>>();
    for (name, value) in present {
        assert_eq!(
            assigned.get(*name).map(String::as_str),
            Some(*value),
            "ORB-13963/ORB-15175: {name} must reach the worker through its environment"
        );
        assert!(
            setenv.contains(name),
            "scope dropped the name-only --setenv for {name}"
        );
    }
    for name in absent {
        assert!(!setenv.contains(name), "scope must not forward {name}");
        assert!(
            !scoped
                .get_envs()
                .any(|(key, value)| key == *name && value.is_some()),
            "scope must not assign {name}"
        );
    }
    for value in absent_from_argv {
        assert!(
            !args.iter().any(|arg| arg.contains(value)),
            "ORB-15175: {value} leaked into systemd-run or worker argv"
        );
    }
}
