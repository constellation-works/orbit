//! Admission crew redraws after a final-recovery requeue preserved a publication.
use super::published_owner::{checkpoint, next, publication, requeue, save_run};
use super::*;
use orbit_core::application::task::TaskUpdateParams;
use orbit_store::contracts::{TaskDocumentUpdateParams, TaskHistoryUpdateParams};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::{TaskComplexity, TaskEnvelopeV2, TaskHistoryEntry};

const REFUSAL: &str = "published source is live, discarded, changed, or lacks an applied requeue";

/// Record the durable shape a usage-pressure admission redraw writes
/// [ORB-17770]: the crew moves under its unchanged automatic source, beside a
/// system `crew_redrawn` entry naming both crews.
fn redraw_crew(f: &Fixture, to: &str) {
    let task = f.runtime.get_task(&f.task).unwrap();
    let from = task.crew.clone().expect("an assigned crew");
    let source = task.crew_source.clone().expect("an automatic crew source");
    let stores = f.runtime.stores();
    stores
        .task_documents()
        .update_task_document(
            &f.task,
            TaskDocumentUpdateParams {
                actor: "system".into(),
                crew: Some(Some(to.into())),
                crew_source: Some(Some(source.clone())),
                ..Default::default()
            },
        )
        .unwrap();
    stores
        .task_history()
        .update_task_history(
            &f.task,
            TaskHistoryUpdateParams {
                actor: "system".into(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: "crew_redrawn".into(),
                    note: Some(format!(
                        "crew redrawn from {source} (`{from}`) to {source} (`{to}`) at admission \
                         under usage pressure: `{from}` ×8 (weekly usage), `{to}` ×1 (no live weekly usage)"
                    )),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

fn resume(f: &Fixture) -> (orbit_types::workflow::JobRun, Value, Result<Value, String>) {
    let (run, setup) = next(f);
    let resumed = execute_deterministic_action(
        &f.runtime,
        "candidate_resume",
        &json!({}),
        &json!({"job_run_id":run.run_id,"task_ids":[f.task],"workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}),
        false,
        &HashMap::new(),
        None,
    )
    .map_err(|error| error.to_string());
    (run, setup, resumed)
}

/// F2026-10-594: admission redrew the crew of a requeued published task twice,
/// and the next run could not adopt the reviewed, CI-green publication.
pub(super) fn system_crew_redraws_keep_the_preserved_publication_adoptable() {
    let mut f = Fixture::new();
    publication(&mut f);
    requeue(&mut f);
    let preserved = f.runtime.get_task(&f.task).unwrap();
    redraw_crew(&f, "sol");
    redraw_crew(&f, "sonnet");
    let redrawn = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(redrawn.crew.as_deref(), Some("sonnet"));
    assert_eq!(redrawn.crew_source, preserved.crew_source);

    let (run, setup, resumed) = resume(&f);
    let resumed = resumed.unwrap_or_else(|error| panic!("adoption after redraws: {error}"));
    assert_eq!(resumed["outcome"], "resumed_carried");
    assert_eq!(resumed["implement"], false);
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["prior_pull_request"], "17");

    // The adopted binding stays the run's publication authority downstream.
    checkpoint(&f, &run.run_id, 6, "resume_candidate", resumed);
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let commit = f.action("git_commit", json!({"job_run_id":run.run_id,"scope":"all","workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}));
    assert_eq!(commit["committed"], true);
    let pushed = f.action("git_push", json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,"base":"main","base_ref":setup["base_ref"]}));
    assert_eq!(pushed["remote_sha_before"], f.head);
}

/// A crew redraw never masks another identity change, liveness or discard:
/// each still refuses adoption with the existing refusal.
pub(super) fn crew_redraws_keep_every_other_adoption_refusal() {
    for case in [
        "title",
        "description",
        "acceptance_criteria",
        "required_tools",
        "complexity",
        "operator_crew",
        "redraw_after_operator_crew",
        "discarded",
        "live",
    ] {
        let mut f = Fixture::new();
        publication(&mut f);
        requeue(&mut f);
        redraw_crew(&f, "sol");
        let human = |params: TaskUpdateParams| {
            f.runtime
                .update_task_as_human(&f.task, params, "human".into())
                .unwrap_or_else(|error| panic!("{case}: {error}"));
        };
        match case {
            "title" => human(TaskUpdateParams {
                title: Some("Published repair, renamed".into()),
                ..Default::default()
            }),
            "description" => human(TaskUpdateParams {
                description: Some("Repair every compiler error".into()),
                ..Default::default()
            }),
            "acceptance_criteria" => human(TaskUpdateParams {
                acceptance_criteria: Some(vec!["Open a new PR".into()]),
                ..Default::default()
            }),
            // No update surface changes required tools; persisted state can.
            "required_tools" => {
                let registry =
                    TaskRegistryStore::open(&task_registry_path(&f.runtime.global_root())).unwrap();
                let path = registry
                    .find_task_binding(&f.task)
                    .unwrap()
                    .unwrap()
                    .canonical_path
                    .join("task.yaml");
                let mut envelope: TaskEnvelopeV2 =
                    serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                envelope.required_tools = vec!["orbit.task.add".into()];
                std::fs::write(&path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
                assert_eq!(
                    f.runtime.get_task(&f.task).unwrap().required_tools,
                    vec!["orbit.task.add".to_string()]
                );
            }
            "complexity" => {
                let current = f.runtime.get_task(&f.task).unwrap().complexity;
                human(TaskUpdateParams {
                    complexity: Some(if current == Some(TaskComplexity::Hard) {
                        TaskComplexity::Low
                    } else {
                        TaskComplexity::Hard
                    }),
                    ..Default::default()
                });
            }
            // The operator restores the preserved crew name, but as an
            // explicit choice rather than the automatic draw.
            "operator_crew" => human(TaskUpdateParams {
                crew: Some(Some("fixture".into())),
                ..Default::default()
            }),
            // A redraw recorded over a crew the operator chose undoes only
            // to that choice, never past it to the preserved crew.
            "redraw_after_operator_crew" => {
                human(TaskUpdateParams {
                    crew: Some(Some("fixture".into())),
                    ..Default::default()
                });
                redraw_crew(&f, "sonnet");
            }
            "discarded" => human(TaskUpdateParams {
                discard_candidate: true,
                ..Default::default()
            }),
            "live" => {
                f.source.pid = Some(std::process::id());
                save_run(&f, &f.source);
            }
            _ => unreachable!(),
        }
        let (_, setup, resumed) = resume(&f);
        let error = resumed.expect_err(case);
        assert!(error.contains(REFUSAL), "{case}: {error}");
        assert_eq!(
            git(
                &PathBuf::from(setup["workspace_path"].as_str().unwrap()),
                &["status", "--porcelain"]
            ),
            "",
            "{case}: refusal occurs before replacement implementation"
        );
    }
}
