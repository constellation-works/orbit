use std::sync::Mutex;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::ExecutorDefStoreBackend;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

use crate::application::executor::{
    migrated_default_executor_for_platform, parse_default_executor_for_platform,
    seed_default_executors_for_platform,
};

const LINUX: &str = "linux";

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

/// [ORB-13857] Where no sandbox backend exists, shipped agent executors keep
/// their declared kind — on a fresh seed and on an install whose earlier seed
/// dropped it — so dispatch refuses them instead of spawning bare. Explicit
/// `off` and `local-shell` are left as they are.
#[test]
fn seed_without_a_host_backend_keeps_the_declared_sandbox() {
    let store = InMemoryExecutorStore::default();
    store
        .upsert_executor_def(&base_def("claude", ExecutorType::DirectAgent))
        .expect("seed dropped-sandbox install");
    let mut off = base_def("gemini", ExecutorType::DirectAgent);
    off.sandbox = Some(ExecutorSandboxKind::Off);
    store.upsert_executor_def(&off).expect("seed operator off");

    seed_default_executors_for_platform(&store, false, "windows").expect("seed");

    for def in store.list_executor_defs().expect("list") {
        let expected = match def.name.as_str() {
            "local-shell" => None,
            "gemini" => Some(ExecutorSandboxKind::Off),
            _ => Some(ExecutorSandboxKind::MacosSandboxExec),
        };
        assert_eq!(def.sandbox, expected, "{} on windows", def.name);
    }
}

/// [ORB-13857] macOS keeps sandbox-exec on a fresh seed and, as before, does
/// not rewrite an installed default that carries no sandbox.
#[test]
fn seed_on_macos_keeps_sandbox_exec_and_existing_choices() {
    let store = InMemoryExecutorStore::default();
    store
        .upsert_executor_def(&base_def("claude", ExecutorType::DirectAgent))
        .expect("seed installed default");

    seed_default_executors_for_platform(&store, false, "macos").expect("seed");

    for def in store.list_executor_defs().expect("list") {
        let expected = match def.name.as_str() {
            "local-shell" | "claude" => None,
            _ => Some(ExecutorSandboxKind::MacosSandboxExec),
        };
        assert_eq!(def.sandbox, expected, "{} on macos", def.name);
    }
}
