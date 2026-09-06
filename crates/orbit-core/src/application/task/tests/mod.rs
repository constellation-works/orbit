#![allow(missing_docs)]

mod add;
mod contention;
mod params;
mod paths;
mod records;
mod update;

use crate::OrbitRuntime;
use tempfile::tempdir;

pub(super) fn test_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[workflow]
default_crew = "implementer"

[crews.implementer]
model = "implementer-model"
provider = "codex"
backend = "cli"

[crews.orchestration]
model = "orchestration-model"
provider = "codex"
backend = "cli"
"#,
    )
    .expect("write crew config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime)
}

mod listing;
