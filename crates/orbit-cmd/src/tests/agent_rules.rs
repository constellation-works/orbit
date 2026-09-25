use std::path::Path;

use orbit_common::OrbitError;
use tempfile::tempdir;

use super::super::agent_rules::{
    AGENT_RULES_TEMPLATE, END_MARKER, InjectionAction, START_MARKER, apply_to_file,
    inject_agent_rules, normalized_block,
};

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read tempfile")
}

fn synth_block(body: &str) -> String {
    format!("{START_MARKER}\n{body}\n{END_MARKER}\n")
}

#[test]
fn creates_file_when_absent() {
    let dir = tempdir().expect("tempdir");
    let result = inject_agent_rules(dir.path()).expect("inject");

    assert_eq!(result.outcomes.len(), 2);
    for outcome in &result.outcomes {
        assert_eq!(outcome.action, InjectionAction::Created);
        assert!(
            outcome.path.exists(),
            "{} should exist",
            outcome.path.display()
        );
        let body = read(&outcome.path);
        assert!(body.starts_with(START_MARKER));
        assert!(body.contains(END_MARKER));
        assert!(
            body.ends_with('\n'),
            "file must end with exactly one newline"
        );
        assert!(!body.ends_with("\n\n"), "no double trailing newline");
    }
}

#[test]
fn appends_block_when_no_markers() {
    let dir = tempdir().expect("tempdir");
    let claude = dir.path().join("CLAUDE.md");
    let existing = "# Project rules\n\nUse 4-space indent.\n";
    std::fs::write(&claude, existing).expect("seed");

    let block = synth_block("test body");
    let action = apply_to_file(&claude, &block).expect("apply");
    assert_eq!(action, InjectionAction::AppendedBlock);

    let final_content = read(&claude);
    assert!(
        final_content.starts_with(existing),
        "pre-existing bytes must be preserved verbatim at the head"
    );
    assert!(final_content.contains(START_MARKER));
    assert!(final_content.contains(END_MARKER));
    // One blank-line separator between original content and the block.
    let after_existing = &final_content[existing.len()..];
    assert!(
        after_existing.starts_with('\n'),
        "expected blank-line separator before block, got: {after_existing:?}"
    );
}

#[test]
fn replaces_block_when_markers_present() {
    let dir = tempdir().expect("tempdir");
    let claude = dir.path().join("CLAUDE.md");
    let prefix = "# Existing\n\nKeep me.\n\n";
    let suffix = "\n## Tail\n\nKeep me too.\n";
    let stale_block = synth_block("stale body");
    let initial = format!("{prefix}{stale_block}{suffix}");
    std::fs::write(&claude, &initial).expect("seed");

    let fresh_block = synth_block("fresh body");
    let action = apply_to_file(&claude, &fresh_block).expect("apply");
    assert_eq!(action, InjectionAction::ReplacedBlock);

    let final_content = read(&claude);
    assert!(
        final_content.starts_with(prefix),
        "head must be byte-stable"
    );
    assert!(final_content.ends_with(suffix), "tail must be byte-stable");
    assert!(final_content.contains("fresh body"));
    assert!(!final_content.contains("stale body"));
}

#[test]
fn rejects_malformed_marker_pair_start_only() {
    let dir = tempdir().expect("tempdir");
    let claude = dir.path().join("CLAUDE.md");
    let malformed = format!("# rules\n{START_MARKER}\nbody without end\n");
    std::fs::write(&claude, &malformed).expect("seed");

    let err = apply_to_file(&claude, &synth_block("ignored"))
        .expect_err("must reject malformed marker pair");
    assert!(matches!(err, OrbitError::InvalidInput(_)), "got {err:?}");
    match err {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("CLAUDE.md"), "msg: {msg}");
            assert!(msg.contains(END_MARKER), "msg: {msg}");
        }
        _ => unreachable!(),
    }
    // File must be untouched.
    assert_eq!(read(&claude), malformed);
}

#[test]
fn rejects_malformed_marker_pair_end_only() {
    let dir = tempdir().expect("tempdir");
    let agents = dir.path().join("AGENTS.md");
    let malformed = format!("# rules\nbody {END_MARKER} without start\n");
    std::fs::write(&agents, &malformed).expect("seed");

    let err = apply_to_file(&agents, &synth_block("ignored"))
        .expect_err("must reject malformed marker pair");
    match err {
        OrbitError::InvalidInput(msg) => {
            assert!(msg.contains("AGENTS.md"), "msg: {msg}");
            assert!(msg.contains(START_MARKER), "msg: {msg}");
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert_eq!(read(&agents), malformed);
}

#[test]
fn idempotent_when_template_unchanged() {
    let dir = tempdir().expect("tempdir");
    let first = inject_agent_rules(dir.path()).expect("first");
    let claude_first = read(&first.outcomes[0].path);
    let agents_first = read(&first.outcomes[1].path);

    let second = inject_agent_rules(dir.path()).expect("second");
    let claude_second = read(&second.outcomes[0].path);
    let agents_second = read(&second.outcomes[1].path);

    assert_eq!(
        claude_first, claude_second,
        "CLAUDE.md must be byte-stable across re-runs"
    );
    assert_eq!(
        agents_first, agents_second,
        "AGENTS.md must be byte-stable across re-runs"
    );
}

#[test]
fn real_template_round_trips_byte_stably() {
    // Guards against the asset losing its markers or the trim/append
    // logic drifting from the asset's newline shape.
    let block = normalized_block(AGENT_RULES_TEMPLATE).expect("normalize");
    let dir = tempdir().expect("tempdir");
    let claude = dir.path().join("CLAUDE.md");
    let action_one = apply_to_file(&claude, &block).expect("first");
    assert_eq!(action_one, InjectionAction::Created);
    let after_one = read(&claude);

    let action_two = apply_to_file(&claude, &block).expect("second");
    assert_eq!(action_two, InjectionAction::ReplacedBlock);
    let after_two = read(&claude);
    assert_eq!(after_one, after_two);
}

#[cfg(unix)]
#[test]
fn symlinked_target_is_written_through_once_and_stays_a_link() {
    let dir = tempdir().expect("tempdir");
    let agents = dir.path().join("AGENTS.md");
    let claude = dir.path().join("CLAUDE.md");
    std::fs::write(&agents, "# Guide\n").expect("write AGENTS.md");
    std::os::unix::fs::symlink("AGENTS.md", &claude).expect("link CLAUDE.md");

    let result = inject_agent_rules(dir.path()).expect("inject");

    assert_eq!(result.outcomes.len(), 1, "one real file, one write");
    assert!(
        std::fs::symlink_metadata(&claude)
            .expect("CLAUDE.md metadata")
            .file_type()
            .is_symlink(),
        "CLAUDE.md must remain a symlink"
    );
    assert_eq!(read(&agents).matches(START_MARKER).count(), 1);
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
