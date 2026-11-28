//! Pilot notes in an assessment [ORB-29588]: a note the pilot returns under a
//! label the workspace mapped is checked by apply, kept in the stored
//! assessment and read from the task, never as a comment. A note that fails a
//! check is dropped with its reason and the rest of the assessment applies. A
//! note is inert: it decides nothing and reaches no prompt.

use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::Dispatch;
use orbit_types::tool::{McpCapability, ToolSessionContext};

use super::*;

const CONFIG: &str = "[crews.fixture]\nmodel = \"fixture-model\"\n\
    provider = \"codex\"\nbackend = \"cli\"\n\
    [workflow]\ndefault_crew = \"fixture\"\n\
    [workflow.pilot_notes.review_focus]\ninstruction = \"docs/review-focus.md\"\n\
    [workflow.pilot_notes.risk]\ninstruction = \"docs/risk.md\"\n\
    [workflow.pilot_notes.docs]\ninstruction = \"docs/docs.md\"\n\
    [workflow.pilot_notes.perf]\ninstruction = \"docs/perf.md\"\n";

const FOCUS: &str = "The retry loop in claim settlement; the lock order in apply.";

impl Workspace {
    fn notes_task(&self, title: &str) -> Task {
        self.recheck_task(title)
    }

    /// Pilot `task` once, returning `notes` (or none) in its assessment.
    fn pilot_with_notes(&self, task: &Task, notes: Option<Value>) -> Value {
        let prepared = self.prepare(&[&task.id]);
        let applied = self.apply_tasks(&prepared, vec![assessment(&task.id, notes)]);
        assert_eq!(applied["applied_count"], 1, "{applied}");
        applied
    }

    fn show(&self, task: &Task) -> Value {
        self.runtime
            .run_tool("orbit.task.show", json!({"id": task.id}))
            .unwrap()
    }
}

fn workspace() -> Workspace {
    let workspace = Workspace::with_config(CONFIG);
    for (file, text) in [
        (
            "review-focus",
            "Name the code a reviewer should read first.\n",
        ),
        ("risk", "Name the riskiest change.\n"),
        ("docs", "Name the docs the change touches.\n"),
        ("perf", "Name any hot path the change touches.\n"),
    ] {
        workspace.commit_file(&format!("docs/{file}.md"), text, "add note instruction");
    }
    workspace
}

fn instruction_sha(text: &str) -> String {
    orbit_common::security::release::sha256_hex(text.as_bytes())
}

fn assessment(task_id: &str, notes: Option<Value>) -> Value {
    let mut assessment = json!({
        "task_id": task_id, "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "medium", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Inspect the persisted scope.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "surface_warnings": [], "utility_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    if let Some(notes) = notes {
        assessment["notes"] = notes;
    }
    assessment
}

fn note(label: &str, text: &str) -> Value {
    json!({"label": label, "text": text})
}

fn a_note_under_a_mapped_label_is_stored_and_read_without_a_comment() {
    let workspace = workspace();
    let task = workspace.notes_task("noted");
    assert!(workspace.show(&task).get("pilot_notes").is_none());
    assert!(
        workspace
            .runtime
            .run_tool(
                "orbit.task.show",
                json!({"id": task.id, "fields": ["pilot_notes"]})
            )
            .unwrap()["pilot_notes"]
            .is_null()
    );

    workspace.pilot_with_notes(&task, Some(json!([note("review_focus", FOCUS)])));

    let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
    assert_eq!(comments.len(), 1, "only the pilot receipt: {comments:?}");
    assert!(!comments[0].message.contains(FOCUS), "{comments:?}");
    let (operation_id, audit) = workspace.latest_receipt(&task.id);
    let stored = &audit["assessment"]["notes"][0];
    assert_eq!(stored["label"], "review_focus", "{audit}");
    assert_eq!(stored["text"], FOCUS, "{audit}");
    assert_eq!(
        stored["instruction"]["path"], "docs/review-focus.md",
        "{audit}"
    );
    assert_eq!(
        stored["instruction"]["sha256"],
        instruction_sha("Name the code a reviewer should read first.\n"),
        "{audit}"
    );
    assert_eq!(
        stored["instruction"]["revision"],
        workspace.git(&["rev-parse", "HEAD"]).trim(),
        "{audit}"
    );
    assert!(
        audit["assessment"].get("notes_dropped").is_none(),
        "{audit}"
    );

    let shown = workspace.show(&task);
    let read = &shown["pilot_notes"];
    assert_eq!(read.as_array().unwrap().len(), 1, "{shown}");
    assert_eq!(read[0]["label"], "review_focus", "{read}");
    assert_eq!(read[0]["text"], FOCUS, "{read}");
    assert_eq!(
        read[0]["instruction_sha256"],
        instruction_sha("Name the code a reviewer should read first.\n"),
        "{read}"
    );
    assert_eq!(read[0]["operation_id"], operation_id, "{read}");
    assert!(read[0]["at"].is_string(), "{read}");
    assert_eq!(shown["comments"].as_array().unwrap().len(), 1, "{shown}");

    // The list returns the field only when `fields` asks for it.
    let listed = workspace
        .runtime
        .run_tool("orbit.task.list", json!({"fields": ["id", "pilot_notes"]}))
        .unwrap();
    assert_eq!(
        listed["tasks"][0]["pilot_notes"][0]["text"], FOCUS,
        "{listed}"
    );
    let listed = workspace
        .runtime
        .run_tool("orbit.task.list", json!({"fields": ["id", "status"]}))
        .unwrap();
    assert!(listed["tasks"][0].get("pilot_notes").is_none(), "{listed}");
    let listed = workspace
        .runtime
        .run_tool("orbit.task.list", json!({}))
        .unwrap();
    assert!(!listed.to_string().contains(FOCUS), "{listed}");
}

fn a_note_that_fails_a_check_is_dropped_and_the_assessment_applies() {
    let workspace = workspace();
    let long = "x".repeat(1025);
    let cases: Vec<(&str, Value, &str)> = vec![
        ("unmapped", note("unmapped", "text"), "unknown_label"),
        ("empty", note("risk", "  \n\t "), "empty"),
        ("large", note("risk", &long), "too_large"),
        ("control", note("risk", "bell\u{7}here"), "not_plain_text"),
        ("escape", note("risk", "esc\u{1b}[31m"), "not_plain_text"),
        ("carriage return", note("risk", "a\rb"), "not_plain_text"),
    ];
    for (name, bad, reason) in cases {
        let task = workspace.notes_task(name);
        let good = note("review_focus", "Kept note.\nWith a second line\tand a tab.");
        workspace.pilot_with_notes(&task, Some(json!([good.clone(), bad])));
        let (_, audit) = workspace.latest_receipt(&task.id);
        assert_eq!(audit["assessment"]["disposition"], "selectors", "{name}");
        assert_eq!(
            audit["assessment"]["notes_dropped"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "{name}: {audit}"
        );
        assert_eq!(
            audit["assessment"]["notes_dropped"][0]["reason"], reason,
            "{name}"
        );
        let kept = audit["assessment"]["notes"].as_array().unwrap();
        assert_eq!(kept.len(), 1, "{name}: {audit}");
        assert_eq!(kept[0]["text"], good["text"], "{name}");
        // Neither a hold nor a correction round: the task is applied as a
        // task without notes would be.
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().context_files,
            ["file:README.md"],
            "{name}"
        );
        assert_eq!(
            workspace.runtime.get_task_comments(&task.id).unwrap().len(),
            1
        );
    }

    // A boundary text of exactly 1 KiB is kept.
    let task = workspace.notes_task("boundary");
    workspace.pilot_with_notes(&task, Some(json!([note("risk", &"y".repeat(1024))])));
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(audit["assessment"]["notes"][0]["text"], "y".repeat(1024));
    assert!(
        audit["assessment"].get("notes_dropped").is_none(),
        "{audit}"
    );
}

fn a_repeated_label_keeps_the_first_note_and_drops_the_repeat() {
    let workspace = workspace();
    let task = workspace.notes_task("duplicate");
    workspace.pilot_with_notes(
        &task,
        Some(json!([note("risk", "first"), note("risk", "second")])),
    );
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(
        audit["assessment"]["notes"],
        json!([{
            "label": "risk", "text": "first",
            "instruction": audit["assessment"]["notes"][0]["instruction"],
        }])
    );
    assert_eq!(
        audit["assessment"]["notes_dropped"],
        json!([{"label": "risk", "reason": "duplicate_label"}])
    );
}

fn the_fifth_note_of_an_entry_is_dropped_whatever_its_label() {
    let workspace = workspace();
    let task = workspace.notes_task("crowded");
    workspace.pilot_with_notes(
        &task,
        Some(json!([
            note("review_focus", "one"),
            note("risk", "two"),
            note("docs", "three"),
            note("perf", "four"),
            note("review_focus", "five"),
            note("elsewhere", "six"),
        ])),
    );
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(
        audit["assessment"]["notes"].as_array().unwrap().len(),
        4,
        "{audit}"
    );
    assert_eq!(
        audit["assessment"]["notes_dropped"],
        json!([
            {"label": "review_focus", "reason": "too_many"},
            {"label": "elsewhere", "reason": "too_many"},
        ]),
        "{audit}"
    );
}

fn a_malformed_notes_field_never_fails_the_pilot() {
    let workspace = workspace();
    for (index, notes) in [
        json!("not a list"),
        json!(null),
        json!([null, 3, {"label": 7, "text": "x"}, {"label": "risk"}]),
    ]
    .into_iter()
    .enumerate()
    {
        let task = workspace.notes_task(&format!("malformed {index}"));
        workspace.pilot_with_notes(&task, Some(notes));
        let (_, audit) = workspace.latest_receipt(&task.id);
        assert!(audit["assessment"].get("notes").is_none(), "{audit}");
        assert_eq!(workspace.show(&task)["pilot_notes"], json!([]), "{index}");
    }
}

fn a_note_and_a_pilot_supplied_drop_record_cannot_be_forged() {
    let workspace = workspace();
    let task = workspace.notes_task("forged");
    let prepared = workspace.prepare(&[&task.id]);
    let mut result = assessment(&task.id, None);
    result["notes_dropped"] = json!([{"label": "made_up", "reason": "forged"}]);
    result["notes"] = json!([{
        "label": "risk", "text": "kept",
        "instruction": {"path": "elsewhere.md", "revision": "x", "sha256": "forged"},
    }]);
    workspace.apply_tasks(&prepared, vec![result]);
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert!(
        audit["assessment"].get("notes_dropped").is_none(),
        "{audit}"
    );
    assert_eq!(
        audit["assessment"]["notes"][0]["instruction"]["path"], "docs/risk.md",
        "{audit}"
    );
}

fn a_note_without_mapped_labels_is_dropped_for_a_workspace_that_maps_none() {
    let workspace = Workspace::new();
    let task = workspace.notes_task("unmapped workspace");
    workspace.pilot_with_notes(&task, Some(json!([note("review_focus", FOCUS)])));
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert!(audit["assessment"].get("notes").is_none(), "{audit}");
    assert_eq!(
        audit["assessment"]["notes_dropped"],
        json!([{"label": "review_focus", "reason": "unknown_label"}])
    );
    assert_eq!(workspace.show(&task)["pilot_notes"], json!([]));
}

fn the_latest_assessment_is_the_truth_for_a_tasks_notes() {
    let workspace = workspace();
    let task = workspace.notes_task("replaced");
    workspace.pilot_with_notes(
        &task,
        Some(json!([
            note("review_focus", FOCUS),
            note("risk", "old risk")
        ])),
    );
    assert_eq!(
        workspace.show(&task)["pilot_notes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // A later pilot that names only one label replaces the notes as a whole.
    workspace.pilot_with_notes(&task, Some(json!([note("risk", "new risk")])));
    let read = workspace.show(&task)["pilot_notes"].clone();
    assert_eq!(read.as_array().unwrap().len(), 1, "{read}");
    assert_eq!(read[0]["text"], "new risk", "{read}");

    // A later pilot that returns none leaves none.
    workspace.pilot_with_notes(&task, None);
    assert_eq!(workspace.show(&task)["pilot_notes"], json!([]));
    let listed = workspace
        .runtime
        .run_tool("orbit.task.list", json!({"fields": ["id", "pilot_notes"]}))
        .unwrap();
    assert_eq!(listed["tasks"][0]["pilot_notes"], json!([]), "{listed}");
}

fn an_unreadable_assessment_shows_no_notes_and_leaves_other_tasks_readable() {
    let workspace = workspace();
    let broken = workspace.notes_task("broken");
    let sound = workspace.notes_task("sound");
    workspace.pilot_with_notes(&broken, Some(json!([note("risk", "lost")])));
    workspace.pilot_with_notes(&sound, Some(json!([note("risk", "kept")])));
    let comments = workspace.runtime.get_task_comments(&broken.id).unwrap();
    let body: Value = serde_json::from_str(comments[0].receipt().unwrap().body).unwrap();
    crate::artifact_tools::drop_artifact_manifest_entry(
        &workspace.runtime.global_root(),
        &broken.id,
        body["assessment_artifact"].as_str().unwrap(),
    );
    let shown = workspace.show(&broken);
    assert_eq!(shown["pilot_notes"], json!([]), "{shown}");
    assert_eq!(
        shown["pilot_warnings"][0]["kind"], "pilot_assessment_unavailable",
        "{shown}"
    );
    let listed = workspace
        .runtime
        .run_tool("orbit.task.list", json!({"fields": ["id", "pilot_notes"]}))
        .unwrap();
    assert!(!listed.to_string().contains("lost"), "{listed}");
    assert_eq!(workspace.show(&sound)["pilot_notes"][0]["text"], "kept");
}

/// What admission, readiness and the drain's selection decide for one task
/// piloted with `notes`, in a workspace of its own so the two runs share no
/// selector.
fn decisions(notes: Option<Value>) -> Value {
    let workspace = workspace();
    let task = workspace.notes_task("inert");
    workspace.pilot_with_notes(&task, notes);
    let shown = workspace.show(&task);
    let proposed = json!({
        "status": shown["status"],
        "readiness": shown["readiness"],
        "pilot_warnings": shown["pilot_warnings"],
        "resolvable": shown["resolvable_pilot_admission_hold"],
        "complexity": shown["complexity"],
        "context_files": shown["context_files"],
        "tags": shown["tags"],
        "history": shown["history"].as_array().unwrap().iter()
            .map(|entry| {
                json!([
                    entry["event"],
                    entry["from_status"],
                    entry["to_status"],
                    entry["decision"]
                ])
            })
            .collect::<Vec<_>>(),
    });
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let attention = workspace
        .runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            json!({"kind": ["pilot_warning", "pilot_hold"]}),
            None,
            None,
            ToolEntryPoint::Mcp,
            ToolSessionContext {
                effective_capabilities: [McpCapability::Operator].into(),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("attention call failed: {error}"))
        .value;
    let attention = attention["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            item.pointer("/refs/task_id").and_then(Value::as_str) == Some(task.id.as_str())
        })
        .map(|item| item["kind"].clone())
        .collect::<Vec<_>>();
    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let selected = workspace.action("list_backlog_tasks", json!({}));

    // The assessment is fresh, and an edit to the instruction a note
    // answered, which changes only what a note would say, queues no pilot.
    let discover = || {
        workspace.action(
            "prepare_task_pilot",
            json!({"workspace_path": workspace.repo, "base_branch": "main"}),
        )["task_ids"]
            .clone()
    };
    let fresh = discover();
    workspace.commit_file(
        "docs/review-focus.md",
        "A different question.\n",
        "edit note",
    );
    let after_edit = discover();
    // A real edit to the task still makes the pilot stale, noted or not.
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                description: Some("Changed requirements.".into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let stale = discover();
    json!({
        "proposed": proposed,
        "readiness": readiness["tasks"],
        "attention": attention,
        "selected": selected["task_ids"],
        "fresh": fresh,
        "after_instruction_edit": after_edit,
        "after_task_edit": stale,
    })
}

fn a_note_decides_nothing_and_changes_no_pilot_freshness() {
    let with = decisions(Some(json!([note("review_focus", FOCUS)])));
    let without = decisions(None);
    assert_eq!(with, without);
    // The shared outcome is the ordinary one, so the comparison is not vacuous.
    assert_eq!(with["fresh"], json!([]), "{with}");
    assert_eq!(with["after_instruction_edit"], json!([]), "{with}");
    assert_eq!(
        with["after_task_edit"].as_array().unwrap().len(),
        1,
        "{with}"
    );
    assert_eq!(with["readiness"][0]["eligible"], true, "{with}");
}

fn no_prompt_carries_a_stored_note() {
    let workspace = workspace();
    let task = workspace.notes_task("prompted");
    workspace.pilot_with_notes(&task, Some(json!([note("review_focus", FOCUS)])));
    assert!(workspace.show(&task).to_string().contains(FOCUS));

    // The implement, review and recovery prompts embed this context.
    let context = Dispatch::task_context_for_agent_input(
        &orbit_core::test_support::EngineHost::new(&workspace.runtime),
        "jrun-notes-fixture",
        "implement",
        &json!({"task_id": task.id}),
    )
    .unwrap()
    .expect("a task context");
    assert!(!context.to_string().contains(FOCUS), "{context}");

    // A later pilot's input carries none either, held or not.
    let prepared = workspace.prepare(&[&task.id]);
    assert!(!prepared.to_string().contains(FOCUS), "{prepared}");
}

fn a_recheck_returns_its_own_notes_and_is_never_handed_the_earlier_ones() {
    let workspace = workspace();
    let task = workspace.notes_task("rechecked");
    let mut held = assessment(&task.id, Some(json!([note("review_focus", FOCUS)])));
    held["utility_warnings"] = json!([{
        "text": "The fixture's utility check must pass first.",
        "holds_promotion": true, "evidence": "README.md:1",
    }]);
    let prepared = workspace.prepare(&[&task.id]);
    workspace.apply_tasks(&prepared, vec![held]);
    assert_eq!(workspace.show(&task)["pilot_notes"][0]["text"], FOCUS);

    let prepared = workspace.prepare_with(&task.id, json!({}));
    let snapshot = &prepared["tasks"][0];
    assert_eq!(snapshot["pilot_mode"], "recheck", "{snapshot}");
    assert!(!prepared.to_string().contains(FOCUS), "{prepared}");
    assert!(
        snapshot["recheck"]["prior_assessment"]
            .get("notes")
            .is_none(),
        "{snapshot}"
    );

    // A re-check that returns none leaves none: the earlier notes are not
    // carried forward.
    let verdict = json!({
        "id": "utility_warnings[0]", "status": "still_holds",
        "evidence": "README.md:1 shows the check still holds.",
    });
    let applied = workspace.apply_tasks(
        &prepared,
        vec![json!({"task_id": task.id, "recheck_findings": [verdict.clone()]})],
    );
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let (_, audit) = workspace.latest_receipt(&task.id);
    assert_eq!(audit["assessment"]["mode"], "recheck", "{audit}");
    assert!(audit["assessment"].get("notes").is_none(), "{audit}");
    assert_eq!(workspace.show(&task)["pilot_notes"], json!([]));

    // A re-check that answers a label replaces them with its own.
    let answering = workspace.notes_task("rechecked with an answer");
    let mut held = assessment(&answering.id, Some(json!([note("review_focus", FOCUS)])));
    held["utility_warnings"] = json!([{
        "text": "The fixture's utility check must pass first.",
        "holds_promotion": true, "evidence": "README.md:1",
    }]);
    let prepared = workspace.prepare(&[&answering.id]);
    workspace.apply_tasks(&prepared, vec![held]);
    let prepared = workspace.prepare_with(&answering.id, json!({}));
    assert_eq!(prepared["tasks"][0]["pilot_mode"], "recheck", "{prepared}");
    let mut result = json!({"task_id": answering.id, "recheck_findings": [verdict]});
    result["notes"] = json!([note("risk", "Rechecked risk.")]);
    let applied = workspace.apply_tasks(&prepared, vec![result]);
    assert_eq!(applied["applied_count"], 1, "{applied}");
    let read = workspace.show(&answering)["pilot_notes"].clone();
    assert_eq!(read.as_array().unwrap().len(), 1, "{read}");
    assert_eq!(read[0]["label"], "risk", "{read}");
    assert_eq!(read[0]["text"], "Rechecked risk.", "{read}");
}

crate::scenario_table!(
    pilot_notes_scenarios,
    a_note_under_a_mapped_label_is_stored_and_read_without_a_comment,
    a_note_that_fails_a_check_is_dropped_and_the_assessment_applies,
    a_repeated_label_keeps_the_first_note_and_drops_the_repeat,
    the_fifth_note_of_an_entry_is_dropped_whatever_its_label,
    a_malformed_notes_field_never_fails_the_pilot,
    a_note_and_a_pilot_supplied_drop_record_cannot_be_forged,
    a_note_without_mapped_labels_is_dropped_for_a_workspace_that_maps_none,
    the_latest_assessment_is_the_truth_for_a_tasks_notes,
    an_unreadable_assessment_shows_no_notes_and_leaves_other_tasks_readable,
    a_note_decides_nothing_and_changes_no_pilot_freshness,
    no_prompt_carries_a_stored_note,
    a_recheck_returns_its_own_notes_and_is_never_handed_the_earlier_ones
);
