use orbit_common::OrbitError;
use orbit_store::{ExecutorDefDir, FileStore};
use orbit_types::timestamp::UtcTimestamp;
use orbit_types::workflow::{ExecutorDef, ExecutorSandboxKind, ExecutorType};

use crate::application::executor::seed_default_executors_for_platform;

const LINUX: &str = "linux";

/// Executor store over a temporary directory, so the seed path runs against
/// the real file repository without touching the host's executors.
struct TempExecutorStore {
    _dir: tempfile::TempDir,
    store: FileStore<ExecutorDefDir>,
}

impl Default for TempExecutorStore {
    fn default() -> Self {
        let dir = tempfile::tempdir().expect("executor tempdir");
        let store = FileStore::new(ExecutorDefDir::new(dir.path().join("executors")));
        Self { _dir: dir, store }
    }
}

impl std::ops::Deref for TempExecutorStore {
    type Target = FileStore<ExecutorDefDir>;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

impl TempExecutorStore {
    fn list_executor_defs(&self) -> Result<Vec<ExecutorDef>, OrbitError> {
        self.store.read().executor_def().list()
    }

    fn get_executor_def(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError> {
        self.store.read().executor_def().get(name)
    }

    fn upsert_executor_def(&self, def: &ExecutorDef) -> Result<(), OrbitError> {
        self.store.write(|tx| tx.executor_def().put(def))
    }
}

fn base_def(name: &str, executor_type: ExecutorType) -> ExecutorDef {
    let now = UtcTimestamp::try_from(std::time::SystemTime::now()).expect("executor timestamp");
    ExecutorDef {
        name: name.to_string(),
        executor_type,
        command: Some("noop".to_string()),
        args: Vec::new(),
        inspection_args: None,
        image_input: None,
        stdout_format: None,
        model_pair_override: None,
        model_flag: None,
        timeout_seconds: None,
        auth_probe: None,
        env: Default::default(),
        sandbox: None,
        allow_fallback: false,
        host_os: None,
        created_at: Some(now),
        updated_at: Some(now),
    }
}

/// A pre-ORB-10552 Linux install is upgraded to the native backend.
#[test]
fn seed_default_executors_heals_leftover_macos_sandbox_on_linux() {
    let store = TempExecutorStore::default();
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
    assert_eq!(healed.executor_type, ExecutorType::DirectAgent);
}

/// [ORB-14342] The shipped local-shell default has no sandbox. Re-seeding an
/// operator-sandboxed install must preserve its fail-closed setting, even on
/// a host where that backend is unavailable.
#[test]
fn seed_default_executors_preserves_local_shell_sandbox_on_mismatched_platform() {
    for (target_os, sandbox) in [
        (LINUX, ExecutorSandboxKind::MacosSandboxExec),
        ("macos", ExecutorSandboxKind::LinuxBwrap),
        ("windows", ExecutorSandboxKind::MacosSandboxExec),
        ("windows", ExecutorSandboxKind::LinuxBwrap),
    ] {
        let store = TempExecutorStore::default();
        let mut installed = base_def("local-shell", ExecutorType::LocalShell);
        installed.sandbox = Some(sandbox);
        store
            .upsert_executor_def(&installed)
            .expect("seed operator-sandboxed local-shell");

        seed_default_executors_for_platform(&store, false, target_os).expect("seed defaults");

        assert_eq!(
            store
                .get_executor_def("local-shell")
                .expect("get")
                .expect("local-shell present")
                .sandbox,
            Some(sandbox),
            "[ORB-14342] reseeding must not remove the operator's {sandbox} sandbox on {target_os}"
        );
    }
}

#[test]
fn seed_default_executors_upgrades_old_unsandboxed_linux_default() {
    let store = TempExecutorStore::default();
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
    let store = TempExecutorStore::default();
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
    let store = TempExecutorStore::default();
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

/// Muse runs on macOS and Linux: a fresh seed on either platform carries
/// no `host_os` restriction, and the shipped sandbox marker is translated
/// per host (`linux-bwrap` on Linux, `macos-sandbox-exec` on macOS).
#[test]
fn seed_shipped_muse_is_unrestricted_with_platform_sandbox() {
    for (target_os, sandbox) in [
        (LINUX, ExecutorSandboxKind::LinuxBwrap),
        ("macos", ExecutorSandboxKind::MacosSandboxExec),
    ] {
        let store = TempExecutorStore::default();
        seed_default_executors_for_platform(&store, false, target_os).expect("seed defaults");
        let muse = store
            .get_executor_def("muse")
            .expect("get")
            .expect("muse present");
        assert_eq!(muse.host_os, None, "muse on {target_os}");
        assert_eq!(muse.sandbox, Some(sandbox), "muse on {target_os}");
    }
}

/// [ORB-13857] macOS keeps sandbox-exec on a fresh seed and, as before, does
/// not rewrite an installed default that carries no sandbox.
#[test]
fn seed_on_macos_keeps_sandbox_exec_and_existing_choices() {
    let store = TempExecutorStore::default();
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

/// [ORB-18096] Re-seeding retires stale shipped executors for removed lanes.
#[test]
fn seed_retires_stale_retired_provider_executors() {
    let store = TempExecutorStore::default();
    for name in ["cursor", "pi", "opencode"] {
        store
            .upsert_executor_def(&base_def(name, ExecutorType::DirectAgent))
            .expect("seed stale retired executor");
    }
    store
        .upsert_executor_def(&base_def("custom", ExecutorType::DirectAgent))
        .expect("seed custom");
    seed_default_executors_for_platform(&store, false, LINUX).expect("seed");
    for name in ["cursor", "pi", "opencode"] {
        assert!(
            store.get_executor_def(name).expect("get").is_none(),
            "stale {name} executor must be retired"
        );
    }
    assert!(store.get_executor_def("custom").expect("get").is_some());
    assert!(store.get_executor_def("claude").expect("get").is_some());
}
