use std::collections::BTreeMap;
use std::ffi::OsString;

use super::super::super::backend::{program_statuses, resolve_declared_programs};
use super::*;

fn executable(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("program dir");
    std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write program");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

#[cfg(unix)]
#[test]
fn a_declared_program_resolves_through_the_enabling_path_to_its_canonical_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    executable(&second.join("tool"));
    // A non-executable file earlier on PATH is skipped, as a shell would.
    std::fs::create_dir_all(&first).expect("first dir");
    std::fs::write(first.join("tool"), "data").expect("plain file");
    let alias = temp.path().join("alias-dir");
    std::fs::create_dir_all(&alias).expect("alias dir");
    std::os::unix::fs::symlink(second.join("tool"), alias.join("aliased")).expect("alias");
    let search_path: OsString = std::env::join_paths([
        PathBuf::from("relative/bin"),
        first.clone(),
        second.clone(),
        alias.clone(),
    ])
    .expect("join PATH");

    let canonical = second.join("tool").canonicalize().expect("canonical");
    let declared = vec![
        "tool".to_string(),
        "aliased".to_string(),
        canonical.to_string_lossy().into_owned(),
        "missing-program".to_string(),
        "bin/tool".to_string(),
    ];
    let (resolved, unresolved) = resolve_declared_programs(&declared, Some(&search_path));

    assert_eq!(resolved.get("tool"), Some(&canonical));
    assert_eq!(
        resolved.get("aliased"),
        Some(&canonical),
        "a link is recorded as the file it resolves to"
    );
    assert_eq!(
        resolved.get(canonical.to_str().expect("utf8")),
        Some(&canonical)
    );
    let unresolved: Vec<&str> = unresolved.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(unresolved, vec!["missing-program", "bin/tool"]);

    let (resolved, unresolved) = resolve_declared_programs(&["tool".to_string()], None);
    assert!(resolved.is_empty());
    assert!(unresolved[0].1.contains("PATH"), "{unresolved:?}");
}

#[cfg(unix)]
#[test]
fn a_recorded_program_is_granted_only_while_it_names_the_consented_executable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let state_dir = global_root.join("state/plugins/demo");
    let tool = temp.path().join("bin/tool");
    let moved = temp.path().join("bin/moved");
    let other_state = global_root.join("state/plugins/other/tool");
    let own_state = state_dir.join("tool");
    for path in [&tool, &moved, &other_state, &own_state] {
        executable(path);
    }
    let tool = tool.canonicalize().expect("canonical tool");
    let link = temp.path().join("bin/link");
    std::os::unix::fs::symlink(&tool, &link).expect("link");
    let recorded: BTreeMap<String, PathBuf> = [
        ("tool", tool.clone()),
        ("link", link.clone()),
        ("gone", temp.path().join("bin/gone")),
        ("foreign", other_state.canonicalize().expect("canonical")),
        ("own", own_state.canonicalize().expect("canonical")),
    ]
    .into_iter()
    .map(|(name, path)| (name.to_string(), path))
    .collect();
    let declared: Vec<String> = ["tool", "link", "gone", "foreign", "own", "unrecorded"]
        .into_iter()
        .map(String::from)
        .collect();

    let statuses = program_statuses(&declared, &recorded, &global_root, &state_dir);
    let granted: Vec<&str> = statuses
        .iter()
        .filter(|status| status.granted())
        .map(|status| status.name.as_str())
        .collect();
    assert_eq!(granted, vec!["tool", "own"], "{statuses:#?}");
    let problem = |name: &str| {
        statuses
            .iter()
            .find(|status| status.name == name)
            .and_then(|status| status.problem.clone())
            .unwrap_or_default()
    };
    assert!(problem("link").contains("now resolves to"), "{statuses:#?}");
    assert!(problem("gone").contains("no such file"), "{statuses:#?}");
    assert!(problem("foreign").contains("host-owned"), "{statuses:#?}");
    assert!(
        problem("unrecorded").contains("no path was recorded"),
        "{statuses:#?}"
    );
}
