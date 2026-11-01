#![allow(
    clippy::unwrap_used,
    reason = "job-wait fixtures panic when a spec does not parse, which means the fixture itself is wrong"
)]

//! Load-time checks of a job's auto-task steps [ORB-20157], through the
//! public `check_job_waits` boundary: the allowed shapes of
//! `docs/design/activity-job/specs/auto-task-step.md` §2-§6 load, and each
//! refusal names its field.

use chrono::{Duration, Utc};
use orbit_automation::condition::{ConditionCheckError, OuterBound, check_job_waits};
use orbit_types::workflow::JobV2;
use serde_json::{Value, json};

fn job(spec: Value) -> JobV2 {
    serde_json::from_value(spec).unwrap()
}

/// The spec's sequential release-readiness job (§6).
fn sequential() -> Value {
    json!({
        "state": "disabled",
        "max_active_runs": 1,
        "expire_when": "now >= job.started_at + duration('24h')",
        "steps": [
            {
                "id": "qa",
                "target": "auto-task:full-qa-sweep",
                "wait_until": "board.empty",
                "require_result": "release-readiness/v1",
                "reuse_when": "result.status == 'passed' && result.revision == job.source_revision \
                               && now - result.completed_at <= duration('24h')"
            },
            {
                "id": "security",
                "target": "auto-task:security-review",
                "wait_until": "self.prev.status == 'done' && board.empty",
                "expire_when": "now >= self.wait_started_at + duration('8h')",
                "require_result": "release-readiness/v1"
            },
            {
                "id": "prepare",
                "target": "auto-task:release-prep",
                "require_result": "release-readiness/v1",
                "wait_until": "steps['qa'].status == 'done' && steps['security'].status == 'done' \
                               && board.empty"
            }
        ]
    })
}

/// The spec's parallel variant (§6), with a `when` guard on one branch.
fn parallel() -> Value {
    json!({
        "state": "disabled",
        "expire_when": "now >= timestamp('2000-01-01T00:00:00Z')",
        "steps": [
            {
                "id": "checks",
                "parallel": {
                    "join": {"mode": "all"},
                    "branches": [
                        {"id": "qa", "target": "auto-task:full-qa-sweep", "wait_until": "board.empty"},
                        {
                            "id": "security",
                            "target": "auto-task:security-review",
                            "when": "{{ input.security }}",
                            "wait_until": "now >= job.started_at + duration('30m') && board.empty"
                        }
                    ]
                }
            },
            {
                "id": "prepare",
                "target": "auto-task:release-prep",
                "wait_until": "steps['qa'].status == 'done' && steps['checks'].status == 'done' \
                               && board.empty"
            }
        ]
    })
}

fn refusal(spec: Value) -> ConditionCheckError {
    match check_job_waits(&job(spec), Utc::now()) {
        Ok(_) => panic!("accepted a job the checks must refuse"),
        Err(error) => error,
    }
}

/// Replace one step's field: `path` is the step's index path through
/// `steps` and `parallel.branches`.
fn with_step(mut spec: Value, path: &[usize], field: &str, value: Value) -> Value {
    let mut step = &mut spec["steps"][path[0]];
    for index in &path[1..] {
        step = &mut step["parallel"]["branches"][*index];
    }
    step[field] = value;
    spec
}

#[test]
fn spec_examples_load_with_their_structure_and_reads() {
    let checked = check_job_waits(&job(sequential()), Utc::now()).unwrap();
    assert!(
        checked.pinned_revision,
        "a readiness requirement pins the revision"
    );
    assert_eq!(
        checked.expiry.as_ref().unwrap().bound,
        OuterBound::AfterStart(Duration::hours(24))
    );
    let ids = checked
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["qa", "security", "prepare"]);
    assert_eq!(checked.steps[0].prev, None, "a first step has no self.prev");
    assert_eq!(checked.steps[1].prev.as_deref(), Some("qa"));
    assert_eq!(checked.steps[2].auto_task, "release-prep");
    assert_eq!(
        checked.steps[2].reads.steps.iter().collect::<Vec<_>>(),
        ["qa", "security"]
    );
    assert!(checked.steps[2].reads.board.contains("empty"));

    let checked = check_job_waits(&job(parallel()), Utc::now()).unwrap();
    assert!(!checked.pinned_revision);
    let branch = &checked.steps[1];
    assert_eq!(branch.parallel.as_deref(), Some("checks"));
    assert_eq!(branch.prev, None, "a parallel branch has no self.prev");
    assert_eq!(branch.when.as_deref(), Some("{{ input.security }}"));
    assert_eq!(
        checked.steps[2].reads.steps.iter().collect::<Vec<_>>(),
        ["checks", "qa"],
        "a step after a joined parallel block reads its branches"
    );
}

#[test]
fn jobs_without_auto_task_steps_are_unchanged() {
    let ordinary = job(json!({
        "state": "enabled",
        "steps": [{"id": "only", "target": "activity:agent_assess_diff", "when": "true"}]
    }));
    let checked = check_job_waits(&ordinary, Utc::now()).unwrap();
    assert!(checked.expiry.is_none() && checked.steps.is_empty());
}

#[test]
fn each_refusal_names_its_field() {
    // Each case is refused at its field, located by the structured `clause`
    // of the error, not by its wording.
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "missing outer bound",
            json!({"state": "disabled", "steps": [{"id": "a", "target": "auto-task:x"}]}),
            "spec.expire_when",
        ),
        (
            "expiry on a job without auto-task steps",
            json!({"state": "disabled", "expire_when": "now >= job.started_at + duration('1h')",
                   "steps": [{"id": "a", "target": "activity:x"}]}),
            "spec.expire_when",
        ),
        (
            "nonfinite outer bound",
            {
                let mut spec = sequential();
                spec["expire_when"] = json!("now >= job.started_at + duration('1h') || true");
                spec
            },
            "spec.expire_when",
        ),
        (
            "outer duration over seven days",
            {
                let mut spec = sequential();
                spec["expire_when"] = json!("now >= job.started_at + duration('169h')");
                spec
            },
            "spec.expire_when",
        ),
        (
            "outer deadline over seven days away",
            {
                let mut spec = sequential();
                spec["expire_when"] = json!(format!(
                    "now >= timestamp('{}')",
                    (Utc::now() + Duration::days(8)).to_rfc3339()
                ));
                spec
            },
            "spec.expire_when",
        ),
        (
            "non-positive outer duration",
            {
                let mut spec = sequential();
                spec["expire_when"] = json!("now >= job.started_at + duration('0s')");
                spec
            },
            "spec.expire_when",
        ),
        (
            "non-bool wait",
            with_step(
                sequential(),
                &[0],
                "wait_until",
                json!("board.backlog.count"),
            ),
            "spec.steps.qa.wait_until",
        ),
        (
            "unknown field",
            with_step(sequential(), &[0], "wait_until", json!("self.started")),
            "spec.steps.qa.wait_until",
        ),
        (
            "step status typo",
            with_step(
                sequential(),
                &[1],
                "wait_until",
                json!("self.prev.status == 'complete'"),
            ),
            "spec.steps.security.wait_until",
        ),
        (
            "computed step reference",
            with_step(
                sequential(),
                &[2],
                "wait_until",
                json!("steps[self.status].status == 'done'"),
            ),
            "spec.steps.prepare.wait_until",
        ),
        (
            "forward step reference",
            with_step(
                sequential(),
                &[0],
                "wait_until",
                json!("steps['prepare'].status == 'done'"),
            ),
            "spec.steps.qa.wait_until",
        ),
        (
            "self step reference",
            with_step(
                sequential(),
                &[1],
                "wait_until",
                json!("steps['security'].status == 'done'"),
            ),
            "spec.steps.security.wait_until",
        ),
        (
            "cross-live-branch reference",
            with_step(
                parallel(),
                &[0, 1],
                "wait_until",
                json!("steps['qa'].status == 'done'"),
            ),
            "spec.steps.security.wait_until",
        ),
        (
            "enclosing step reference",
            with_step(
                parallel(),
                &[0, 0],
                "wait_until",
                json!("steps['checks'].status == 'done'"),
            ),
            "spec.steps.qa.wait_until",
        ),
        (
            "unknown step reference",
            with_step(
                sequential(),
                &[2],
                "wait_until",
                json!("steps['nope'].status == 'done'"),
            ),
            "spec.steps.prepare.wait_until",
        ),
        (
            "self.prev on a first step",
            with_step(
                sequential(),
                &[0],
                "wait_until",
                json!("self.prev.status == 'done'"),
            ),
            "spec.steps.qa.wait_until",
        ),
        (
            "unguarded optional fact",
            with_step(
                sequential(),
                &[0],
                "wait_until",
                json!("self.task_id == 'ORB-1'"),
            ),
            "spec.steps.qa.wait_until",
        ),
        (
            "unguarded unpinned revision",
            with_step(
                parallel(),
                &[1],
                "wait_until",
                json!("job.source_revision == 'abc'"),
            ),
            "spec.steps.prepare.wait_until",
        ),
        (
            "result outside reuse_when",
            with_step(
                sequential(),
                &[0],
                "wait_until",
                json!("result.status == 'passed'"),
            ),
            "spec.steps.qa.wait_until",
        ),
        (
            "unpinned result revision",
            with_step(
                with_step(sequential(), &[0], "require_result", Value::Null),
                &[0],
                "reuse_when",
                json!("result.revision == 'abc'"),
            ),
            "spec.steps.qa.reuse_when",
        ),
        (
            "unknown result contract",
            with_step(
                sequential(),
                &[0],
                "require_result",
                json!("release-readiness/v9"),
            ),
            "spec.steps.qa.require_result",
        ),
        (
            "regular expression",
            with_step(sequential(), &[0], "wait_until", json!("'a'.matches('a')")),
            "spec.steps.qa.wait_until",
        ),
        (
            "auto-task target inside a loop",
            json!({"state": "disabled", "expire_when": "now >= job.started_at + duration('1h')",
                   "steps": [{"id": "each", "loop": {"max_iterations": 2,
                              "steps": [{"id": "inner", "target": "auto-task:x"}]}}]}),
            "spec.steps.inner.target",
        ),
        (
            "auto-task target inside a fan_out",
            json!({"state": "disabled", "expire_when": "now >= job.started_at + duration('1h')",
                   "steps": [{"id": "fan", "fan_out": {"items": "{{ input.items }}",
                              "worker": {"id": "inner", "target": "auto-task:x"}},
                              "fan_in": {"join": {"mode": "all"}}}]}),
            "spec.steps.inner.target",
        ),
        (
            "over 32 auto-task steps",
            json!({"state": "disabled", "expire_when": "now >= job.started_at + duration('1h')",
                   "steps": (0..33)
                       .map(|index| json!({"id": format!("s{index}"), "target": "auto-task:x"}))
                       .collect::<Vec<_>>()}),
            "spec.steps",
        ),
    ];
    for (case, spec, clause) in cases {
        assert_eq!(refusal(spec).clause, clause, "{case}");
    }
}

#[test]
fn guarded_optional_facts_and_proven_prev_load() {
    for (path, wait) in [
        (
            &[0][..],
            "has(self.task_id) && self.task_id.startsWith('ORB-')",
        ),
        (
            &[1][..],
            "self.prev.status == 'done' && has(self.prev.end_time) \
                    && now >= self.prev.end_time + duration('30m')",
        ),
        (
            &[2][..],
            "has(steps['qa'].end_time) && now - steps['qa'].end_time <= duration('1h')",
        ),
        (
            &[2][..],
            "board.by_status.backlog == 0 && tasks['ORB-1'].status == 'done'",
        ),
    ] {
        let spec = with_step(sequential(), path, "wait_until", json!(wait));
        check_job_waits(&job(spec), Utc::now())
            .unwrap_or_else(|error| panic!("refused {wait}: {error}"));
    }
}
