#![cfg(unix)]
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[cfg(unix)]
#[test]
fn skill_cli_links_only_owned_catalog_entries_and_preserves_user_files() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let first = fixture.json(&["skill", "link", "--json"]);
    let roots = first["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    let external = fixture.repo.join("user-skill");
    fs::create_dir_all(&external).unwrap();
    fs::write(external.join("SKILL.md"), "user-owned skill bytes").unwrap();
    for root in roots {
        let root = PathBuf::from(root.as_str().unwrap());
        assert!(
            root.starts_with(fixture._temp.path()),
            "fixture root must stay disposable"
        );
        fs::write(root.join("user-note.txt"), "preserve me").unwrap();
        symlink(&external, root.join("user-skill")).unwrap();
    }
    let listed = fixture.json(&["skill", "list", "--json"]);
    let selected = &listed.as_array().unwrap()[0];
    let id = selected["id"].as_str().unwrap();
    let shown = fixture.json(&["skill", "show", id, "--json"]);
    assert_eq!(shown["id"], id);
    assert_eq!(shown["content_hash"], selected["content_hash"]);
    assert!(!shown["content"].as_str().unwrap().is_empty());
    let doctor = fixture.json(&["skill", "doctor", "--json"]);
    assert!(
        doctor
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["skill_id"] == id)
    );
    fixture
        .command(&["skill", "show", "missing-fixture-skill", "--json"])
        .assert()
        .failure();
    let unlinked = fixture.json(&["skill", "unlink", "--json"]);
    assert!(unlinked["removed_count"].as_u64().unwrap() > 0);
    for root in roots {
        let root = PathBuf::from(root.as_str().unwrap());
        assert_eq!(
            fs::read_to_string(root.join("user-note.txt")).unwrap(),
            "preserve me"
        );
        assert_eq!(fs::read_link(root.join("user-skill")).unwrap(), external);
    }
    assert_eq!(
        fs::read_to_string(external.join("SKILL.md")).unwrap(),
        "user-owned skill bytes"
    );
    assert_eq!(
        fixture.json(&["skill", "unlink", "--json"])["removed_count"],
        0
    );
    assert!(
        fixture.json(&["skill", "link", "--json"])["linked_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        fixture.json(&["skill", "link", "--json"])["linked_count"],
        0
    );
    assert_eq!(fixture.json(&["skill", "show", id, "--json"]), shown);
}

#[cfg(unix)]
#[test]
fn skill_unlink_preserves_foreign_tree_behind_symlinked_discovery_parent() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let roots = fixture.json(&["skill", "link", "--json"])["roots"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(roots.len(), 2);

    // Move each real `.agents` / `.claude` parent out of the base, then point the
    // original name at it. The discovery `skills` dir is real, but reached through
    // a symlinked parent, so unlink must treat it as foreign.
    let mut redirected = Vec::new();
    for root in &roots {
        let skills_dir = PathBuf::from(root.as_str().unwrap());
        let parent = skills_dir.parent().unwrap().to_path_buf();
        let name = parent.file_name().unwrap().to_str().unwrap();
        let foreign_parent = fixture.repo.join(format!("foreign-{name}"));
        fs::rename(&parent, &foreign_parent).unwrap();
        symlink(&foreign_parent, &parent).unwrap();
        fs::write(
            foreign_parent.join("skills").join("user-note.txt"),
            "foreign note",
        )
        .unwrap();
        redirected.push((parent, foreign_parent.join("skills")));
    }
    let before: Vec<_> = redirected
        .iter()
        .map(|(_, skills_dir)| discovery_entries(skills_dir))
        .collect();
    assert!(
        before
            .iter()
            .all(|entries| entries.values().any(|link| link.is_some())),
        "foreign discovery dirs must hold core-shaped catalog links"
    );

    let unlinked = fixture.json(&["skill", "unlink", "--json"]);
    assert_eq!(unlinked["removed_count"], 0);
    assert!(unlinked["cleaned_dirs"].as_array().unwrap().is_empty());

    for ((parent, skills_dir), entries) in redirected.iter().zip(&before) {
        assert!(
            fs::symlink_metadata(parent)
                .unwrap()
                .file_type()
                .is_symlink(),
            "discovery parent redirect must survive unlink: {}",
            parent.display()
        );
        assert_eq!(
            &discovery_entries(skills_dir),
            entries,
            "foreign discovery dir must keep every entry: {}",
            skills_dir.display()
        );
        for target in entries.values().flatten() {
            assert!(
                target.is_dir(),
                "catalog target must survive unlink: {}",
                target.display()
            );
        }
    }
    assert_eq!(
        fs::read_to_string(redirected[0].1.join("user-note.txt")).unwrap(),
        "foreign note"
    );
}

/// Entry names with their symlink targets, so a directory's contents compare exactly.
fn discovery_entries(dir: &Path) -> BTreeMap<String, Option<PathBuf>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            (name, fs::read_link(entry.path()).ok())
        })
        .collect()
}
