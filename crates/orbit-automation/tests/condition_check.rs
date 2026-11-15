#![allow(
    clippy::unwrap_used,
    reason = "predicate fixtures panic when a board or clause cannot be built, which is a broken test"
)]

//! Load-time condition checks and their evaluation, through the public
//! `check_condition` and `ConditionEvaluator` boundaries.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_automation::condition::{
    BoardFacts, ConditionCarrier, ConditionEvaluator, ConditionFacts, ConditionReads, GUARD_FALSE,
    GUARD_UNKNOWN, GuardVerdict, JobWaitFacts, SelfFacts, Truth, check_condition, check_guard,
    check_job_waits,
};
use orbit_types::workflow::automation::ConditionTrigger;

fn task_condition(when: &str) -> ConditionTrigger {
    serde_json::from_value(serde_json::json!({"scope": "task", "when": when})).unwrap()
}

fn workspace_condition(when: &str) -> ConditionTrigger {
    serde_json::from_value(serde_json::json!({"scope": "workspace", "when": when})).unwrap()
}

/// Facts for a healthy workspace: an empty eligible queue, no open runs and no
/// drain running. Like the explain gatherer, it binds only the board fields the
/// checked condition reads, and `open_runs` and `drain_running` load together.
fn healthy_board_facts(reads: &ConditionReads) -> ConditionFacts {
    let mut board = BoardFacts::default();
    if reads.board.contains("eligible") {
        board.eligible = Some(0);
    }
    if reads.board.contains("open_runs") || reads.board.contains("drain_running") {
        board.open_runs = Some(0);
        board.drain_running = Some(false);
    }
    ConditionFacts {
        now: Utc::now(),
        board,
        tasks: BTreeMap::new(),
        self_facts: SelfFacts::default(),
        auto_tasks: BTreeMap::new(),
    }
}

#[test]
fn contradictory_status_prefilter_is_refused() {
    // No status satisfies these conjuncts. Loading them would leave the
    // prefilter empty, which lists every task as a subject instead of none.
    for when in [
        "task.status == 'done' && task.status == 'blocked'",
        "task.status in [] && 'release' in task.tags",
    ] {
        let result = check_condition(&task_condition(when), ConditionCarrier::Routine);
        assert!(
            result.is_err(),
            "accepted a prefilter no task can satisfy: {when}"
        );
    }
}

#[test]
fn satisfiable_status_prefilters_still_load() {
    for when in [
        "task.status == 'done' && task.status in ['done', 'blocked'] && 'release' in task.tags",
        "'release' in task.tags && task.status != 'done'",
    ] {
        let result = check_condition(&task_condition(when), ConditionCarrier::Routine);
        assert!(result.is_ok(), "refused a satisfiable prefilter: {when}");
    }
}

#[test]
fn board_presence_tests_record_their_field_as_a_read() {
    for (when, field) in [
        ("has(board.eligible)", "eligible"),
        ("has(board.open_runs)", "open_runs"),
        ("has(board.drain_running)", "drain_running"),
    ] {
        let checked = check_condition(&workspace_condition(when), ConditionCarrier::Routine)
            .unwrap_or_else(|error| panic!("refused {when}: {error}"));
        assert!(
            checked.reads.board.contains(field),
            "{when} did not record board.{field} as a read"
        );
    }
}

#[test]
fn board_presence_test_matches_direct_read_on_healthy_facts() {
    // Each direct read is true on healthy facts, so its presence-only form must
    // be true too. Before the fix the presence form read as false: the field was
    // never loaded, so `has()` found no fact.
    for (presence, direct) in [
        (
            "has(board.eligible)",
            "has(board.eligible) && board.eligible.count >= 0",
        ),
        (
            "has(board.open_runs)",
            "has(board.open_runs) && board.open_runs.count >= 0",
        ),
        (
            "has(board.drain_running)",
            "has(board.drain_running) && !board.drain_running",
        ),
    ] {
        let direct_condition =
            check_condition(&workspace_condition(direct), ConditionCarrier::Routine).unwrap();
        let presence_condition =
            check_condition(&workspace_condition(presence), ConditionCarrier::Routine).unwrap();
        let expected = ConditionEvaluator::new(&healthy_board_facts(&direct_condition.reads))
            .truth(&direct_condition.when, None);
        let actual = ConditionEvaluator::new(&healthy_board_facts(&presence_condition.reads))
            .truth(&presence_condition.when, None);
        assert_eq!(
            expected,
            Truth::True,
            "direct read is not true on healthy facts: {direct}"
        );
        assert_eq!(
            actual, expected,
            "{presence} differs from its direct-read form on healthy facts"
        );
    }
}

fn checked_when(when: &str) -> orbit_automation::condition::CheckedExpression {
    check_condition(&workspace_condition(when), ConditionCarrier::Routine)
        .unwrap_or_else(|error| panic!("refused condition {when}: {error}"))
        .when
}

fn checked_guard_when(when: &str) -> orbit_automation::condition::CheckedExpression {
    check_guard(when, "trigger.when", ConditionCarrier::AutoTask)
        .unwrap_or_else(|error| panic!("refused guard {when}: {error}"))
        .when
}

fn checked_wait(when: &str) -> orbit_automation::condition::CheckedExpression {
    let spec = serde_json::json!({
        "state": "disabled",
        "expire_when": "now >= job.started_at + duration('1h')",
        "steps": [{
            "id": "qa",
            "target": "auto-task:fixture",
            "wait_until": when
        }]
    });
    let job = serde_json::from_value(spec).unwrap();
    check_job_waits(&job, Utc::now())
        .unwrap_or_else(|error| panic!("refused wait {when}: {error}"))
        .steps
        .into_iter()
        .next()
        .and_then(|step| step.wait_until)
        .unwrap_or_else(|| panic!("wait {when} did not check"))
}

fn condition_facts(board: BoardFacts, self_facts: SelfFacts) -> ConditionFacts {
    ConditionFacts {
        now: Utc::now(),
        board,
        tasks: BTreeMap::new(),
        self_facts,
        auto_tasks: BTreeMap::new(),
    }
}

fn present_board(eligible: i64) -> BoardFacts {
    BoardFacts {
        eligible: Some(eligible),
        backlog: Some(0),
        in_progress: Some(0),
        blocked: Some(0),
        review: Some(0),
        open_runs: Some(0),
        drain_running: Some(false),
        ..BoardFacts::default()
    }
}

fn job_facts(
    board: BoardFacts,
    board_empty: Option<bool>,
    task_id: Option<String>,
) -> JobWaitFacts {
    let now = Utc::now();
    JobWaitFacts {
        now,
        board,
        board_empty,
        tasks: BTreeMap::new(),
        job_started_at: now,
        job_source_revision: None,
        wait_started_at: now,
        status: "waiting".into(),
        task_id,
        prev: None,
        steps: BTreeMap::new(),
        result: None,
    }
}

fn assert_truth(when: &str, truth: &Truth, label: &str) {
    assert_eq!(truth.label(), label, "{when} evaluated to {truth:?}");
}

/// An unread board aggregate is unknown, including `has()` and its negation.
/// A present aggregate stays boolean, and a genuinely optional self or step
/// field stays absent. Before the repair, `BoardFacts::default()` made
/// `has(board.eligible)` false and `!has(board.eligible)` true, so a guard
/// opened and a job wait was satisfied on a read failure.
#[test]
fn unavailable_board_presence_is_unknown_and_optional_absence_stays_boolean() {
    let unread = BoardFacts::default();
    let unread_facts = condition_facts(unread.clone(), SelfFacts::default());
    let unread_wait = job_facts(unread.clone(), None, None);
    for when in [
        "has(board.eligible)",
        "!has(board.eligible)",
        "board.eligible.empty",
        "board.eligible.count == 0",
        "!has(board.open_runs)",
        "!has(board.drain_running)",
        "!has(board.backlog)",
        "true || !has(board.eligible)",
    ] {
        let truth = ConditionEvaluator::new(&unread_facts).truth(&checked_when(when), None);
        assert_truth(when, &truth, "unknown");
        let tree = ConditionEvaluator::new(&unread_facts).clause_tree(&checked_when(when), None);
        assert_eq!(tree.value, "unknown", "explain clause for {when}");
        let guard_truth =
            ConditionEvaluator::new(&unread_facts).truth(&checked_guard_when(when), None);
        assert_truth(when, &guard_truth, "unknown");
        assert!(
            matches!(
                GuardVerdict::from_truth(when, guard_truth),
                GuardVerdict::Closed {
                    reason: GUARD_UNKNOWN,
                    ..
                }
            ),
            "{when} opened a guard from an unread board"
        );
        let wait_truth =
            ConditionEvaluator::for_job_wait(&unread_wait).truth(&checked_wait(when), None);
        assert_ne!(
            wait_truth,
            Truth::True,
            "{when} satisfied a wait through an unread board"
        );
        assert_truth(when, &wait_truth, "unknown");
    }

    // The status map is bound even when counts were not read. Unread
    // aggregates must not make every board read unknown.
    let map_present =
        ConditionEvaluator::new(&unread_facts).truth(&checked_when("has(board.by_status)"), None);
    assert_truth("has(board.by_status)", &map_present, "true");

    let present = present_board(4);
    let present_facts = condition_facts(present.clone(), SelfFacts::default());
    let present_wait = job_facts(present.clone(), Some(true), None);
    for (when, label) in [
        ("has(board.eligible)", "true"),
        ("!has(board.eligible)", "false"),
        ("board.eligible.empty", "false"),
        ("board.eligible.count == 4", "true"),
        ("has(board.open_runs)", "true"),
        ("has(board.drain_running)", "true"),
        ("!has(board.drain_running)", "false"),
        ("board.backlog.empty", "true"),
    ] {
        let truth = ConditionEvaluator::new(&present_facts).truth(&checked_when(when), None);
        assert_truth(when, &truth, label);
        let guard_truth =
            ConditionEvaluator::new(&present_facts).truth(&checked_guard_when(when), None);
        assert_truth(when, &guard_truth, label);
        let verdict = GuardVerdict::from_truth(when, guard_truth);
        if label == "true" {
            assert!(
                matches!(verdict, GuardVerdict::Open),
                "{when} did not open on a present aggregate: {verdict:?}"
            );
        } else {
            assert!(
                matches!(
                    verdict,
                    GuardVerdict::Closed {
                        reason: GUARD_FALSE,
                        ..
                    }
                ),
                "{when} was not a false guard on a present aggregate: {verdict:?}"
            );
        }
        let wait_truth =
            ConditionEvaluator::for_job_wait(&present_wait).truth(&checked_wait(when), None);
        assert_truth(when, &wait_truth, label);
    }

    let empty_unread = ConditionEvaluator::for_job_wait(&unread_wait)
        .truth(&checked_wait("!has(board.empty)"), None);
    assert_truth("!has(board.empty)", &empty_unread, "unknown");
    let empty_present =
        ConditionEvaluator::for_job_wait(&job_facts(present_board(0), Some(true), None))
            .truth(&checked_wait("board.empty"), None);
    assert_truth("board.empty", &empty_present, "true");
    let empty_occupied =
        ConditionEvaluator::for_job_wait(&job_facts(present_board(0), Some(false), None))
            .truth(&checked_wait("board.empty"), None);
    assert_truth("board.empty occupied", &empty_occupied, "false");

    // Optional absence stays boolean on the same unread board. An unreadable
    // accepted-result history stays unknown, and a recorded fire stays present.
    for (when, label) in [
        ("has(self.last_fired)", "false"),
        ("!has(self.last_fired)", "true"),
    ] {
        let truth = ConditionEvaluator::new(&unread_facts).truth(&checked_when(when), None);
        assert_truth(when, &truth, label);
    }
    let fired = SelfFacts {
        last_fired: Some(Utc::now()),
        ..SelfFacts::default()
    };
    let fired_facts = condition_facts(present_board(0), fired);
    let fired_truth =
        ConditionEvaluator::new(&fired_facts).truth(&checked_when("has(self.last_fired)"), None);
    assert_truth("has(self.last_fired) present", &fired_truth, "true");
    let unreadable = SelfFacts {
        last_succeeded_unreadable: Some("history does not load".into()),
        ..SelfFacts::default()
    };
    let unreadable_facts = condition_facts(present_board(0), unreadable);
    // Only an auto-task self carries last_succeeded_at. An unreadable history
    // stays unknown under has() and its negation; it is not "never succeeded".
    for when in [
        "has(self.last_succeeded_at)",
        "!has(self.last_succeeded_at)",
    ] {
        let expression = check_condition(&workspace_condition(when), ConditionCarrier::AutoTask)
            .unwrap_or_else(|error| panic!("refused {when}: {error}"))
            .when;
        let truth = ConditionEvaluator::new(&unreadable_facts).truth(&expression, None);
        assert_truth(when, &truth, "unknown");
    }

    let absent_task = ConditionEvaluator::for_job_wait(&unread_wait)
        .truth(&checked_wait("has(self.task_id)"), None);
    assert_truth("has(self.task_id)", &absent_task, "false");
    let negated_task = ConditionEvaluator::for_job_wait(&unread_wait)
        .truth(&checked_wait("!has(self.task_id)"), None);
    assert_truth("!has(self.task_id)", &negated_task, "true");
    let bound_task = ConditionEvaluator::for_job_wait(&job_facts(
        present_board(0),
        Some(true),
        Some("task-1".into()),
    ))
    .truth(&checked_wait("has(self.task_id)"), None);
    assert_truth("has(self.task_id) bound", &bound_task, "true");
}

#[test]
fn board_status_map_reads_require_stored_status_spellings() {
    for when in [
        "board.by_status.typo == 0",
        "board.by_status['typo'] == 0",
        "has(board.by_status.typo)",
    ] {
        let error = match check_condition(&workspace_condition(when), ConditionCarrier::Routine) {
            Ok(_) => panic!("accepted unknown board status key: {when}"),
            Err(error) => error,
        };
        assert_eq!(
            error.clause, "trigger.condition.when",
            "unknown status key was reported against the wrong clause: {when}"
        );
        assert!(
            error.position.is_some(),
            "unknown status key had no source position: {when}"
        );
        assert!(
            error.message.contains("unknown task status 'typo'"),
            "unknown status key had no status-spelling error: {when}: {error}"
        );
    }

    for when in [
        "board.by_status.in_progress == 0",
        "board.by_status['in_progress'] == 0",
        "has(board.by_status.in_progress)",
    ] {
        check_condition(&workspace_condition(when), ConditionCarrier::Routine)
            .unwrap_or_else(|error| panic!("refused stored status key in {when}: {error}"));
    }
}

#[test]
fn duration_negation_is_refused_at_load() {
    // cel 0.15 has no duration negation overload, so the checker must refuse
    // unary '-' on a duration at load instead of accepting an expression that
    // can only evaluate to unknown. The `true || ...` form pins the absorbed
    // case: cel's `||` would return true despite the missing overload.
    for carrier in [ConditionCarrier::Routine, ConditionCarrier::AutoTask] {
        for when in [
            "-duration('1h') < duration('0s')",
            "true || -duration('1h') < duration('0s')",
        ] {
            let error = match check_condition(&workspace_condition(when), carrier) {
                Ok(_) => panic!("accepted duration negation for {carrier:?}: {when}"),
                Err(error) => error,
            };
            assert_eq!(
                error.clause,
                format!("{}.when", carrier.clause()),
                "wrong clause for {when}"
            );
            assert!(
                error.position.is_some(),
                "missing source position for {when}: {error}"
            );
            assert!(
                error.message.contains("cannot negate duration"),
                "unexpected message for {when}: {error}"
            );
        }
    }
}

#[test]
fn supported_negation_still_checks_and_evaluates() {
    // Numeric negation and signed duration string literals are supported and
    // must keep loading and evaluating.
    for carrier in [ConditionCarrier::Routine, ConditionCarrier::AutoTask] {
        for when in ["-1 < 0", "duration('-1h') < duration('0s')"] {
            let checked = check_condition(&workspace_condition(when), carrier)
                .unwrap_or_else(|error| panic!("refused {when} for {carrier:?}: {error}"));
            let truth = ConditionEvaluator::new(&healthy_board_facts(&checked.reads))
                .truth(&checked.when, None);
            assert_eq!(truth, Truth::True, "{when} did not evaluate true");
        }
    }
}

/// A task condition that would match its own template's output is a load
/// error naming the definition and `schedule.condition.when` (§8). The
/// template below mints a `backlog` task tagged `release`, which its own
/// condition matches under both origin shapes.
#[test]
fn self_matching_task_condition_is_a_load_error() {
    let root = tempfile::tempdir().unwrap();
    write_condition_definition(
        root.path(),
        "selfmatch",
        "task.status == 'backlog' && 'release' in task.tags",
        &["release"],
    );

    let collection = orbit_automation::auto_tasks::loader::collect_auto_tasks(root.path());
    assert!(
        collection.definitions.is_empty(),
        "a self-matching definition loaded: {:?}",
        collection.definitions
    );
    assert_eq!(collection.errors.len(), 1, "{:?}", collection.errors);
    let message = &collection.errors[0].message;
    assert!(
        message.contains("selfmatch"),
        "load error names no definition: {message}"
    );
    assert!(
        message.contains("schedule.condition.when"),
        "load error names no clause: {message}"
    );
}

/// A task condition its own template cannot satisfy still loads unchanged.
#[test]
fn non_matching_task_condition_still_loads() {
    let root = tempfile::tempdir().unwrap();
    write_condition_definition(
        root.path(),
        "calm",
        "task.status == 'backlog' && 'release' in task.tags",
        &[],
    );

    let collection = orbit_automation::auto_tasks::loader::collect_auto_tasks(root.path());
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    assert_eq!(collection.definitions.len(), 1);
    assert_eq!(collection.definitions[0].definition.name, "calm");
}

/// A board read is unknown at load, so a task condition that needs the board
/// cannot match its own template output and still loads.
#[test]
fn board_read_self_match_evaluates_unknown_and_loads() {
    let root = tempfile::tempdir().unwrap();
    write_condition_definition(
        root.path(),
        "boarded",
        "task.status == 'backlog' && board.eligible.empty",
        &[],
    );

    let collection = orbit_automation::auto_tasks::loader::collect_auto_tasks(root.path());
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    assert_eq!(collection.definitions.len(), 1);
    assert_eq!(collection.definitions[0].definition.name, "boarded");
}

/// An origin-dependent condition is true under only one mint shape, so the
/// both-shapes rule keeps it loading.
#[test]
fn origin_dependent_task_condition_still_loads() {
    let root = tempfile::tempdir().unwrap();
    write_condition_definition(
        root.path(),
        "originbound",
        "task.status == 'backlog' && !has(task.origin)",
        &[],
    );

    let collection = orbit_automation::auto_tasks::loader::collect_auto_tasks(root.path());
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    assert_eq!(collection.definitions.len(), 1);
    assert_eq!(collection.definitions[0].definition.name, "originbound");
}

/// A template naming the retired `system` crew mints an unassigned task under
/// the default crew set ([ORB-15663]), so a condition reading the absent crew
/// matches its own template output and is a load error naming the definition
/// and `schedule.condition.when`.
#[test]
fn retired_system_crew_absent_subject_is_a_load_error() {
    let root = tempfile::tempdir().unwrap();
    write_condition_definition_with_crew(
        root.path(),
        "retiredabsent",
        "task.status == 'backlog' && !has(task.crew)",
        &[],
        Some("system"),
    );

    let collection = orbit_automation::auto_tasks::loader::collect_auto_tasks(root.path());
    assert!(
        collection.definitions.is_empty(),
        "a retired-crew self-matching definition loaded: {:?}",
        collection.definitions
    );
    assert_eq!(collection.errors.len(), 1, "{:?}", collection.errors);
    let message = &collection.errors[0].message;
    assert!(
        message.contains("retiredabsent"),
        "load error names no definition: {message}"
    );
    assert!(
        message.contains("schedule.condition.when"),
        "load error names no clause: {message}"
    );
}

/// A condition that can only match when the minted task carries the retired
/// name still loads: the mint drops that crew, so `has(task.crew)` is false
/// on its own template output.
#[test]
fn retired_system_crew_equality_still_loads() {
    let root = tempfile::tempdir().unwrap();
    write_condition_definition_with_crew(
        root.path(),
        "retiredkept",
        "task.status == 'backlog' && has(task.crew) && task.crew == 'system'",
        &[],
        Some("system"),
    );

    let collection = orbit_automation::auto_tasks::loader::collect_auto_tasks(root.path());
    assert!(collection.errors.is_empty(), "{:?}", collection.errors);
    assert_eq!(collection.definitions.len(), 1);
    assert_eq!(collection.definitions[0].definition.name, "retiredkept");
}

fn write_condition_definition(root: &std::path::Path, name: &str, when: &str, tags: &[&str]) {
    write_condition_definition_with_crew(root, name, when, tags, None);
}

fn write_condition_definition_with_crew(
    root: &std::path::Path,
    name: &str,
    when: &str,
    tags: &[&str],
    crew: Option<&str>,
) {
    use std::fmt::Write as _;

    let dir = orbit_automation::auto_tasks::loader::auto_tasks_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    let mut tag_list = String::new();
    for (index, tag) in tags.iter().enumerate() {
        if index > 0 {
            tag_list.push_str(", ");
        }
        let _ = write!(tag_list, "'{tag}'");
    }
    let tags_yaml = if tags.is_empty() {
        "  tags: []".to_string()
    } else {
        format!("  tags: [{tag_list}]")
    };
    let crew_yaml = crew
        .map(|crew| format!("  crew: {crew}\n"))
        .unwrap_or_default();
    std::fs::write(
        dir.join(format!("{name}.yaml")),
        format!(
            "schemaVersion: 1\nname: {name}\nschedule:\n  condition:\n    scope: task\n    when: \"{when}\"\ntemplate:\n  title: Fixture {name}\n{tags_yaml}\n{crew_yaml}"
        ),
    )
    .unwrap();
}

#[test]
fn named_auto_task_history_reads_check_at_load() {
    // [ORB-22676] A literal name loads in either carrier and is recorded as a
    // read; its optional field needs has() like any other.
    for carrier in [ConditionCarrier::Routine, ConditionCarrier::AutoTask] {
        let checked = check_condition(
            &workspace_condition(
                "!has(auto_tasks['release-prep'].last_succeeded_at) \
                 || now - auto_tasks['release-prep'].last_succeeded_at > duration('24h')",
            ),
            carrier,
        )
        .unwrap();
        assert_eq!(
            checked.reads.auto_task_histories.iter().collect::<Vec<_>>(),
            ["release-prep"]
        );
    }
    for (when, refusal) in [
        (
            "now - auto_tasks['release-prep'].last_succeeded_at > duration('24h')",
            "is optional",
        ),
        (
            "now > auto_tasks[board.backlog.count > 0 ? 'a' : 'b'].last_succeeded_at",
            "string literal",
        ),
        (
            "has(auto_tasks['Release Prep'].last_succeeded_at)",
            "auto-task name",
        ),
        (
            "has(auto_tasks.release.last_succeeded_at)",
            "string-literal index",
        ),
    ] {
        let error = check_condition(&workspace_condition(when), ConditionCarrier::Routine)
            .unwrap_err()
            .to_string();
        assert!(error.contains(refusal), "{when}: {error}");
    }
}
