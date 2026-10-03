//! `skip_if_unchanged` cursor-task selection [ORB-12698].

use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_types::task::{TaskArtifact, TaskStatus};
use orbit_types::workflow::{SWEEP_CURSOR_ARTIFACT, SkipIfUnchanged, SweepCursorSelector};

use crate::OrbitRuntime;
use crate::application::auto_tasks::change_probe::newest_completed_sweep;
use crate::application::auto_tasks::{DEFAULT_AUTO_TASK_FILES, render_default_auto_task};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

fn precondition(tags: &[&str], legacy_tags: &[&str]) -> SkipIfUnchanged {
    SkipIfUnchanged {
        reference: "agent-main".to_string(),
        cursor: SweepCursorSelector {
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            legacy_tags: legacy_tags.iter().map(|tag| (*tag).to_string()).collect(),
        },
    }
}

/// One completed sweep chore carrying `tags`, with an optional cursor artifact.
fn completed_sweep(runtime: &OrbitRuntime, tags: &[&str], cursor: Option<&str>) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review recently merged changes".to_string(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            task_type: Some(orbit_types::task::TaskType::Chore),
            ..TaskAddParams::default()
        })
        .expect("add sweep");
    if let Some(content) = cursor {
        runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        SWEEP_CURSOR_ARTIFACT,
                        content.to_string(),
                    )],
                    ..Default::default()
                },
            )
            .expect("attach cursor artifact");
    }
    runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("complete sweep");
    task.id.to_string()
}

/// The shipped `code-review` definition's precondition, rendered for this
/// module's `agent-main` fixtures.
fn shipped_code_review_precondition() -> SkipIfUnchanged {
    let (_, yaml) = DEFAULT_AUTO_TASK_FILES
        .iter()
        .find(|(name, _)| *name == "code-review")
        .expect("code-review default");
    parse_auto_task_yaml(&render_default_auto_task(yaml, "agent-main"))
        .expect("parse code-review")
        .skip_if_unchanged
        .expect("code-review ships the precondition")
}

/// A whole-codebase review files done chores tagged `code-review` +
/// `no-diff-expected`; one newer than the last real sweep once became the
/// cursor task and blocked the next sweep for want of a cursor. The shipped
/// selector keys on the provenance tag only a minted sweep carries.
#[test]
fn a_newer_full_review_chore_never_shadows_an_older_minted_sweep() {
    let runtime = runtime();
    let shipped = shipped_code_review_precondition();
    let sweep = completed_sweep(
        &runtime,
        &["code-review", "no-diff-expected", "auto-task:code-review"],
        None,
    );
    completed_sweep(
        &runtime,
        &["code-review", "no-diff-expected", "full-review"],
        None,
    );
    let full_code_review = completed_sweep(
        &runtime,
        &["code-review", "no-diff-expected", "full-code-review"],
        None,
    );

    // Control: without the provenance tag the newest full-review chore wins.
    let unmarked = precondition(&["code-review", "no-diff-expected"], &[]);
    assert_eq!(
        newest_completed_sweep(&runtime, &unmarked)
            .expect("selection")
            .map(|task| task.id.to_string()),
        Some(full_code_review),
        "fixture must create full-review chores newer than the sweep"
    );

    assert_eq!(
        newest_completed_sweep(&runtime, &shipped)
            .expect("selection")
            .map(|task| task.id.to_string()),
        Some(sweep)
    );
}
