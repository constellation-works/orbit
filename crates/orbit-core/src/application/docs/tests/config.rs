//! Config parsing (roots + search weights) tests migrated for ORB-00250.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use tempfile::tempdir;

use super::super::config::{
    DocsRoot, parse_docs_roots_from_config_toml, parse_docs_search_config_from_config_toml,
    parse_task_context_docs_roots_from_config_toml, read_docs_roots_from_config_path,
};

fn root_paths(roots: &[DocsRoot]) -> Vec<&str> {
    roots.iter().map(|root| root.path.as_str()).collect()
}

#[test]
fn config_roots_default_and_parse_explicit_values() {
    assert_eq!(
        root_paths(&parse_docs_roots_from_config_toml("").unwrap()),
        vec!["docs/"]
    );
    let parsed =
        parse_docs_roots_from_config_toml("[docs]\nroots = [\"docs/\", \"apps/*/docs/\"]\n")
            .unwrap();
    assert_eq!(root_paths(&parsed), vec!["docs/", "apps/*/docs/"]);
    assert!(parsed.iter().all(|root| root.respect_gitignore));
}

#[test]
fn config_roots_parse_explicit_override_table_entries() {
    let parsed = parse_docs_roots_from_config_toml(
        "[docs]\nroots = [\"docs/\", { path = \"external/docs/\", respect_gitignore = false }]\n",
    )
    .unwrap();
    assert_eq!(root_paths(&parsed), vec!["docs/", "external/docs/"]);
    assert!(parsed[0].respect_gitignore);
    assert!(!parsed[1].respect_gitignore);
}

#[test]
fn config_roots_table_entry_defaults_to_respecting_gitignore() {
    let parsed =
        parse_docs_roots_from_config_toml("[docs]\nroots = [{ path = \"docs/\" }]\n").unwrap();
    assert_eq!(root_paths(&parsed), vec!["docs/"]);
    assert!(parsed[0].respect_gitignore);
}

#[test]
fn docs_search_config_defaults_and_clamps_semantic_weight() {
    assert_eq!(
        parse_docs_search_config_from_config_toml("")
            .unwrap()
            .semantic_weight,
        0.5
    );
    assert_eq!(
        parse_docs_search_config_from_config_toml("[docs.search]\nsemantic_weight = 0.7\n")
            .unwrap()
            .semantic_weight,
        0.7
    );
    assert_eq!(
        parse_docs_search_config_from_config_toml("[docs.search]\nsemantic_weight = -1.0\n")
            .unwrap()
            .semantic_weight,
        0.0
    );
    assert_eq!(
        parse_docs_search_config_from_config_toml("[docs.search]\nsemantic_weight = 2.0\n")
            .unwrap()
            .semantic_weight,
        1.0
    );
}

#[test]
fn task_context_docs_roots_skip_explicit_empty_or_unset_roots() {
    assert_eq!(
        parse_task_context_docs_roots_from_config_toml("[docs]\n")
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        parse_task_context_docs_roots_from_config_toml("[docs]\nroots = []\n")
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        root_paths(&parse_task_context_docs_roots_from_config_toml("").unwrap()),
        vec!["docs/"]
    );
}

#[test]
fn config_reader_accepts_the_workspace_config_file() {
    let root = tempdir().expect("create tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[docs]\nroots = [\"guides/\"]\n").expect("write config");

    let roots = read_docs_roots_from_config_path(&path).expect("read config");

    assert_eq!(root_paths(&roots), vec!["guides/"]);
}

#[test]
fn config_reader_rejects_an_existing_non_config_file() {
    let root = tempdir().expect("create tempdir");
    let path = root.path().join("secrets.txt");
    fs::write(&path, "[docs]\nroots = [\"guides/\"]\n").expect("write fixture");

    let error = read_docs_roots_from_config_path(&path).expect_err("reject non-config path");

    assert!(error.to_string().contains("config.toml"));
}

#[cfg(unix)]
#[test]
fn config_reader_rejects_a_config_symlink_to_another_file() {
    let root = tempdir().expect("create tempdir");
    let target = root.path().join("secrets.toml");
    let path = root.path().join("config.toml");
    fs::write(&target, "[docs]\nroots = [\"guides/\"]\n").expect("write fixture");
    symlink(&target, &path).expect("create config symlink");

    let error = read_docs_roots_from_config_path(&path).expect_err("reject config symlink");

    assert!(error.to_string().contains("config.toml"));
}
