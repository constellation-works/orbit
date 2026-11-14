//! Regression at the public forge boundary for a failed job whose workflow
//! has not completed yet (ORB-22211). Time and gh replies are deterministic and local to
//! each test thread; no credentials, environment changes or real waits.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::task::pr_forge::{GhTaskPrForge, TaskPrForge};

const HEAD: &str = "0123456789012345678901234567890123456789";

enum Reply {
    List(Value),
    Rerun(u64),
}

struct FakeGh {
    replies: VecDeque<Reply>,
    epoch: Instant,
    elapsed: Duration,
    sleeps: Vec<Duration>,
}

thread_local! {
    static GH: RefCell<Option<FakeGh>> = const { RefCell::new(None) };
}

struct Fixture;

impl Fixture {
    fn install(replies: impl IntoIterator<Item = Reply>) -> Self {
        GH.with_borrow_mut(|gh| {
            assert!(gh.is_none());
            *gh = Some(FakeGh {
                replies: replies.into_iter().collect(),
                epoch: Instant::now(),
                elapsed: Duration::ZERO,
                sleeps: Vec::new(),
            });
        });
        Self
    }

    fn assert_finished(&self, expected_wait: Duration) {
        GH.with_borrow(|gh| {
            let gh = gh.as_ref().expect("fake gh installed");
            assert!(gh.replies.is_empty(), "all expected gh requests were made");
            assert_eq!(gh.elapsed, expected_wait);
            assert_eq!(gh.sleeps.iter().sum::<Duration>(), expected_wait);
        });
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        GH.with_borrow_mut(|gh| *gh = None);
    }
}

// Crate visibility lets the sibling source module bind this test-only double.
pub(crate) fn task_forge_gh_request(
    request: ExecRequest,
    _repo_root: &Path,
    _label: &str,
) -> Result<String, OrbitError> {
    GH.with_borrow_mut(|gh| {
        let gh = gh
            .as_mut()
            .expect("real gh is forbidden in forge unit tests");
        match gh.replies.pop_front().expect("unexpected extra gh request") {
            Reply::List(runs) => {
                let remaining_ms = (Duration::from_secs(120) - gh.elapsed).as_millis() as u64;
                assert!(
                    request
                        .timeout_ms
                        .is_some_and(|ms| ms > 0 && ms <= remaining_ms),
                    "workflow reads must fit inside the remaining wait budget"
                );
                assert_eq!(
                    request.args,
                    [
                        "run",
                        "list",
                        "--commit",
                        HEAD,
                        "--limit",
                        "100",
                        "--json",
                        "databaseId,workflowName,headSha,status,conclusion",
                    ]
                );
                Ok(runs.to_string())
            }
            Reply::Rerun(id) => {
                assert_eq!(request.args, ["run", "rerun", &id.to_string(), "--failed"]);
                Ok(String::new())
            }
        }
    })
}

// Crate visibility lets the sibling source module bind this test-only clock.
pub(crate) fn workflow_rerun_now() -> Instant {
    GH.with_borrow(|gh| {
        let gh = gh.as_ref().expect("fake gh installed");
        gh.epoch + gh.elapsed
    })
}

// Crate visibility lets the sibling source module bind this test-only sleep.
pub(crate) fn workflow_rerun_sleep(duration: Duration) {
    GH.with_borrow_mut(|gh| {
        let gh = gh.as_mut().expect("fake gh installed");
        assert!(!duration.is_zero(), "polling must never busy loop");
        gh.elapsed += duration;
        gh.sleeps.push(duration);
    });
}

fn workflow(id: u64, head: &str, status: &str, conclusion: &str) -> Value {
    json!({
        "databaseId": id, "workflowName": "CI", "headSha": head,
        "status": status, "conclusion": conclusion,
    })
}

#[test]
fn rerun_waits_for_running_workflow_before_requesting_failed_jobs() {
    let fixture = Fixture::install([
        Reply::List(json!([
            workflow(12, HEAD, "in_progress", ""),
            workflow(13, "different-head", "in_progress", ""),
        ])),
        Reply::List(json!([
            workflow(12, HEAD, "completed", "failure"),
            workflow(13, "different-head", "completed", "failure"),
        ])),
        Reply::Rerun(12),
    ]);
    let outcome = GhTaskPrForge
        .rerun_failed_checks(Path::new("."), 42, HEAD)
        .expect("wait for workflow completion, then rerun failed jobs");
    assert_eq!(outcome.requested, [(12, "CI".into())]);
    assert!(outcome.uncertain.is_none());
    fixture.assert_finished(Duration::from_secs(5));
}

#[test]
fn rerun_refuses_after_bounded_wait_and_names_running_workflow() {
    let fixture = Fixture::install(
        (0..24).map(|_| Reply::List(json!([workflow(12, HEAD, "in_progress", "")]))),
    );
    let error = GhTaskPrForge
        .rerun_failed_checks(Path::new("."), 42, HEAD)
        .expect_err("running workflow must expire without a rerun");
    let message = error.to_string();
    assert!(message.contains("no failed workflow run on head"));
    assert!(message.contains("in_progress"), "{message}");
    assert!(message.contains("CI (run 12)"), "{message}");
    fixture.assert_finished(Duration::from_secs(120));
}

#[test]
fn rerun_still_refuses_without_failed_or_running_workflows_on_head() {
    let fixture = Fixture::install([Reply::List(json!([
        workflow(12, HEAD, "completed", "success"),
        workflow(13, "different-head", "in_progress", ""),
    ]))]);
    let error = GhTaskPrForge
        .rerun_failed_checks(Path::new("."), 42, HEAD)
        .expect_err("no failed workflow on this head");
    assert_eq!(
        error.to_string(),
        format!(
            "execution failed: no failed workflow run on head {HEAD} of pull request #42 can be rerun"
        )
    );
    fixture.assert_finished(Duration::ZERO);
}
