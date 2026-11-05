//! A pilot that cannot establish a task's lock footprint keeps the task's
//! assessed tier and pool, and that footprint holds the approval whatever the
//! tier or selectors say [ORB-22153]. ORB-22144 was rated low, the pilot
//! returned `unassessed` with its ledger unread and a consulted rulebook as the
//! only selector, and the apply wrote `unassessed` over the tier and redrew the
//! crew. These scenarios drive the drain's own pilot child through prepare,
//! apply and approval.

use super::*;

const LEDGER_GAP: &str =
    "predecessor ledger ORB-22120 artifact unreadable under the pilot's read authority";
const RULEBOOK: &str = "docs/design/CONVENTIONS.md";

/// A pilot assessment for `task` at `complexity`, with the given evidence
/// gaps, selectors and `utility_warnings`.
fn pilot_says(
    task: &Task,
    complexity: &str,
    gaps: &[&str],
    selectors: &[&str],
    warnings: Value,
) -> Value {
    let mut pilot = assessment(task);
    pilot["context_files_after"] = json!(selectors);
    pilot["recommended_complexity"] = json!(complexity);
    pilot["confidence"] = json!(if gaps.is_empty() { "high" } else { "medium" });
    pilot["evidence_gaps"] = json!(gaps);
    pilot["utility_warnings"] = warnings;
    pilot
}

fn stored(ws: &Workspace, task: &Task) -> Task {
    ws.runtime.get_task(&task.id).unwrap()
}

/// `task`'s hold in a drain selection names `needle` in one of its findings,
/// so an operator reads what to resolve.
fn assert_held_finding(selection: &Value, task: &Task, needle: &str) {
    let named = selection["held"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|held| held["task_id"] == task.id.as_str())
        .flat_map(|held| held["findings"].as_array().into_iter().flatten())
        .any(|finding| {
            finding["text"]
                .as_str()
                .is_some_and(|text| text.contains(needle))
        });
    assert!(named, "{needle:?} in the hold of {}: {selection}", task.id);
}

/// Select `task` for a drain pilot, pilot it with `assessment`, and return the
/// stored task.
fn pilot_once(ws: &Workspace, drain: &str, task: &Task, assessment: Value) -> Task {
    let selection = ws.select(drain);
    assert_task_ids(&selection, &[task]);
    let pilot = ws.pilot_child(drain, &selection["task_ids"]);
    ws.pilot_ok(drain, &pilot, vec![assessment]);
    stored(ws, task)
}

fn an_unresolved_footprint_keeps_the_tier_and_pool_and_holds_approval() {
    let (ws, drain) = harness();
    std::fs::create_dir_all(ws.repo.join("docs/design")).unwrap();
    ws.commit_file(RULEBOOK, "conventions\n", "add conventions");
    let task = ws.readme_task("ledger driven");
    let before = stored(&ws, &task);
    assert_eq!(before.complexity, Some(TaskComplexity::Low));

    // The ORB-22144 output: unassessed, the ledger unread, and the rulebook as
    // the sole selector.
    let after = pilot_once(
        &ws,
        &drain,
        &task,
        pilot_says(
            &task,
            "unassessed",
            &[LEDGER_GAP],
            &[&format!("file:{RULEBOOK}")],
            json!([]),
        ),
    );
    assert_eq!(
        after.complexity,
        Some(TaskComplexity::Low),
        "tier kept: {after:?}"
    );
    assert_eq!(after.crew, before.crew, "pool kept");
    assert_eq!(
        after.crew_source, before.crew_source,
        "pool provenance kept"
    );
    assert_status(&ws, &task, TaskStatus::Proposed);

    let next = ws.select(&drain);
    assert_task_ids(&next, &[]);
    assert_held(&next, &task, "footprint_unresolved");
    assert_held_finding(&next, &task, "Footprint unresolved");
    assert_held_finding(&next, &task, LEDGER_GAP);
    assert_eq!(ws.record(&drain)["approved"], json!([]));
    assert_status(&ws, &task, TaskStatus::Proposed);
}

fn an_identifiable_task_with_readable_evidence_is_rerated_and_approved() {
    let (ws, drain) = harness();
    let task = ws.readme_task("ledger readable");
    // Readable evidence supports an assessed-to-assessed rerating, which
    // applies and promotes as before.
    let after = pilot_once(
        &ws,
        &drain,
        &task,
        pilot_says(&task, "medium", &[], &["file:README.md"], json!([])),
    );
    assert_eq!(after.complexity, Some(TaskComplexity::Medium));
    assert_status(&ws, &task, TaskStatus::Backlog);
}

fn an_identifiable_task_with_unread_evidence_is_assessed_and_held_by_its_declared_finding() {
    let (ws, drain) = harness();
    let task = ws.readme_task("ledger unread");
    // The pilot rates the identifiable repair and declares the unread source
    // as a holding finding. The hold holds whatever the tier and selectors.
    let warning = json!([{
        "text": "Footprint unresolved: the predecessor ledger could not be read.",
        "holds_promotion": true,
    }]);
    let after = pilot_once(
        &ws,
        &drain,
        &task,
        pilot_says(&task, "low", &[LEDGER_GAP], &["file:README.md"], warning),
    );
    assert_eq!(after.complexity, Some(TaskComplexity::Low));
    assert_status(&ws, &task, TaskStatus::Proposed);
    let next = ws.select(&drain);
    assert_held(&next, &task, "warnings");
    assert_eq!(ws.record(&drain)["approved"], json!([]));
    assert_status(&ws, &task, TaskStatus::Proposed);
}

fn a_truly_unidentifiable_task_stays_unassessed_and_held() {
    let (ws, drain) = harness();
    let task = ws.task(
        "unidentifiable",
        &[],
        &["file:README.md"],
        TaskComplexity::Unassessed,
    );
    // The drain holds an unrated task before it pilots anything.
    let selection = ws.select(&drain);
    assert_task_ids(&selection, &[]);
    assert_held(&selection, &task, "unassessed_complexity");

    // An explicit pilot that cannot identify any repair leaves the task
    // unrated and held, with no footprint hold: there is no tier to keep.
    let (run, prepared) = explicit_pilot(&ws, &task);
    let applied = ws.apply_unpromoted_assessment(
        &run,
        &prepared,
        pilot_says(
            &task,
            "unassessed",
            &["no reproduction or failing signature exists to identify the repair"],
            &["file:README.md"],
            json!([]),
        ),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(
        stored(&ws, &task).complexity,
        Some(TaskComplexity::Unassessed)
    );
    assert_status(&ws, &task, TaskStatus::Proposed);
    let next = ws.select(&drain);
    assert_ne!(
        held_reason(&next, &task),
        Some("footprint_unresolved"),
        "{next}"
    );
    assert_held(&next, &task, "unassessed_complexity");
    assert_eq!(ws.record(&drain)["approved"], json!([]));
}

crate::scenario_table!(
    unresolved_footprint_scenarios,
    an_unresolved_footprint_keeps_the_tier_and_pool_and_holds_approval,
    an_identifiable_task_with_readable_evidence_is_rerated_and_approved,
    an_identifiable_task_with_unread_evidence_is_assessed_and_held_by_its_declared_finding,
    a_truly_unidentifiable_task_stays_unassessed_and_held
);
