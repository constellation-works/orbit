use std::fs;

use tempfile::tempdir;

use crate::OrbitRuntime;

#[test]
fn preserves_comments_and_unrelated_tables_when_adding_a_docs_root() {
    let root = tempdir().expect("create tempdir");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    let docs_root = repo_root.join("guides");
    let global_root = root.path().join("global");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    fs::create_dir_all(&docs_root).expect("create docs root");
    fs::create_dir_all(&global_root).expect("create global root");

    let prefix = "# operator note\n[workflow]\nbase_branch = \"agent-main\"\n\n";
    let suffix = "\n[search]\nlimit = 20\n";
    fs::write(
        workspace_root.join("config.toml"),
        format!("{prefix}[docs]\nroots = [\"docs/\"]{suffix}"),
    )
    .expect("write config");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");

    runtime.add_docs_root("guides").expect("add docs root");

    let saved = fs::read_to_string(workspace_root.join("config.toml")).expect("read config");
    assert!(saved.starts_with(prefix), "{saved}");
    assert!(saved.ends_with(suffix), "{saved}");
    assert!(
        saved.contains("roots = [\"docs/\", \"guides/\"]"),
        "{saved}"
    );
}

#[test]
fn adds_docs_root_to_workspace_config_without_touching_global_config() {
    let root = tempdir().expect("create tempdir");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    let docs_root = repo_root.join("guides");
    let global_root = root.path().join("global");
    let global_config = global_root.join("config.toml");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    fs::create_dir_all(&docs_root).expect("create docs root");
    fs::create_dir_all(&global_root).expect("create global root");
    let global_content = "# global-only\n[workflow]\nbase_branch = \"agent-main\"\n";
    fs::write(&global_config, global_content).expect("write global config");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");

    runtime.add_docs_root("guides").expect("add docs root");

    assert_eq!(
        fs::read_to_string(&global_config).expect("read global config"),
        global_content
    );
    assert!(workspace_root.join("config.toml").exists());
    assert_eq!(
        runtime.docs_roots().expect("read docs roots"),
        vec![
            super::super::DocsRoot::new("docs/"),
            super::super::DocsRoot::new("guides/")
        ]
    );
}
