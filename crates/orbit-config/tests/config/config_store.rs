//  Config path/value admission and write conflict behavior through ConfigStore.
//  Two writers of one `config.toml` never erase each other's successful edit
//  [ORB-15555]: a store opened before another writer saved refuses to save,
//  and writers that run open-to-save under the write lock serialize, each
//  validating and persisting the version that includes the other's edit.

struct Roots {
    global: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

impl Roots {
    fn new() -> Self {
        let roots = Self {
            global: tempfile::tempdir().expect("global tempdir"),
            workspace: tempfile::tempdir().expect("workspace tempdir"),
        };
        std::fs::write(roots.workspace_config(), "").expect("empty workspace config");
        roots
    }

    fn workspace_config(&self) -> PathBuf {
        self.workspace.path().join("config.toml")
    }

    fn config_roots(&self) -> ConfigRoots {
        ConfigRoots::new(self.global.path(), self.workspace.path())
    }

    fn load(&self) -> ResolvedConfig {
        ResolvedConfig::load(&self.config_roots()).expect("final config loads")
    }

    fn toggles(&self) -> std::collections::BTreeMap<String, bool> {
        load_workspace_plugin_enablement(&self.config_roots()).expect("toggles load")
    }
}

fn open(path: &Path) -> ConfigStore {
    ConfigStore::open(ConfigScope::Workspace, path).expect("open workspace store")
}

fn set_key(store: &mut ConfigStore, global_root: &Path, key: &str, value: &str) {
    store.set_value(key, value).expect("set");
    store
        .validate_workspace_for_set(key, global_root)
        .expect("validate");
}

fn set_toggle(store: &mut ConfigStore, global_root: &Path, plugin: &str, enabled: bool) {
    store
        .set_document_value(&plugin_enablement_key(plugin), &enabled.to_string())
        .expect("set toggle");
    store
        .validate_workspace_with_global(global_root)
        .expect("validate toggle");
}

#[test]
fn global_only_workspace_key_refusal_points_to_global_config_and_leaves_workspace_unchanged() {
    let roots = Roots::new();
    let workspace_path = roots.workspace_config();
    let global_path = roots.global.path().join("config.toml");
    let before = std::fs::read(&workspace_path).expect("read initial workspace config");
    let mut store = ConfigStore::open_for_workspace_set(
        &workspace_path,
        &global_path,
        WorkspaceInitMode::RequireExisting,
    )
    .expect("open existing workspace config");

    let refusal = store
        .set_value("machine.worker_cpu_quota", "16")
        .expect_err("a global-only key must be refused in the workspace config")
        .to_string();
    let global_path_text = global_path.to_string_lossy();
    let workspace_path_text = workspace_path.to_string_lossy();
    assert!(
        refusal.contains(global_path_text.as_ref()),
        "the refusal must identify the config file selected by --global: {refusal}"
    );
    assert!(
        !refusal.contains(workspace_path_text.as_ref()),
        "the refusal must not identify the workspace file as the machine config: {refusal}"
    );
    assert_eq!(
        std::fs::read(&workspace_path).expect("read workspace config after refusal"),
        before,
        "refusing a global-only key leaves workspace config bytes unchanged"
    );
}

#[test]
fn a_store_opened_before_another_save_refuses_to_replace_it() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    let global = roots.global.path();

    let mut first = open(&path);
    let mut second = open(&path);
    set_key(&mut first, global, "workflow.auto_ship", "true");
    set_key(&mut second, global, "workflow.base_branch", "agent-main");

    first.save().expect("first save lands");
    let before = std::fs::read_to_string(&path).expect("read after first save");
    let refused = second
        .save()
        .expect_err("a save over content written after this store opened must be refused");
    assert!(
        matches!(refused, OrbitError::Store(_)),
        "a stale save is a store conflict, not a validation or I/O failure: {refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read after refusal"),
        before,
        "the refused save leaves the first writer's file untouched"
    );

    let config = roots.load();
    assert!(config.workflow_auto_ship, "first edit survives");
    assert_eq!(config.workflow_base_branch, "main");

    // Reapplied to the current file, the second edit keeps the first.
    let mut reopened = open(&path);
    set_key(&mut reopened, global, "workflow.base_branch", "agent-main");
    reopened.save().expect("reapplied save lands");
    let config = roots.load();
    assert!(config.workflow_auto_ship, "first edit still holds");
    assert_eq!(config.workflow_base_branch, "agent-main");
}

#[test]
fn a_store_saves_again_after_its_own_save() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    let global = roots.global.path();

    let mut store = open(&path);
    set_key(&mut store, global, "workflow.auto_ship", "true");
    store.save().expect("first save");
    set_key(&mut store, global, "workflow.base_branch", "agent-main");
    store
        .save()
        .expect("its own earlier save is not someone else's edit");

    let config = roots.load();
    assert!(config.workflow_auto_ship);
    assert_eq!(config.workflow_base_branch, "agent-main");
}

#[test]
fn plugin_toggle_and_config_writers_refuse_each_others_stale_saves() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    let global = roots.global.path();

    // A config edit opened before a toggle write is refused...
    let mut config_writer = open(&path);
    let mut toggle_writer = open(&path);
    set_key(&mut config_writer, global, "workflow.auto_ship", "true");
    set_toggle(&mut toggle_writer, global, "alpha", false);
    toggle_writer.save().expect("toggle save lands");
    config_writer
        .save()
        .expect_err("a config save must not erase the toggle written after it opened");
    assert_eq!(roots.toggles().get("alpha"), Some(&false));

    // ...and a toggle edit opened before a config write is refused too.
    let mut config_writer = open(&path);
    let mut toggle_writer = open(&path);
    set_key(
        &mut config_writer,
        global,
        "workflow.base_branch",
        "agent-main",
    );
    set_toggle(&mut toggle_writer, global, "beta", false);
    config_writer.save().expect("config save lands");
    toggle_writer
        .save()
        .expect_err("a toggle save must not erase the config edit written after it opened");

    let config = roots.load();
    assert_eq!(config.workflow_base_branch, "agent-main");
    assert!(!config.workflow_auto_ship, "the refused edit never landed");
    assert_eq!(roots.toggles().get("alpha"), Some(&false));
    assert_eq!(roots.toggles().get("beta"), None);
}

#[test]
fn writers_under_the_write_lock_serialize_and_keep_both_edits() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    let global = roots.global.path().to_path_buf();

    // The first writer holds the lock from open to save. The second starts
    // only once the first has opened and edited, so without the lock it would
    // read the same pre-edit version and its save would erase the first edit.
    let (started_tx, started_rx) = mpsc::channel();
    let second = ConfigStore::with_write_lock(&path, || {
        let mut store = open(&path);
        set_key(&mut store, &global, "workflow.auto_ship", "true");

        let second = std::thread::spawn({
            let path = path.clone();
            let global = global.clone();
            move || {
                started_tx.send(()).expect("signal start");
                ConfigStore::with_write_lock(&path, || {
                    let mut store = open(&path);
                    set_toggle(&mut store, &global, "alpha", false);
                    set_key(&mut store, &global, "workflow.base_branch", "agent-main");
                    store.save()
                })
            }
        });
        started_rx.recv().expect("second writer started");
        store.save()?;
        Ok(second)
    })
    .expect("first writer saves");

    second
        .join()
        .expect("second writer thread")
        .expect("second writer reads the first's save and lands");

    let config = roots.load();
    assert!(config.workflow_auto_ship, "first edit survives");
    assert_eq!(config.workflow_base_branch, "agent-main");
    assert_eq!(roots.toggles().get("alpha"), Some(&false));
}

mod admission {
    use orbit_common::OrbitError;
    use orbit_config::{ConfigScope, ConfigStore};
    use std::fs;
    use tempfile::tempdir;

    fn config_path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("config.toml")
    }

    #[test]
    fn set_rejects_invalid_value_and_leaves_file_byte_identical() {
        let dir = tempdir().expect("tempdir");
        let path = config_path(dir.path());
        let original = "[execution.codex]\nsandbox = \"workspace-write\"\n";
        fs::write(&path, original).expect("write config");

        let mut store = ConfigStore::open(ConfigScope::Workspace, &path).expect("open store");
        store
            .set_value("execution.codex.sandbox", "not-a-real-mode")
            .expect("set_value only mutates the in-memory document");

        let error = store
            .validate()
            .expect_err("invalid sandbox mode must fail validation");
        assert!(matches!(error, OrbitError::InvalidInput(_)), "{error}");

        // `set_value`/`validate` never touch disk; only `save` (which the
        // caller must not call after a failed `validate`) does. Confirm the
        // file on disk is untouched, byte for byte.
        let after = fs::read(&path).expect("read config after failed validate");
        assert_eq!(after, original.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_a_symlinked_config_file() {
        let dir = tempdir().expect("tempdir");
        let real = dir.path().join("real.toml");
        fs::write(&real, "[workflow]\nbase_branch = \"agent-main\"\n").expect("write real config");
        let link = config_path(dir.path());
        std::os::unix::fs::symlink(&real, &link).expect("symlink config leaf");

        let error = match ConfigStore::open(ConfigScope::Workspace, &link) {
            Err(error) => error,
            Ok(_) => panic!("a symlinked config file must be refused"),
        };
        assert!(
            error
                .to_string()
                .contains("config path must not be a symlink"),
            "{error}"
        );
    }
}

#[test]
fn review_timing_writes_remain_readable_by_previous_readers() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    for (alias, canonical) in [
        ("off", "off"),
        ("none", "off"),
        ("after_landing", "off"),
        ("after-landing", "off"),
        ("before_pr", "before_pr"),
        ("before-pr", "before_pr"),
        ("before_landing", "before_landing"),
        ("before-landing", "before_landing"),
    ] {
        // Cover deliberate CLI/subsystem writes and an unrelated doctor's
        // edit to a file already carrying an alias.
        for existing in [false, true] {
            let original = format!("[review]\n# operator note\ntiming = {alias:?} # keep this\n");
            std::fs::write(&path, if existing { &original } else { "" })
                .expect("review timing write round trip");
            let mut store = open(&path);
            if !existing {
                store
                    .set_value("review.timing", &format!("{alias:?}"))
                    .expect("review timing write round trip");
            }
            store
                .set_document_value("review.minutes", "45")
                .expect("review timing write round trip");
            store
                .validate_workspace_with_global(roots.global.path())
                .expect("review timing write round trip");
            store.save().expect("review timing write round trip");
            let text = std::fs::read_to_string(&path).expect("review timing write round trip");
            let document: toml::Value =
                toml::from_str(&text).expect("review timing write round trip");
            assert_eq!(
                document["review"]["timing"].as_str(),
                Some(canonical),
                "{alias}"
            );
            if existing {
                assert!(text.contains("# operator note") && text.contains("# keep this"));
            }
            assert_eq!(roots.load().snapshot.review_timing, canonical);
            let saved = std::fs::read(&path).expect("review timing write round trip");
            store.save().expect("review timing write round trip");
            assert_eq!(
                std::fs::read(&path).expect("review timing write round trip"),
                saved,
                "idempotent save"
            );
        }
    }
}
