//! Config parsing (roots + search weights) tests migrated for ORB-00250.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;

use tempfile::tempdir;

#[cfg(unix)]
use super::super::config::docs_config_test_hook::{self, Phase};
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
fn config_reader_preserves_defaults_when_config_is_missing() {
    let root = tempdir().expect("create tempdir");

    let roots = read_docs_roots_from_config_path(&root.path().join("config.toml"))
        .expect("missing config uses defaults");

    assert_eq!(root_paths(&roots), vec!["docs/"]);
}

#[test]
fn config_reader_rejects_a_non_config_file_name_even_when_missing() {
    let root = tempdir().expect("create tempdir");
    let path = root.path().join("secrets.txt");

    let error = read_docs_roots_from_config_path(&path).expect_err("reject non-config path");

    assert!(
        error.to_string().contains("must name config.toml"),
        "{error}"
    );
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

    assert!(
        error.to_string().contains("must not be a symlink"),
        "{error}"
    );
    assert!(error.to_string().contains("secrets.toml"), "{error}");
}

#[cfg(unix)]
#[test]
fn config_reader_rejects_a_config_symlink_to_same_named_external_file() {
    let root = tempdir().expect("create tempdir");
    let outside = tempdir().expect("create outside tempdir");
    let target = outside.path().join("config.toml");
    let path = root.path().join("config.toml");
    fs::write(&target, "[docs]\nroots = [\"external/\"]\n").expect("write fixture");
    symlink(&target, &path).expect("create config symlink");

    let error = read_docs_roots_from_config_path(&path).expect_err("reject config symlink");
    let diagnostic = error.to_string();

    assert!(diagnostic.contains("must not be a symlink"), "{diagnostic}");
    assert!(
        diagnostic.contains(&target.to_string_lossy().into_owned()),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("regular config.toml"), "{diagnostic}");
}

#[cfg(unix)]
#[test]
fn config_reader_accepts_a_directory_alias_for_the_fixed_config_leaf() {
    let root = tempdir().expect("create tempdir");
    let real = root.path().join("real");
    let alias = root.path().join("alias");
    fs::create_dir(&real).expect("create real config directory");
    fs::write(real.join("config.toml"), "[docs]\nroots = [\"aliased/\"]\n").expect("write config");
    symlink(&real, &alias).expect("create directory alias");

    let roots = read_docs_roots_from_config_path(&alias.join("config.toml"))
        .expect("read through supported directory alias");

    assert_eq!(root_paths(&roots), vec!["aliased/"]);
}

#[cfg(unix)]
#[test]
fn config_reader_rejects_a_leaf_swapped_for_a_symlink_before_open() {
    let root = tempdir().expect("create tempdir");
    let outside = tempdir().expect("create outside tempdir");
    let path = root.path().join("config.toml");
    let target = outside.path().join("config.toml");
    fs::write(&path, "[docs]\nroots = [\"inside/\"]\n").expect("write inside config");
    fs::write(&target, "[docs]\nroots = [\"external/\"]\n").expect("write outside config");
    let path_for_hook = path.clone();
    let target_for_hook = target.clone();
    docs_config_test_hook::set(Phase::BeforeLeafOpen, move |_| {
        fs::remove_file(&path_for_hook).expect("remove validated config");
        symlink(&target_for_hook, &path_for_hook).expect("swap config for symlink");
    });

    let error = read_docs_roots_from_config_path(&path).expect_err("reject swapped symlink");

    assert!(
        error.to_string().contains("must not be a symlink"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn config_reader_rejects_a_parent_swapped_after_validation() {
    let root = tempdir().expect("create tempdir");
    let authority = root.path().join("authority");
    let moved_authority = root.path().join("moved-authority");
    let outside = root.path().join("outside");
    fs::create_dir(&authority).expect("create authority");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(
        authority.join("config.toml"),
        "[docs]\nroots = [\"inside/\"]\n",
    )
    .expect("write inside config");
    fs::write(
        outside.join("config.toml"),
        "[docs]\nroots = [\"external/\"]\n",
    )
    .expect("write outside config");
    let authority_for_hook = authority.clone();
    let moved_for_hook = moved_authority.clone();
    let outside_for_hook = outside.clone();
    docs_config_test_hook::set(Phase::ParentValidated, move |_| {
        fs::rename(&authority_for_hook, &moved_for_hook).expect("move validated authority");
        symlink(&outside_for_hook, &authority_for_hook).expect("redirect authority path");
    });

    let error = read_docs_roots_from_config_path(&authority.join("config.toml"))
        .expect_err("reject changed parent authority");

    assert!(
        error.to_string().contains("changed or became a symlink")
            || error
                .to_string()
                .contains("directory changed while it was opened"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn config_reader_keeps_the_opened_parent_authority_when_its_path_is_swapped() {
    let root = tempdir().expect("create tempdir");
    let authority = root.path().join("authority");
    let moved_authority = root.path().join("moved-authority");
    let outside = root.path().join("outside");
    fs::create_dir(&authority).expect("create authority");
    fs::create_dir(&outside).expect("create outside directory");
    fs::write(
        authority.join("config.toml"),
        "[docs]\nroots = [\"inside/\"]\n",
    )
    .expect("write inside config");
    fs::write(
        outside.join("config.toml"),
        "[docs]\nroots = [\"external/\"]\n",
    )
    .expect("write outside config");
    let authority_for_hook = authority.clone();
    let moved_for_hook = moved_authority.clone();
    let outside_for_hook = outside.clone();
    docs_config_test_hook::set(Phase::BeforeLeafOpen, move |_| {
        fs::rename(&authority_for_hook, &moved_for_hook).expect("move opened authority");
        symlink(&outside_for_hook, &authority_for_hook).expect("redirect former authority path");
    });

    let roots = read_docs_roots_from_config_path(&authority.join("config.toml"))
        .expect("read from held parent authority");

    assert_eq!(root_paths(&roots), vec!["inside/"]);
}

#[cfg(unix)]
#[test]
fn config_reader_rejects_non_regular_leaf_without_blocking() {
    let root = tempdir().expect("create tempdir");
    let path = root.path().join("config.toml");
    let path_c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .expect("fixture path contains no null");
    let result = unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) };
    assert_eq!(
        result,
        0,
        "create fifo: {}",
        std::io::Error::last_os_error()
    );

    let error = read_docs_roots_from_config_path(&path).expect_err("reject fifo config");

    assert!(error.to_string().contains("regular config.toml"), "{error}");
}

#[cfg(unix)]
#[test]
fn config_reader_opens_read_only_without_changing_permissions() {
    let root = tempdir().expect("create tempdir");
    let path = root.path().join("config.toml");
    fs::write(&path, "[docs]\nroots = [\"readonly/\"]\n").expect("write config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("make config read-only");

    let roots = read_docs_roots_from_config_path(&path).expect("read read-only config");
    let mode = fs::metadata(&path)
        .expect("inspect config")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(root_paths(&roots), vec!["readonly/"]);
    assert_eq!(mode, 0o400);
}
