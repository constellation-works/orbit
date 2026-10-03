#![cfg(unix)]
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

#[path = "support/isolated_cli_fixture.rs"]
mod isolated_cli_fixture;
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
