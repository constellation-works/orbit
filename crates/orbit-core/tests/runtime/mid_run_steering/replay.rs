//! Replay the missed operator correction from ORB-16577 through both shipped
//! pipeline heads, their real steering hooks and the real commit action.
//! Provider work stays deterministic: the candidate records whether its prompt
//! carried the correction. Dispatch is backdated seven minutes, with no sleep.

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use orbit_core::TaskStatus;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::{RuntimeHost, SteeringGateReading, SteeringRoundAdmission};
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use super::gate::{Pipeline, admitted, pipeline_head, rounds};
use super::{ENABLED, Fixture};

const PIPELINES: [&str; 2] = ["task_local_pipeline", "task_pr_pipeline"];
const CORRECTION: &str =
    "STEER: document the confirmed model ids and all three crew tiers; classify provider failures.";
const INITIAL: &str = "candidate from dispatch snapshot\n";
const CORRECTED: &str = "confirmed models; three crew tiers; provider failure classification\n";

fn git(repo: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Pin a real disposable candidate and associate the task with its run, as
/// pipeline admission does. No Git or Orbit state outside the fixture changes.
fn candidate(fx: &Fixture, run: &str) -> (std::path::PathBuf, String) {
    let repo = fx.workspace.parent().unwrap();
    git(repo, &["init", "-b", "main"]);
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    std::fs::write(repo.join("candidate.txt"), "base\n").unwrap();
    git(repo, &["add", ".gitignore", "candidate.txt"]);
    git(repo, &["commit", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("candidate.txt"), INITIAL).unwrap();
    fx.runtime
        .update_task_with_identity(
            &fx.task,
            TaskUpdateParams {
                job_run_id: Some(Some(run.into())),
                context_files: Some(vec!["file:candidate.txt".into()]),
                execution_summary: Some(
                    "Outcome: success\nChanges: produced the replay candidate.".into(),
                ),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    (repo.into(), base)
}

fn carries_correction(prompt: &Value) -> bool {
    prompt["steering"]["comments"]
        .as_str()
        .is_some_and(|comments| comments.contains(CORRECTION))
}

/// Re-enter at the commit checkpoint of a run whose implement step finished.
/// This is important for enablement: rebuilding its initial prompt would
/// create a watermark and fail to exercise the no-watermark migration path.
fn before_commit(fx: &Fixture, run: &str, job: &JobV2, base: &str) -> PipelineState {
    let mut resume = fx.state(run);
    for (index, step) in job
        .steps
        .iter()
        .take_while(|s| s.id != "commit")
        .enumerate()
    {
        let output = match step.id.as_str() {
            "worktree" => json!({
                "job_run_id": run, "workspace_path": fx.workspace.parent().unwrap(),
                "base_sha": base, "base_ref": "main",
            }),
            "resume_candidate" => json!({"implement": true, "repair": null}),
            _ => json!({}),
        };
        resume.record_step(
            index as u32,
            JobRunState::Success,
            Some(output.clone()),
            None,
        );
        resume.record_pipeline_output(&step.id, output);
    }
    resume
}

fn the_seven_minute_correction_reaches_a_safe_point_or_a_round_before_real_commit() {
    for pipeline in PIPELINES {
        for safe_point in [true, false] {
            let fx = admitted(ENABLED);
            let dispatched = Utc::now() - Duration::minutes(7);
            let run = fx.run_at(None, dispatched);
            let (repo, base) = candidate(&fx, &run);
            let safe_prompts = Arc::new(Mutex::new(Vec::new()));
            let recorded = safe_prompts.clone();
            let checkpoint_run = run.clone();
            let mut host = Pipeline::new(&fx, &run);
            host.candidate = Some((repo.clone(), base.clone()));
            host.on_implement = Box::new(move |fx, built| {
                if built == 0 {
                    let at = fx.operator_comment(CORRECTION);
                    assert!(at - dispatched >= Duration::minutes(7));
                    if safe_point {
                        let prompt = fx.prompt(&checkpoint_run, "validate_repair");
                        assert!(carries_correction(&prompt));
                        recorded.lock().unwrap().push(prompt);
                        std::fs::write(
                            fx.workspace.parent().unwrap().join("candidate.txt"),
                            CORRECTED,
                        )
                        .unwrap();
                    }
                } else {
                    // The gate re-dispatched the implementer; its prompt is
                    // checked below independently of this scripted edit.
                    std::fs::write(
                        fx.workspace.parent().unwrap().join("candidate.txt"),
                        CORRECTED,
                    )
                    .unwrap();
                }
            });
            let outcome = host.execute(&pipeline_head(pipeline), None);
            assert!(
                outcome.success,
                "{pipeline}, safe={safe_point}: {outcome:?}"
            );
            assert_eq!(outcome.pipeline["commit"]["committed"], true);
            let head = git(&repo, &["rev-parse", "HEAD"]);
            assert_ne!(head, base);
            assert_eq!(outcome.pipeline["commit"]["commit_sha"], head);
            assert_eq!(
                git(&repo, &["show", "HEAD:candidate.txt"]),
                CORRECTED.trim()
            );
            assert!(git(&repo, &["status", "--porcelain"]).is_empty());
            assert_eq!(rounds(&fx, &run), usize::from(!safe_point));
            let prompts = host.prompts.lock().unwrap();
            assert_eq!(prompts.len(), if safe_point { 1 } else { 2 });
            assert!(
                !carries_correction(&prompts[0]),
                "the dispatch snapshot predates the comment"
            );
            if !safe_point {
                assert!(carries_correction(&prompts[1]));
            }
            assert_eq!(safe_prompts.lock().unwrap().len(), usize::from(safe_point));
            let deliveries = fx.deliveries();
            assert_eq!(deliveries.len(), 1);
            assert_eq!(
                deliveries[0]["step_id"],
                if safe_point {
                    "validate_repair"
                } else {
                    "implement_one"
                }
            );
            assert_eq!(
                RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task, None).unwrap(),
                SteeringGateReading::Clear
            );
        }
    }
}

fn enabling_an_in_flight_run_without_a_watermark_delivers_only_post_dispatch_comments() {
    for pipeline in PIPELINES {
        for safe_point in [false, true] {
            let fx = admitted("");
            fx.operator_comment("BEFORE-DISPATCH: already part of the brief.");
            let run = fx.run(None);
            let (repo, base) = candidate(&fx, &run);
            fx.prompt(&run, "implement_one");
            fx.operator_comment(CORRECTION);
            // The ordinary task projection omits the correction. A newly
            // enabled safe point must not count that snapshot as delivery.
            for _ in 0..21 {
                fx.comment("AGENT: progress bookkeeping.");
            }
            assert!(fx.state(&run).steering_watermarks.is_empty());
            let job = pipeline_head(pipeline);
            let resume = before_commit(&fx, &run, &job, &base);
            let config = fx.workspace.join("config.toml");
            std::fs::write(&config, ENABLED).unwrap();
            let SteeringGateReading::Outstanding(pending) =
                RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task, None).unwrap()
            else {
                panic!("{pipeline}: enabling must not snapshot away unseen steering");
            };
            assert_eq!(pending.len(), 1);
            let comments = fx.runtime.get_task_comments(&fx.task).unwrap();
            assert_eq!(comments[pending[0].position as usize].message, CORRECTION);
            // A malformed edit keeps the successfully enabled policy, rather
            // than falling back to the runtime's original disabled snapshot.
            std::fs::write(&config, "[workflow\n").unwrap();
            assert_eq!(
                RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task, None).unwrap(),
                SteeringGateReading::Outstanding(pending.clone())
            );
            std::fs::write(&config, ENABLED).unwrap();
            let delivered_prompt = safe_point.then(|| {
                let prompt = fx.prompt(&run, "validate_repair");
                assert!(
                    carries_correction(&prompt),
                    "{pipeline}: enabling must deliver at the next safe point"
                );
                std::fs::write(repo.join("candidate.txt"), CORRECTED).unwrap();
                prompt
            });
            let mut host = Pipeline::new(&fx, &run);
            host.candidate = Some((repo.clone(), base));
            host.on_implement = Box::new(|fx, _| {
                std::fs::write(
                    fx.workspace.parent().unwrap().join("candidate.txt"),
                    CORRECTED,
                )
                .unwrap();
            });
            let outcome = host.execute(&job, Some(&resume));
            assert!(
                outcome.success,
                "{pipeline}, safe={safe_point}: {outcome:?}"
            );
            let prompts = host.prompts.lock().unwrap();
            let prompt = if let Some(prompt) = &delivered_prompt {
                assert_eq!(host.labels(), ["commit"]);
                assert!(prompts.is_empty());
                prompt
            } else {
                assert_eq!(host.labels(), ["implement_one", "commit"]);
                assert_eq!(prompts.len(), 1);
                &prompts[0]
            };
            assert!(carries_correction(prompt));
            assert!(
                !prompt["steering"]["comments"]
                    .as_str()
                    .unwrap()
                    .contains("BEFORE-DISPATCH")
            );
            assert_eq!(rounds(&fx, &run), usize::from(!safe_point));
            let deliveries = fx.deliveries();
            assert_eq!(deliveries.len(), 1);
            assert_eq!(
                deliveries[0]["delivered"][0]["position"],
                pending[0].position
            );
            assert_eq!(
                git(&repo, &["show", "HEAD:candidate.txt"]),
                CORRECTED.trim()
            );
        }
    }
}

fn disabling_mid_flight_leaves_watermarks_and_rounds_inert_at_prompt_and_commit() {
    for pipeline in PIPELINES {
        let fx = admitted(ENABLED);
        let run = fx.run(None);
        let (repo, base) = candidate(&fx, &run);
        fx.prompt(&run, "implement_one");
        fx.operator_comment(CORRECTION);
        let SteeringGateReading::Outstanding(pending) =
            RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task, None).unwrap()
        else {
            panic!("{pipeline}: pending steering before disable");
        };
        assert!(matches!(
            RuntimeHost::admit_steering_round(&fx.runtime, &run, &fx.task, &pending, None).unwrap(),
            SteeringRoundAdmission::Admitted { round: 1, .. }
        ));
        assert!(carries_correction(&fx.prompt(&run, "implement_one")));
        fx.operator_comment("STEER-AFTER: this stays unseen when disabled.");
        let job = pipeline_head(pipeline);
        let resume = before_commit(&fx, &run, &job, &base);
        let state = serde_json::to_value(fx.state(&run)).unwrap();
        let history = serde_json::to_value(fx.runtime.get_task_history(&fx.task).unwrap()).unwrap();
        let task = serde_json::to_value(fx.runtime.get_task(&fx.task).unwrap()).unwrap();
        std::fs::write(fx.workspace.join("config.toml"), "").unwrap();
        assert!(fx.prompt(&run, "validate_repair").get("steering").is_none());
        assert_eq!(
            RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task, None).unwrap(),
            SteeringGateReading::Disabled
        );
        // An invalid edit must not resurrect the originally enabled policy.
        std::fs::write(fx.workspace.join("config.toml"), "[workflow\n").unwrap();
        assert!(fx.prompt(&run, "final_recovery").get("steering").is_none());
        assert_eq!(
            RuntimeHost::steering_gate(&fx.runtime, &run, &fx.task, None).unwrap(),
            SteeringGateReading::Disabled
        );
        std::fs::write(fx.workspace.join("config.toml"), "").unwrap();
        assert_eq!(serde_json::to_value(fx.state(&run)).unwrap(), state);
        assert_eq!(
            serde_json::to_value(fx.runtime.get_task_history(&fx.task).unwrap()).unwrap(),
            history
        );
        assert_eq!(
            serde_json::to_value(fx.runtime.get_task(&fx.task).unwrap()).unwrap(),
            task
        );
        let mut host = Pipeline::new(&fx, &run);
        host.candidate = Some((repo.clone(), base));
        let outcome = host.execute(&job, Some(&resume));
        assert!(outcome.success, "{pipeline}: {outcome:?}");
        assert_eq!(host.labels(), ["commit"]);
        assert_eq!(git(&repo, &["show", "HEAD:candidate.txt"]), INITIAL.trim());
        assert_eq!(
            serde_json::to_value(fx.state(&run)).unwrap(),
            state,
            "leftover state is neither consulted nor deleted"
        );
        assert_eq!(rounds(&fx, &run), 1);
        assert_eq!(fx.deliveries().len(), 1);
        assert_eq!(
            fx.runtime.get_task(&fx.task).unwrap().status,
            TaskStatus::InProgress
        );
    }
}

crate::scenario_table!(
    mid_run_steering_replay_scenarios,
    the_seven_minute_correction_reaches_a_safe_point_or_a_round_before_real_commit
);

crate::scenario_table!(
    mid_run_steering_enable_migration_scenarios,
    enabling_an_in_flight_run_without_a_watermark_delivers_only_post_dispatch_comments
);

crate::scenario_table!(
    mid_run_steering_disable_migration_scenarios,
    disabling_mid_flight_leaves_watermarks_and_rounds_inert_at_prompt_and_commit
);
