//! Fixture state built once per test process and copied into each row.
//!
//! A [`TreeTemplate`] holds a directory tree in memory, so a template leaves
//! nothing on disk once it is built. A row writes its own copy and never
//! shares a file or handle with the template or another row. The store
//! template is a migrated global `orbit.db`: a fixture seeds each fresh global
//! root with it and skips the schema migrations that dominate a first open,
//! while the runtime still bootstraps that root itself. The control table
//! below proves one fixture's writes reach neither another fixture nor a
//! later row.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use orbit_core::OrbitRuntime;
use orbit_core::application::task::TaskAddParams;

const STORE: &str = "orbit.db";

/// Every directory and file under a root, by path relative to that root.
pub(crate) struct TreeTemplate {
    dirs: Vec<PathBuf>,
    files: Vec<(PathBuf, Vec<u8>)>,
}

impl TreeTemplate {
    /// Read `root` into memory. Templates hold plain directories and files.
    pub(crate) fn capture(root: &Path) -> Self {
        let mut template = Self {
            dirs: Vec::new(),
            files: Vec::new(),
        };
        template.read(root, Path::new(""));
        template
    }

    fn read(&mut self, root: &Path, relative: &Path) {
        let mut entries = std::fs::read_dir(root.join(relative))
            .expect("template dir")
            .map(|entry| entry.expect("template entry"))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = relative.join(entry.file_name());
            let kind = entry.file_type().expect("template entry type");
            if kind.is_dir() {
                self.dirs.push(path.clone());
                self.read(root, &path);
            } else {
                assert!(kind.is_file(), "template entry {path:?} is not a file");
                let bytes = std::fs::read(entry.path()).expect("template file");
                self.files.push((path, bytes));
            }
        }
    }

    /// Write a private copy of the tree under `root`.
    pub(crate) fn write(&self, root: &Path) {
        std::fs::create_dir_all(root).expect("template copy root");
        for dir in &self.dirs {
            std::fs::create_dir_all(root.join(dir)).expect("template copy dir");
        }
        for (path, bytes) in &self.files {
            std::fs::write(root.join(path), bytes).expect("template copy file");
        }
    }
}

/// The store a fresh open migrates, read back after its last connection
/// closed. A WAL or journal file left beside it would make a byte copy
/// unsafe, so the template refuses one instead of copying it.
fn store() -> &'static TreeTemplate {
    static TEMPLATE: OnceLock<TreeTemplate> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        let dir = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
            .expect("store template dir");
        drop(
            orbit_store::Store::open(&dir.path().join(STORE)).expect("migrate the template store"),
        );
        let template = TreeTemplate::capture(dir.path());
        assert!(
            template.dirs.is_empty()
                && template
                    .files
                    .iter()
                    .map(|(path, _)| path.as_path())
                    .eq([Path::new(STORE)]),
            "a closed template store must leave only its database file to copy"
        );
        template
    })
}

/// Give `global` a migrated store unless it already holds one. Call it only
/// before a fixture first opens that root; a reopen keeps the row's store.
pub(crate) fn seed_store(global: &Path) {
    if !global.join(STORE).exists() {
        store().write(global);
    }
}

/// Open a fresh workspace under `root`, seeded from the template when `seeded`.
fn open(root: &Path, seeded: bool) -> OrbitRuntime {
    let global = root.join("global");
    let workspace = root.join("repo/.orbit");
    std::fs::create_dir_all(&workspace).unwrap();
    if seeded {
        seed_store(&global);
    }
    OrbitRuntime::from_roots(&global, &workspace).unwrap()
}

fn add_task(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

/// A templated store starts as a freshly migrated one does, and a task written
/// into it stays in that fixture.
fn a_templated_store_is_a_private_fresh_store() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let fresh = open(&root.path().join("fresh"), false);
    let writer = open(&root.path().join("writer"), true);
    let sibling = open(&root.path().join("sibling"), true);
    assert_eq!(
        add_task(&writer, "Written by one fixture"),
        add_task(&fresh, "Written by an unseeded fixture"),
        "a templated store allocates the first task id a migrated store does"
    );
    assert_eq!(writer.list_tasks().unwrap().len(), 1);
    assert!(
        sibling.list_tasks().unwrap().is_empty(),
        "another fixture's templated store must not see this fixture's task"
    );
}

/// Runs after the writing row in the same child process, so it observes any
/// write that reached the shared template instead of the row's copy.
fn a_later_row_starts_from_an_unwritten_template() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let runtime = open(root.path(), true);
    assert!(
        runtime.list_tasks().unwrap().is_empty(),
        "an earlier row's task reached the shared store template"
    );
}

crate::scenario_table!(
    store_template_isolation_scenarios,
    a_templated_store_is_a_private_fresh_store,
    a_later_row_starts_from_an_unwritten_template,
);
