//! Two writers of one `config.toml` never erase each other's successful edit
//! [ORB-15555]: a store opened before another writer saved refuses to save,
//! and writers that run open-to-save under the write lock serialize, each
//! validating and persisting the version that includes the other's edit.
#![allow(clippy::expect_used, missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use orbit_common::OrbitError;
use orbit_config::{
    ConfigRoots, ConfigScope, ConfigStore, ResolvedConfig, load_workspace_plugin_enablement,
    plugin_enablement_key,
};

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
fn a_store_opened_before_another_save_refuses_to_replace_it() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    let global = roots.global.path();

    let mut first = open(&path);
    let mut second = open(&path);
    set_key(&mut first, global, "scoring.enabled", "false");
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
    assert!(!config.scoring_enabled, "first edit survives");
    assert_eq!(config.workflow_base_branch, "main");

    // Reapplied to the current file, the second edit keeps the first.
    let mut reopened = open(&path);
    set_key(&mut reopened, global, "workflow.base_branch", "agent-main");
    reopened.save().expect("reapplied save lands");
    let config = roots.load();
    assert!(!config.scoring_enabled, "first edit still holds");
    assert_eq!(config.workflow_base_branch, "agent-main");
}

#[test]
fn a_store_saves_again_after_its_own_save() {
    let roots = Roots::new();
    let path = roots.workspace_config();
    let global = roots.global.path();

    let mut store = open(&path);
    set_key(&mut store, global, "scoring.enabled", "false");
    store.save().expect("first save");
    set_key(&mut store, global, "workflow.base_branch", "agent-main");
    store
        .save()
        .expect("its own earlier save is not someone else's edit");

    let config = roots.load();
    assert!(!config.scoring_enabled);
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
    set_key(&mut config_writer, global, "scoring.enabled", "false");
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
    assert!(config.scoring_enabled, "the refused edit never landed");
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
        set_key(&mut store, &global, "scoring.enabled", "false");

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
    assert!(!config.scoring_enabled, "first edit survives");
    assert_eq!(config.workflow_base_branch, "agent-main");
    assert_eq!(roots.toggles().get("alpha"), Some(&false));
}
