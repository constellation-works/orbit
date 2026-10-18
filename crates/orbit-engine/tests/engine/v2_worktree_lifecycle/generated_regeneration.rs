//! [ORB-16784] Conflicts on `workflow.generated_paths` are regenerated.
//!
//! A stopped rebase that conflicts on a configured generated file is not
//! merged: the host continuation takes a side, runs the configured command
//! in the checkout once the provider returns, and stages its output with the
//! resolution. A command that fails or leaves conflict markers fails the
//! repair and leaves the stopped rebase for the failure handoff. Each test
//! runs a no-op substitute provider, so only the host touches the file.

use super::*;

/// A candidate and a base that both rewrote the generated `INDEX.md`, the
/// rebase stopped on it, and the provider that changes nothing.
fn stopped_on_generated(run_id: &str, command: &str) -> (PreparedRebase, LifecycleHost, Value) {
    let prepared = PreparedRebase::new(run_id, "INDEX.md", "INDEX.md");
    prepared.host.set_generated_path("INDEX.md", command);
    let conflict = stopped_conflict(&prepared);
    assert_eq!(conflict["conflicting_paths"], json!(["INDEX.md"]));
    let provider = prepared.fixture.root.path().join("codex");
    write_executable(&provider, &provider_script(""));
    let host = prepared.host.with_provider(&provider);
    (prepared, host, conflict)
}

/// The configured command rebuilds the generated file from a side the host
/// took, and the rebase continues with its output committed.
fn a_generated_path_conflict_is_resolved_by_its_regenerate_command() {
    isolated_in(
        module_path!(),
        "a_generated_path_conflict_is_resolved_by_its_regenerate_command",
        || {
            // The command sees one side, marker-free, before rewriting it.
            let (prepared, host, conflict) = stopped_on_generated(
                "jrun-regenerate",
                "! grep -q '<<<<<<<' INDEX.md && { cat INDEX.md; echo regenerated; } > INDEX.md.next && mv INDEX.md.next INDEX.md",
            );
            let checkout = &prepared.checkout.path;
            let outcome = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect("the regenerated conflict continues the rebase");
            assert!(outcome.success, "{:?}", outcome.message);
            assert!(!rebase_in_progress(checkout));
            assert!(is_ancestor(checkout, &prepared.target, "HEAD"));
            assert_eq!(
                git(checkout, &["show", "HEAD:INDEX.md"]),
                "target\nregenerated",
                "the upstream side, regenerated"
            );
            assert_eq!(git(checkout, &["status", "--porcelain"]), "");
            let checkpoints = host.checkpoints();
            assert_eq!(checkpoints.len(), 1, "one certified recovery");
            assert_eq!(checkpoints[0].2["regenerated_paths"], json!(["INDEX.md"]));
        },
    );
}

/// A failing command, or one that leaves the file conflicted, fails the
/// repair: nothing is staged, certified or continued.
fn a_failing_regenerate_command_fails_the_repair() {
    isolated_in(
        module_path!(),
        "a_failing_regenerate_command_fails_the_repair",
        || {
            for (case, command, refusal) in [
                (
                    "fails",
                    "echo index broken >&2; exit 3",
                    "exit code Some(3)",
                ),
                (
                    "leaves-markers",
                    "printf '<<<<<<< a\\nx\\n=======\\ny\\n>>>>>>> b\\n' > INDEX.md",
                    "left conflict markers",
                ),
            ] {
                let (prepared, host, conflict) =
                    stopped_on_generated(&format!("jrun-regenerate-{case}"), command);
                let checkout = &prepared.checkout.path;
                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("a failed regeneration must not continue the rebase");
                let error = error.to_string();
                assert!(
                    error.contains("regenerating a generated conflict path failed")
                        && error.contains(refusal),
                    "{case}: {error}"
                );
                assert!(rebase_in_progress(checkout), "{case}");
                assert_eq!(
                    git(checkout, &["diff", "--name-only", "--diff-filter=U"]),
                    "INDEX.md",
                    "{case}: the conflict stays unstaged"
                );
                assert!(host.checkpoints().is_empty(), "{case}");
            }
        },
    );
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn regeneration_contracts() {
    isolated_in(module_path!(), "regeneration_contracts", || {
        crate::git_fixture::run_scenarios(&[
            (
                "a_generated_path_conflict_is_resolved_by_its_regenerate_command",
                a_generated_path_conflict_is_resolved_by_its_regenerate_command,
            ),
            (
                "a_failing_regenerate_command_fails_the_repair",
                a_failing_regenerate_command_fails_the_repair,
            ),
        ]);
    });
}
