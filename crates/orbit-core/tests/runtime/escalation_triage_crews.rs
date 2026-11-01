//! Which crew `prepare_escalation_triage` draws each triager from. A pilot
//! warning reads `workflow.purpose_crews.pilot_warning_triage` while it is set
//! and holds a crew whose executor declares `inspection_args`, else
//! `workflow.hard_complexity_crews`, and passes over the crew that wrote the
//! held assessment while the pool offers another. Crew names are fixture policy; providers matter only in that
//! `gemini` declares no `inspection_args`.

use chrono::Utc;
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::RuntimeHost;
use orbit_types::workflow::{ActivityCrewDraw, PipelineState};
use serde_json::{Value, json};

use super::recovery_fixture::{Fixture, fixture_with_config};

const TASK_PILOT_POOL: &str = "workflow.purpose_crews.task_pilot";

const CREWS: &str = r#"
[crews.hard_lane]
provider = "codex"
model = "fixture-hard"

[crews.pilot_codex]
provider = "codex"
model = "fixture-pilot-codex"

[crews.pilot_claude]
provider = "claude"
model = "fixture-pilot-claude"

[crews.pilot_gemini]
provider = "gemini"
model = "fixture-pilot-gemini"

"#;

/// A fixture whose hard tier names `hard_lane`, with `purposes` as the
/// `[workflow.purpose_crews]` body.
fn triage_fixture(purposes: &str) -> Fixture {
    let purpose_table = if purposes.is_empty() {
        String::new()
    } else {
        format!("[workflow.purpose_crews]\n{purposes}\n")
    };
    fixture_with_config(&format!(
        "[workflow]\ndefault_crew = \"hard_lane\"\nhard_complexity_crews = [\"hard_lane\"]\n\
         {purpose_table}{CREWS}"
    ))
}

/// `prepare_escalation_triage` for `subject` over `task`.
fn prepare(fx: &Fixture, subject: &str, task: &str) -> Value {
    let prepared = fx
        .runtime
        .run_deterministic(
            "prepare_escalation_triage",
            &json!({}),
            &json!({
                "subject": subject, "task_ids": [task],
                "workspace_path": fx.repo, "base_branch": "main",
            }),
            orbit_tools::ToolContext::default(),
        )
        .unwrap();
    assert_eq!(prepared["task_ids"], json!([task]), "{prepared}");
    prepared
}

/// The prepared row, its partition's crew, and the partition's recorded draw
/// for `task`: `(crew, source, eligible members)`. The three agree.
fn drawn(prepared: &Value, task: &str) -> (String, String, Vec<String>) {
    let row = &prepared["tasks"][0];
    assert_eq!(row["task_id"], task, "{prepared}");
    let crew = row["crew"].as_str().unwrap().to_string();
    let partition = &prepared["partitions"][0];
    assert_eq!(partition["crew"], json!(crew), "{prepared}");
    let draw = &partition["draws"][0];
    assert_eq!(draw["task_id"], task, "{prepared}");
    assert_eq!(draw["source"], row["crew_source"], "{prepared}");
    assert_eq!(draw["eligible_pool"], row["eligible_pool"], "{prepared}");
    let members = row["eligible_pool"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["name"].as_str().unwrap().to_string())
        .collect();
    (
        crew,
        row["crew_source"].as_str().unwrap().to_string(),
        members,
    )
}

/// A `proposed` task whose latest task-pilot assessment holds promotion on a
/// warning, written by the pilot run `author` drew for (`None`: no run).
pub(super) fn held_pilot_warning(fx: &Fixture, author: Option<&str>) -> String {
    let created = fx
        .runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": "A task its pilot held on a warning",
                "description": "Escalation triage crew fixture task.",
                "acceptance_criteria": ["Triaged."],
                "complexity": "low",
                "type": "chore",
                "workspace": fx.repo.to_string_lossy(),
                "model": "codex",
            }),
        )
        .unwrap();
    let task = created["id"].as_str().unwrap().to_string();
    let mut audit = json!({"assessment": {
        "surface_warnings": [{"text": "The change reaches an unnamed surface.", "holds_promotion": true}],
    }});
    if let Some(author) = author {
        let jobs = orbit_store::compose::workspace_job_run_store(
            fx.runtime.sqlite_store().unwrap(),
            fx.runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
            .unwrap();
        let mut state =
            PipelineState::new(run.run_id.clone(), "task_pilot_pipeline".into(), json!({}));
        state.activity_crew_draws.insert(
            TASK_PILOT_POOL.to_string(),
            ActivityCrewDraw {
                crew: author.to_string(),
                source: TASK_PILOT_POOL.to_string(),
                eligible_pool: Vec::new(),
            },
        );
        fx.runtime.write_run_state(&run.run_id, &state).unwrap();
        audit["pilot_run_id"] = json!(run.run_id);
    }
    // A comment is a pilot receipt only when the task pilot wrote it.
    OrbitRuntime::from_roots(&fx.global, &fx.repo.join(".orbit"))
        .unwrap()
        .with_actor(ActorIdentity::human("task-pilot"))
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task,
                "comment": format!("operation_id={}\n{audit}", "b".repeat(64)),
                "workspace": fx.repo.to_string_lossy(),
            }),
        )
        .unwrap();
    task
}

#[test]
fn a_pilot_warning_draws_from_its_purpose_pool_and_records_it_as_the_source() {
    let fx = triage_fixture("pilot_warning_triage = [\"pilot_codex:1\", \"pilot_gemini:500\"]");
    let task = held_pilot_warning(&fx, None);
    let (crew, source, members) = drawn(&prepare(&fx, "pilot_warning", &task), &task);
    // `pilot_gemini` holds nearly every ticket but cannot inspect source, so
    // it is never drawn.
    assert_eq!(crew, "pilot_codex");
    assert_eq!(source, "purpose:pilot_warning_triage");
    assert_eq!(members, ["pilot_codex"]);
}

#[test]
fn an_unset_pilot_warning_pool_draws_from_the_hard_tier_as_before() {
    let fx = triage_fixture("");
    let task = held_pilot_warning(&fx, None);
    let prepared = prepare(&fx, "pilot_warning", &task);
    let (crew, source, members) = drawn(&prepared, &task);
    assert_eq!((crew.as_str(), source.as_str()), ("hard_lane", "pool:hard"));
    assert_eq!(members, ["hard_lane"]);
    assert_eq!(prepared["tasks"][0]["tier"], "hard", "{prepared}");
}

#[test]
fn a_pool_without_an_inspection_capable_member_falls_back_to_the_hard_tier() {
    let fx = triage_fixture("pilot_warning_triage = [\"pilot_gemini\"]");
    let task = held_pilot_warning(&fx, None);
    let (crew, source, _) = drawn(&prepare(&fx, "pilot_warning", &task), &task);
    assert_eq!((crew.as_str(), source.as_str()), ("hard_lane", "pool:hard"));
}

#[test]
fn the_retired_block_subject_is_refused_and_points_at_final_recovery() {
    let fx = triage_fixture("");
    let task = fx.task(&[]);
    fx.escalate(&task);
    let error = fx
        .runtime
        .run_deterministic(
            "prepare_escalation_triage",
            &json!({}),
            &json!({
                "subject": "block", "task_ids": [task],
                "workspace_path": fx.repo, "base_branch": "main",
            }),
            orbit_tools::ToolContext::default(),
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("retired") && error.contains("final recovery"),
        "{error}"
    );
    assert_eq!(fx.status(&task), "blocked", "nothing changed");
}

#[test]
fn partitions_stay_crew_homogeneous_with_at_most_five_tasks_each() {
    let fx = triage_fixture("pilot_warning_triage = [\"pilot_codex\"]");
    let tasks = (0..7)
        .map(|_| held_pilot_warning(&fx, None))
        .collect::<Vec<_>>();
    let prepared = fx
        .runtime
        .run_deterministic(
            "prepare_escalation_triage",
            &json!({}),
            &json!({"subject": "pilot_warning", "workspace_path": fx.repo, "base_branch": "main"}),
            orbit_tools::ToolContext::default(),
        )
        .unwrap();
    assert_eq!(prepared["task_count"], tasks.len(), "{prepared}");
    let sizes = prepared["partitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|partition| {
            assert_eq!(partition["crew"], "pilot_codex", "{prepared}");
            assert!(
                partition["draws"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|draw| draw["source"] == "purpose:pilot_warning_triage"),
                "{prepared}"
            );
            partition["task_ids"].as_array().unwrap().len()
        })
        .collect::<Vec<_>>();
    assert_eq!(sizes.iter().sum::<usize>(), tasks.len());
    assert!(sizes.iter().all(|size| *size <= 5), "{sizes:?}");
}

#[test]
fn a_pilot_warning_passes_over_the_assessments_author_while_the_pool_has_another() {
    // The author holds almost every ticket, so a draw that did not pass it
    // over would all but always pick it.
    let fx = triage_fixture("pilot_warning_triage = [\"pilot_codex:1\", \"pilot_claude:1000\"]");
    let task = held_pilot_warning(&fx, Some("pilot_claude"));
    let prepared = prepare(&fx, "pilot_warning", &task);
    let (crew, source, members) = drawn(&prepared, &task);
    assert_eq!(crew, "pilot_codex");
    assert_eq!(source, "purpose:pilot_warning_triage");
    assert_eq!(members, ["pilot_codex"]);
    assert_eq!(
        prepared["tasks"][0]["assessment_crew"], "pilot_claude",
        "{prepared}"
    );
}

#[test]
fn a_pilot_warning_draws_its_author_when_the_pool_offers_no_other_member() {
    let fx = triage_fixture("pilot_warning_triage = [\"pilot_claude\", \"pilot_gemini\"]");
    let task = held_pilot_warning(&fx, Some("pilot_claude"));
    let (crew, source, _) = drawn(&prepare(&fx, "pilot_warning", &task), &task);
    assert_eq!(crew, "pilot_claude");
    assert_eq!(source, "purpose:pilot_warning_triage");
}
