//! [ORB-20157] The auto-task step shape: its keys parse into typed fields
//! and round-trip, and a key the shape does not admit is refused instead of
//! dropped.

use orbit_types::workflow::{JobV2, JobV2Step};

fn step(yaml: &str) -> Result<JobV2Step, String> {
    serde_yaml::from_str(yaml).map_err(|error| error.to_string())
}

#[test]
fn auto_task_step_fields_parse_and_round_trip() {
    let parsed = step(
        "id: security\n\
         target: auto-task:security-review\n\
         when: '{{ input.run_security }}'\n\
         wait_until: \"self.prev.status == 'done' && board.empty\"\n\
         expire_when: \"now >= self.wait_started_at + duration('8h')\"\n\
         reuse_when: \"result.status == 'passed'\"\n\
         require_result: release-readiness/v1\n",
    )
    .unwrap();
    assert_eq!(parsed.auto_task_name(), Some("security-review"));
    assert_eq!(parsed.when.as_deref(), Some("{{ input.run_security }}"));
    assert_eq!(
        parsed.wait_until.as_deref(),
        Some("self.prev.status == 'done' && board.empty")
    );
    assert_eq!(
        parsed.require_result.as_deref(),
        Some("release-readiness/v1")
    );
    let round_trip: JobV2Step =
        serde_json::from_value(serde_json::to_value(&parsed).unwrap()).unwrap();
    assert_eq!(round_trip, parsed);

    let job: JobV2 = serde_yaml::from_str(
        "state: disabled\n\
         expire_when: \"now >= job.started_at + duration('24h')\"\n\
         steps:\n\
         - id: checks\n\
         \x20 parallel:\n\
         \x20   join: { mode: all }\n\
         \x20   branches:\n\
         \x20   - { id: qa, target: 'auto-task:full-qa-sweep' }\n\
         \x20   - { id: lint, target: 'activity:agent_assess_diff' }\n",
    )
    .unwrap();
    assert!(job.declares_auto_task_steps());
    let ids = job
        .auto_task_steps()
        .iter()
        .map(|step| step.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        ["qa"],
        "nested auto-task steps are found, others are not"
    );
}

#[test]
fn keys_outside_the_auto_task_shape_are_refused() {
    for (yaml, expected) in [
        (
            "id: a\ntarget: activity:agent_assess_diff\nwait_until: board.empty\n",
            "`wait_until` is admitted only on a `target: auto-task:<name>` step",
        ),
        (
            "id: a\nspec: { kind: deterministic, action: noop }\nrequire_result: release-readiness/v1\n",
            "`require_result` is admitted only on a `target: auto-task:<name>` step",
        ),
        (
            "id: a\ntarget: auto-task:qa\nwait_untill: board.empty\n",
            "does not admit `wait_untill`",
        ),
        (
            "id: a\ntarget: auto-task:qa\ndefault_input: { x: 1 }\n",
            "does not admit `default_input`",
        ),
        (
            "id: a\ntarget: auto-task:qa\ntimeout_seconds: 60\n",
            "does not admit `timeout_seconds`",
        ),
        (
            "id: a\ntarget: auto-task:qa\nretry: { max_attempts: 2 }\n",
            "does not admit `retry`",
        ),
        (
            "id: a\ntarget: auto-task:qa\nrecovery_activity: fix\n",
            "does not admit `recovery_activity`",
        ),
        (
            "id: a\ntarget: 'auto-task:'\n",
            "does not name an auto-task",
        ),
        (
            "id: a\ntarget: auto-task:Not_Valid\n",
            "does not name an auto-task",
        ),
        (
            "id: a\ntarget: auto-task:qa\nwait_until: 3\n",
            "`wait_until`",
        ),
    ] {
        let error = step(yaml).expect_err(yaml);
        assert!(error.contains(expected), "{yaml}: {error}");
    }
}

#[test]
fn ordinary_steps_keep_their_shape() {
    // An activity step still tolerates the keys it always tolerated.
    let parsed = step(
        "id: a\ntarget: activity:agent_assess_diff\ntimeout_seconds: 30\nsession: s\nnote: kept-quiet\n",
    )
    .unwrap();
    assert_eq!(parsed.auto_task_name(), None);
    assert!(parsed.wait_until.is_none());
}
