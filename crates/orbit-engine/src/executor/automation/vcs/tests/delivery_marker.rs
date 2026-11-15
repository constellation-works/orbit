use std::path::Path;

use orbit_common::test_env::canonical_temp_dir;

use super::super::delivery_marker::{commits_matching, delivery_markers};

// Parser edge cases meet the combinatorial pure-logic unit-test criterion.
#[test]
fn delivery_markers_preserve_valid_inner_candidates_and_marker_grammar() {
    let cases: &[(&str, &[&str])] = &[
        ("fix: buf[i [ORB-5]", &["[ORB-5]"]),
        ("feat: ... [see ORB-1… [ORB-5]", &["[ORB-5]"]),
        ("fix: [[[[ORB-5]]]]", &["[ORB-5]"]),
        ("fix: [buf[i [ORB-5] tail]", &["[ORB-5]"]),
        ("fix: é[備考 [ORB-5]", &["[ORB-5]"]),
        (
            "[ORB-5] [broken [T20260430-31B] [ORB-5]\n[GITHUB-PR-902]",
            &["[ORB-5]", "[T20260430-31B]", "[GITHUB-PR-902]"],
        ),
        (
            "[] [123] [letters] [ORB 5] [ORB\n5] [ORB\u{a0}5] [ORB-5",
            &[],
        ),
        ("ORB-5] plain text [unfinished", &[]),
        ("", &[]),
        (
            "fix: repair [ORB-21300]\n\nFixes: 5cac195ba9ab (\"feat: original [ORB-21284]\")\nRegression-From: ORB-21284",
            &["[ORB-21300]"],
        ),
        ("fix: x\n\n  Fixes: abc (\"feat: y [ORB-1]\")", &[]),
        (
            "feat(dashboard): drain card [ORB-20509, ORB-20510] (#7304)",
            &["[ORB-20509]", "[ORB-20510]"],
        ),
        (
            "fix: a long bundle subject [ORB-1]\n\nBody.\n\nOrbit-Task: ORB-1\nOrbit-Task: ORB-2",
            &["[ORB-1]", "[ORB-2]"],
        ),
    ];
    for (message, expected) in cases {
        assert_eq!(delivery_markers(message), *expected, "message: {message:?}");
    }

    for (length, accepted) in [(64, true), (65, false)] {
        let token = format!("A{}5", "x".repeat(length - 2));
        let marker = format!("[{token}]");
        let message = format!("[unfinished {marker} [ORB-5]");
        let expected = if accepted {
            vec![marker.as_str(), "[ORB-5]"]
        } else {
            vec!["[ORB-5]"]
        };
        assert_eq!(
            delivery_markers(&message),
            expected,
            "token length: {length}"
        );
    }
}

#[test]
fn commits_matching_ignores_comma_list_prose_but_matches_delivery_markers() {
    let repo = tempfile::tempdir_in(canonical_temp_dir()).unwrap();
    git_ok(repo.path(), &["init", "-q", "-b", "main", "."]);
    git_ok(repo.path(), &["config", "user.name", "Orbit Test"]);
    git_ok(
        repo.path(),
        &["config", "user.email", "orbit-test@example.com"],
    );
    git_ok(repo.path(), &["config", "commit.gpgsign", "false"]);

    let prose = commit(
        repo.path(),
        &[
            "feat: discussed work",
            "Related work: ORB-111, ORB-222, ORB-333 were discussed, none delivered here.",
        ],
    );
    let unclosed = commit(
        repo.path(),
        &["fix: parser [ORB-111, ORB-222, ORB-333 handled; see [docs]"],
    );
    let single = commit(repo.path(), &["feat: single [ORB-222]"]);
    let list = commit(repo.path(), &["feat: list [ORB-111, ORB-222]"]);
    let trailer = commit(
        repo.path(),
        &["feat: trailer delivery", "Body.\n\nOrbit-Task: ORB-222"],
    );

    let matched = commits_matching(repo.path(), "[ORB-222]", &["HEAD"]).unwrap();
    assert_eq!(matched, vec![trailer, list, single]);
    assert!(!matched.contains(&prose));
    assert!(!matched.contains(&unclosed));
}

fn commit(repo: &Path, messages: &[&str]) -> String {
    let mut args = vec!["commit", "-q", "--allow-empty", "--no-verify"];
    for message in messages {
        args.push("-m");
        args.push(message);
    }
    git_ok(repo, &args);
    git_ok(repo, &["rev-parse", "HEAD"])
}

fn git_ok(repo: &Path, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    command.args(args).current_dir(repo);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
