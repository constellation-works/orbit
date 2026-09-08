use std::sync::Mutex;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::ExecutorDefStoreBackend;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

use crate::application::executor::{
    DEFAULT_EXECUTOR_FILES, migrated_default_executor, migrated_default_executor_for_platform,
    parse_default_executor, parse_default_executor_for_platform,
    seed_default_executors_for_platform,
};

const MACOS: &str = "macos";
const LINUX: &str = "linux";

/// Shipped executors that opt into Orbit's sandbox wrapper. `local-shell` is
/// deliberately excluded — it stays unsandboxed on every platform.
const SANDBOXED_SHIPPED: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "antigravity",
    "grok",
    "copilot",
    "cursor",
    "pi",
    "opencode",
];

fn base_def(name: &str, executor_type: ExecutorType) -> ExecutorDef {
    let now = Utc::now();
    ExecutorDef {
        name: name.to_string(),
        executor_type,
        command: Some("noop".to_string()),
        args: Vec::new(),
        stdout_format: None,
        model_pair_override: None,
        model_flag: None,
        timeout_seconds: None,
        env: Default::default(),
        sandbox: None,
        allow_fallback: false,
        created_at: Some(now),
        updated_at: Some(now),
    }
}

fn yaml_for(name: &str) -> &'static str {
    DEFAULT_EXECUTOR_FILES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, yaml)| *yaml)
        .unwrap_or_else(|| panic!("shipped executor asset `{name}` present"))
}

/// Minimal in-memory store so the seed path can be exercised without touching
/// the filesystem or the host platform.
#[derive(Default)]
struct InMemoryExecutorStore {
    defs: Mutex<Vec<ExecutorDef>>,
}

impl ExecutorDefStoreBackend for InMemoryExecutorStore {
    fn list_executor_defs(&self) -> Result<Vec<ExecutorDef>, OrbitError> {
        Ok(self.defs.lock().expect("lock").clone())
    }

    fn get_executor_def(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError> {
        Ok(self
            .defs
            .lock()
            .expect("lock")
            .iter()
            .find(|def| def.name == name)
            .cloned())
    }

    fn upsert_executor_def(&self, def: &ExecutorDef) -> Result<(), OrbitError> {
        let mut defs = self.defs.lock().expect("lock");
        if let Some(existing) = defs.iter_mut().find(|d| d.name == def.name) {
            *existing = def.clone();
        } else {
            defs.push(def.clone());
        }
        Ok(())
    }
}

const CLAUDE_YAML: &str = r#"schemaVersion: 2
kind: Executor
metadata:
  name: claude
spec:
  executor_type: direct_agent
  command: claude
  sandbox: macos-sandbox-exec
"#;

/// On the macOS seed path every sandbox-supporting shipped executor keeps the
/// `macos-sandbox-exec` declaration; `local-shell` stays unsandboxed. Driven
/// by an injected platform string so it runs deterministically on any CI host.
#[test]
fn parse_default_executor_installs_macos_sandbox_on_macos_path() {
    for name in SANDBOXED_SHIPPED {
        let def = parse_default_executor_for_platform(name, yaml_for(name), MACOS)
            .unwrap_or_else(|err| panic!("parse {name}: {err}"));
        assert_eq!(
            def.sandbox,
            Some(ExecutorSandboxKind::MacosSandboxExec),
            "{name} should keep macos-sandbox-exec on the macOS path"
        );
    }

    let local_shell =
        parse_default_executor_for_platform("local-shell", yaml_for("local-shell"), MACOS)
            .expect("parse local-shell");
    assert_eq!(
        local_shell.sandbox, None,
        "local-shell must stay unsandboxed on macOS"
    );
}

/// On Linux every shipped agent uses Bubblewrap while local-shell remains bare.
#[test]
fn parse_default_executor_installs_linux_bwrap_on_linux_path() {
    for name in SANDBOXED_SHIPPED {
        let def = parse_default_executor_for_platform(name, yaml_for(name), LINUX)
            .unwrap_or_else(|err| panic!("parse {name}: {err}"));
        assert_eq!(
            def.sandbox,
            Some(ExecutorSandboxKind::LinuxBwrap),
            "{name} must install linux-bwrap on the Linux path"
        );
    }
    let local_shell =
        parse_default_executor_for_platform("local-shell", yaml_for("local-shell"), LINUX)
            .expect("parse local-shell");
    assert_eq!(local_shell.sandbox, None);
}

/// The host-platform default entry point defers to the injected core: on the
/// running host, a shipped sandbox declaration survives iff the primitive
/// applies to this OS.
#[test]
fn parse_default_executor_selects_by_host_platform() {
    let def = parse_default_executor("claude", CLAUDE_YAML).expect("parse");
    let expected = match std::env::consts::OS {
        "macos" => Some(ExecutorSandboxKind::MacosSandboxExec),
        "linux" => Some(ExecutorSandboxKind::LinuxBwrap),
        _ => None,
    };
    assert_eq!(def.sandbox, expected);
}

/// Re-seeding must re-align a platform-mismatched sandbox left over from a
/// prior install: an upgrade on Linux clears a persisted `macos-sandbox-exec`
/// even though the executor type is unchanged.
#[test]
fn migrated_default_executor_realigns_platform_mismatched_sandbox_on_linux() {
    let mut existing = base_def("claude", ExecutorType::DirectAgent);
    existing.sandbox = Some(ExecutorSandboxKind::MacosSandboxExec);
    let seeded = parse_default_executor_for_platform("claude", CLAUDE_YAML, LINUX).expect("seed");

    let migrated = migrated_default_executor_for_platform(&existing, &seeded, LINUX)
        .expect("re-align should produce a migrated def");
    assert_eq!(migrated.sandbox, Some(ExecutorSandboxKind::LinuxBwrap));
    assert_eq!(migrated.executor_type, ExecutorType::DirectAgent);
}

/// On macOS the persisted sandbox is host-compatible; re-seeding must not touch
/// it and must not force a rewrite.
#[test]
fn migrated_default_executor_preserves_sandbox_on_macos() {
    let mut existing = base_def("claude", ExecutorType::DirectAgent);
    existing.sandbox = Some(ExecutorSandboxKind::MacosSandboxExec);
    let seeded = parse_default_executor_for_platform("claude", CLAUDE_YAML, MACOS).expect("seed");

    assert!(migrated_default_executor_for_platform(&existing, &seeded, MACOS).is_none());
}

/// The pre-ORB-10047 `AgentCli → DirectAgent` migration path must still fire so
/// upgrades from older on-disk executor defs continue to move forward,
/// regardless of platform.
#[test]
fn migrated_default_executor_still_migrates_agent_cli_to_direct_agent() {
    let existing = base_def("claude", ExecutorType::AgentCli);
    let seeded = base_def("claude", ExecutorType::DirectAgent);

    let migrated = migrated_default_executor(&existing, &seeded).expect("type migration");
    assert_eq!(migrated.executor_type, ExecutorType::DirectAgent);
    assert_eq!(migrated.sandbox, None);
}

#[test]
fn migrated_default_executor_returns_none_when_nothing_needs_migrating() {
    let existing = base_def("claude", ExecutorType::DirectAgent);
    let seeded = base_def("claude", ExecutorType::DirectAgent);
    assert!(migrated_default_executor(&existing, &seeded).is_none());
}

/// Seeding twice on the macOS path is idempotent and preserves the
/// `macos-sandbox-exec` value: the second pass creates nothing new and leaves
/// every sandbox-supporting default sandboxed.
#[test]
fn seed_default_executors_is_idempotent_on_macos_path() {
    let store = InMemoryExecutorStore::default();

    let first = seed_default_executors_for_platform(&store, false, MACOS).expect("first seed");
    assert_eq!(first, DEFAULT_EXECUTOR_FILES.len());

    let second = seed_default_executors_for_platform(&store, false, MACOS).expect("second seed");
    assert_eq!(second, 0, "re-seed must be a no-op on the macOS path");

    for name in SANDBOXED_SHIPPED {
        let def = store
            .get_executor_def(name)
            .expect("get")
            .unwrap_or_else(|| panic!("{name} seeded"));
        assert_eq!(
            def.sandbox,
            Some(ExecutorSandboxKind::MacosSandboxExec),
            "{name} sandbox must survive idempotent re-seed on macOS"
        );
    }
}

/// Seeding twice on Linux is idempotent and preserves linux-bwrap.
#[test]
fn seed_default_executors_is_idempotent_on_linux_path() {
    let store = InMemoryExecutorStore::default();

    let first = seed_default_executors_for_platform(&store, false, LINUX).expect("first seed");
    assert_eq!(first, DEFAULT_EXECUTOR_FILES.len());

    let second = seed_default_executors_for_platform(&store, false, LINUX).expect("second seed");
    assert_eq!(second, 0, "re-seed must be a no-op on the Linux path");

    for name in SANDBOXED_SHIPPED {
        let def = store
            .get_executor_def(name)
            .expect("get")
            .unwrap_or_else(|| panic!("{name} seeded"));
        assert_eq!(
            def.sandbox,
            Some(ExecutorSandboxKind::LinuxBwrap),
            "{name} must keep linux-bwrap across idempotent re-seed"
        );
    }
    assert_eq!(
        store
            .get_executor_def("local-shell")
            .expect("get")
            .expect("local-shell seeded")
            .sandbox,
        None
    );
}

/// A pre-ORB-10552 Linux install is upgraded to the native backend.
#[test]
fn seed_default_executors_heals_leftover_macos_sandbox_on_linux() {
    let store = InMemoryExecutorStore::default();
    let mut stale = base_def("claude", ExecutorType::DirectAgent);
    stale.sandbox = Some(ExecutorSandboxKind::MacosSandboxExec);
    store.upsert_executor_def(&stale).expect("seed stale def");

    seed_default_executors_for_platform(&store, false, LINUX).expect("seed");

    let healed = store
        .get_executor_def("claude")
        .expect("get")
        .expect("claude present");
    assert_eq!(
        healed.sandbox,
        Some(ExecutorSandboxKind::LinuxBwrap),
        "leftover macos-sandbox-exec must become linux-bwrap"
    );
}

#[test]
fn seed_default_executors_upgrades_old_unsandboxed_linux_default() {
    let store = InMemoryExecutorStore::default();
    let stale = base_def("claude", ExecutorType::DirectAgent);
    store
        .upsert_executor_def(&stale)
        .expect("seed old Linux def");

    seed_default_executors_for_platform(&store, false, LINUX).expect("seed");

    assert_eq!(
        store
            .get_executor_def("claude")
            .expect("get")
            .expect("claude present")
            .sandbox,
        Some(ExecutorSandboxKind::LinuxBwrap)
    );
}

#[test]
fn explicit_off_survives_repeated_seeding_and_legacy_executor_type_migration() {
    for platform in [LINUX, MACOS, "windows"] {
        let store = InMemoryExecutorStore::default();
        for name in SANDBOXED_SHIPPED {
            let mut def = base_def(name, ExecutorType::AgentCli);
            def.sandbox = Some(ExecutorSandboxKind::Off);
            store
                .upsert_executor_def(&def)
                .expect("install explicit off");
        }

        seed_default_executors_for_platform(&store, false, platform).expect("migrate type");
        assert_eq!(
            seed_default_executors_for_platform(&store, false, platform).expect("reseed"),
            0
        );
        for name in SANDBOXED_SHIPPED {
            let def = store
                .get_executor_def(name)
                .expect("load")
                .expect("present");
            assert_eq!(
                def.sandbox,
                Some(ExecutorSandboxKind::Off),
                "{name} on {platform}"
            );
            assert_eq!(def.executor_type, ExecutorType::DirectAgent);
        }

        // Explicit overwrite still means replacing operator customizations.
        seed_default_executors_for_platform(&store, true, platform).expect("overwrite");
        let expected = parse_default_executor_for_platform("codex", yaml_for("codex"), platform)
            .expect("shipped choice")
            .sandbox;
        assert_eq!(
            store
                .get_executor_def("codex")
                .expect("load")
                .expect("present")
                .sandbox,
            expected
        );
    }
}

#[test]
fn custom_executor_sandbox_choice_is_not_rewritten_by_seed_migration() {
    let store = InMemoryExecutorStore::default();
    let mut custom = base_def("custom", ExecutorType::DirectAgent);
    custom.sandbox = Some(ExecutorSandboxKind::MacosSandboxExec);
    store.upsert_executor_def(&custom).expect("seed custom");

    seed_default_executors_for_platform(&store, false, LINUX).expect("seed defaults");

    assert_eq!(
        store
            .get_executor_def("custom")
            .expect("get")
            .expect("custom preserved")
            .sandbox,
        Some(ExecutorSandboxKind::MacosSandboxExec)
    );
}

/// Shipped direct-agent defaults use crew-selected models at runtime. Their
/// legacy model-pair field remains readable for older/user-authored files but
/// must not be emitted by fresh assets.
#[test]
fn shipped_direct_agent_defaults_omit_legacy_model_pair_override() {
    for name in SANDBOXED_SHIPPED {
        let def = parse_default_executor_for_platform(name, yaml_for(name), LINUX)
            .unwrap_or_else(|err| panic!("parse {name}: {err}"));
        assert_eq!(
            def.model_pair_override(),
            None,
            "fresh {name} default must omit the legacy model pair override"
        );
        assert!(
            def.model_flag.is_some(),
            "fresh {name} default must retain crew-selected model flag behavior"
        );
    }
}

#[test]
fn seeding_adds_antigravity_without_rewriting_a_customized_gemini_executor() {
    let store = InMemoryExecutorStore::default();
    let mut custom = base_def("gemini", ExecutorType::DirectAgent);
    custom.command = Some("gemini".to_string());
    custom.args = vec![
        "--approval-mode".to_string(),
        "yolo".to_string(),
        "--allowed-mcp-server-names".to_string(),
        "orbit".to_string(),
        "-o".to_string(),
        "json".to_string(),
    ];
    custom.model_pair_override = Some(orbit_types::workflow::ModelPairOverride {
        strong: "custom-strong".to_string(),
        weak: "custom-weak".to_string(),
    });
    custom.model_flag = Some("-m".to_string());
    custom.sandbox = Some(ExecutorSandboxKind::LinuxBwrap);
    store
        .upsert_executor_def(&custom)
        .expect("seed custom gemini");

    seed_default_executors_for_platform(&store, false, LINUX).expect("seed defaults");

    let gemini = store
        .get_executor_def("gemini")
        .expect("get")
        .expect("gemini present");
    assert_eq!(gemini.command.as_deref(), Some("gemini"));
    assert_eq!(gemini.args, custom.args);
    assert_eq!(gemini.model_pair_override, custom.model_pair_override);
    assert_eq!(gemini.model_flag, custom.model_flag);

    let antigravity = store
        .get_executor_def("antigravity")
        .expect("get")
        .expect("antigravity seeded");
    assert_eq!(antigravity.command.as_deref(), Some("agy"));
    assert!(
        !antigravity.args.iter().any(|arg| arg == "--approval-mode"
            || arg == "-o"
            || arg == "--allowed-mcp-server-names")
    );
    assert!(
        antigravity
            .args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"])
    );
    assert_eq!(antigravity.model_pair_override, None);
}

#[test]
fn seeding_preserves_customized_legacy_model_pair_override() {
    let store = InMemoryExecutorStore::default();
    let mut custom = base_def("claude", ExecutorType::DirectAgent);
    custom.model_pair_override = Some(orbit_types::workflow::ModelPairOverride {
        strong: "custom-strong".to_string(),
        weak: "custom-weak".to_string(),
    });
    custom.model_flag = Some("--model".to_string());
    custom.sandbox = Some(ExecutorSandboxKind::LinuxBwrap);
    store.upsert_executor_def(&custom).expect("seed custom def");

    seed_default_executors_for_platform(&store, false, LINUX).expect("seed defaults");

    let persisted = store
        .get_executor_def("claude")
        .expect("get")
        .expect("claude present");
    assert_eq!(persisted.model_pair_override, custom.model_pair_override);
    assert_eq!(persisted.model_flag, custom.model_flag);
}

/// The shipped `local-shell` asset advertises only what the runtime supports:
/// the `local_shell` executor family that the deterministic `local_shell`
/// action dispatches, and no agent-shaped fields. [ORB-11294]
#[test]
fn shipped_local_shell_advertises_the_local_shell_executor_family() {
    let def = parse_default_executor_for_platform("local-shell", yaml_for("local-shell"), LINUX)
        .expect("parse local-shell");

    assert_eq!(def.executor_type, ExecutorType::LocalShell);
    assert_eq!(def.command, None);
    assert!(def.args.is_empty());
    assert_eq!(def.model_pair_override, None);
    assert_eq!(def.model_flag, None);
    assert_eq!(def.stdout_format, None);
}

/// A definition installed before [ORB-11294] persists `executor_type:
/// cli_command`. Re-seeding must load it, keep every customization the operator
/// made, and not rewrite it.
#[test]
fn seeding_preserves_a_customized_legacy_local_shell_definition() {
    let legacy_yaml = r#"schemaVersion: 2
kind: Executor
metadata:
  name: local-shell
spec:
  executor_type: cli_command
  command: /bin/bash
  args: ["-lc"]
  timeout_seconds: 90
"#;
    let legacy = parse_default_executor_for_platform("local-shell", legacy_yaml, LINUX)
        .expect("legacy cli_command definition still loads");
    assert_eq!(legacy.executor_type, ExecutorType::LocalShell);
    assert_eq!(legacy.command.as_deref(), Some("/bin/bash"));

    let store = InMemoryExecutorStore::default();
    store.upsert_executor_def(&legacy).expect("install legacy");
    seed_default_executors_for_platform(&store, false, LINUX).expect("seed");

    let after = store
        .get_executor_def("local-shell")
        .expect("get")
        .expect("local-shell present");
    assert_eq!(
        after, legacy,
        "re-seeding must not overwrite an operator's local-shell definition"
    );
}
