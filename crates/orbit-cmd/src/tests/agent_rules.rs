use std::path::Path;

use orbit_common::OrbitError;
use tempfile::tempdir;

use super::super::agent_rules::{END_MARKER, START_MARKER, inject_agent_rules};

#[test]
fn bad_markers_in_one_guide_leave_every_guide_unchanged() {
    let cases = [
        (
            "unbalanced",
            format!("# Agents\n{START_MARKER}\nno end marker\n"),
        ),
        (
            "misordered",
            format!("# Agents\n{END_MARKER}\nbetween\n{START_MARKER}\n"),
        ),
    ];
    for (label, agents) in cases {
        let dir = tempdir().expect("tempdir");
        let claude = b"# Claude guide\nKeep this unchanged.\n";
        std::fs::write(dir.path().join("CLAUDE.md"), claude).expect("write CLAUDE.md");
        std::fs::write(dir.path().join("AGENTS.md"), &agents).expect("write AGENTS.md");

        let err = inject_agent_rules(dir.path()).expect_err(label);

        assert!(
            matches!(err, OrbitError::InvalidInput(_)),
            "{label}: expected InvalidInput, got {err:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("CLAUDE.md")).expect("read CLAUDE.md"),
            claude,
            "{label}: CLAUDE.md must not change when AGENTS.md is refused"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("AGENTS.md")).expect("read AGENTS.md"),
            agents,
            "{label}: AGENTS.md must not change"
        );
    }
}

#[cfg(unix)]
#[test]
fn escaping_guide_symlinks_are_rejected_without_writing() {
    for name in ["CLAUDE.md", "AGENTS.md"] {
        let dir = tempdir().expect("tempdir");
        let workspace = dir.path().join("workspace");
        let shared = dir.path().join("shared");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&shared).expect("shared");
        let outside = shared.join("guide.md");
        let original = b"# Shared guide\nKeep this unchanged.\n";
        std::fs::write(&outside, original).expect("write external guide");
        let guide = workspace.join(name);
        let link_target = if name == "CLAUDE.md" {
            Path::new("../shared/guide.md")
        } else {
            outside.as_path()
        };
        std::os::unix::fs::symlink(link_target, &guide).expect("link guide");

        let err = inject_agent_rules(&workspace).expect_err("outside guide must be rejected");

        match err {
            OrbitError::InvalidInput(message) => {
                assert!(message.contains(name), "msg: {message}");
                assert!(message.contains("outside workspace"), "msg: {message}");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        assert_eq!(std::fs::read(&outside).expect("external guide"), original);
        assert!(
            std::fs::symlink_metadata(&guide)
                .expect("guide metadata")
                .file_type()
                .is_symlink(),
            "{name} must remain a symlink"
        );
        let other = if name == "CLAUDE.md" {
            "AGENTS.md"
        } else {
            "CLAUDE.md"
        };
        assert!(
            !workspace.join(other).exists(),
            "rejection must happen before writing either guide"
        );
    }
}
