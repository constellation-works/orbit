//! An owner run whose agent an Orbit upgrade refused mid-step did not judge
//! its task. Finalization returns the task to the backlog instead of blocking
//! it, and admission takes it again, so its next run resumes the candidate the
//! failure handoff held.

use orbit_core::TaskStatus;
use orbit_types::workflow::{AgentBlocker, UPGRADE_PENDING_REQUEUED_EVENT};

use super::dispatch_admission::isolated;
use super::provider_failure_hold::fixture;

const CREWS: &str = r#"[workflow]
default_crew = "sol"
medium_complexity_crews = ["sol"]

[crews.sol]
provider = "codex"
model = "sol-model"
"#;

#[test]
fn an_upgrade_refused_run_returns_its_task_to_the_backlog() {
    if !isolated("upgrade_pending_requeue::an_upgrade_refused_run_returns_its_task_to_the_backlog")
    {
        return;
    }
    let fx = fixture(CREWS);
    let task = fx.task("sol");
    let run = fx.admit(&task, "sol");
    let blocker = AgentBlocker {
        kind: "orbit_upgrade_admission_refused".to_string(),
        evidence: "every orbit command was refused".to_string(),
    };
    fx.fail(&run, &blocker.step_failure_message());

    assert_eq!(fx.status(&task), TaskStatus::Backlog);
    let history = fx.runtime.get_task_history(&task).unwrap();
    let entry = history.last().expect("the requeue is recorded");
    assert_eq!(entry.event, UPGRADE_PENDING_REQUEUED_EVENT, "{entry:?}");
    assert_eq!(entry.to_status, Some(TaskStatus::Backlog), "{entry:?}");
    assert!(
        entry
            .note
            .as_deref()
            .is_some_and(|note| note.contains(&run)),
        "the requeue names the run: {entry:?}"
    );
    assert!(
        fx.admitted(&task, &["sol"]),
        "admission takes the task again, on the same crew"
    );
}
