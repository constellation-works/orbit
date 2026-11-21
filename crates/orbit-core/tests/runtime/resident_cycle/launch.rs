//! Exercise the shipped cycle step through template rendering and the real
//! agent-launch integrity guard, with a local provider substitute.

use super::*;
use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{
    V2AuditWriter, execute_job_with_resume, load_activity_asset,
    resolve_job_catalog_refs_for_execution,
};
use orbit_types::workflow::PipelineState;

#[test]
fn cycle_pipeline_launches_in_its_declared_checkout_or_the_primary_fallback() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::launch::cycle_pipeline_launches_in_its_declared_checkout_or_the_primary_fallback",
    ) {
        return;
    }
    for managed_worktree in [true, false] {
        let fx = fixture();
        let (_, mandate) = fx.watched_mandate("Ship the next slice");
        let run = fx.cycle_run(&mandate, "resident-launch");
        if !managed_worktree {
            // Observe must preserve a directory it cannot prove it created,
            // producing the real no-worktree fallback rather than a fake brief.
            std::fs::create_dir_all(
                fx.repo
                    .join(format!(".orbit/state/worktrees/resident-{run}")),
            )
            .unwrap();
        }
        let brief = fx.begin_cycle(&run, Utc::now());
        let expected_cwd = if managed_worktree {
            PathBuf::from(brief["workspace_path"].as_str().unwrap())
        } else {
            assert!(brief["workspace_path"].is_null(), "{brief}");
            fx.repo.canonicalize().unwrap()
        };
        let invocation = fx._root.path().join("invocation.json");
        let provider = fx._root.path().join("codex");
        fx.install_codex(
            &provider,
            format!(
                r#"#!/usr/bin/env python3
import json, pathlib, sys
envelope = json.loads(sys.stdin.read().strip().splitlines()[-1])
pathlib.Path({invocation:?}).write_text(json.dumps({{"cwd": str(pathlib.Path.cwd()), "input": envelope["input"]}}))
print(json.dumps({{"schemaVersion": 1, "status": "success", "result": {{"outcome": "idle", "summary": "ok"}}, "error": None}}))
"#,
                invocation = invocation.to_str().unwrap(),
            ),
        );

        let mut job = load_job_asset(include_str!(
            "../../../assets/jobs/resident_cycle_pipeline.yaml"
        ))
        .unwrap()
        .spec;
        // Observe above has already taken the real lease and created (or
        // refused) the checkout. Resume from that boundary; finalize is
        // exercised explicitly below after inspecting the launched provider.
        job.steps.retain(|step| step.id != "finalize");
        let mut catalog = V2ActivityCatalog::new();
        for asset in [
            include_str!("../../../assets/activities/resident_observe.yaml"),
            include_str!("../../../assets/activities/resident_cycle.yaml"),
        ] {
            let mut activity = load_activity_asset(asset).unwrap();
            // The fixture substitutes the provider, not the integrity guard;
            // sandbox enforcement has its own platform boundary tests.
            activity.spec.fs_profile = None;
            catalog.insert(activity.name, activity.spec);
        }
        resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
        let input = json!({"task_ids": [mandate]});
        let mut resume =
            PipelineState::new(run.clone(), "resident_cycle_pipeline".into(), input.clone());
        resume.step_states.insert(0, JobRunState::Success);
        resume.step_outputs.insert(0, brief.clone());
        let audit = V2AuditWriter::with_disk_sinks(
            &fx._root.path().join("audit"),
            Arc::new(orbit_store::Store::open_in_memory().unwrap()),
            fx.runtime.workspace_id().unwrap(),
            &run,
            "codex:test",
            None,
        )
        .unwrap();
        let outcome = execute_job_with_resume(&job, input, &run, audit, &fx.runtime, Some(&resume));
        assert!(
            outcome.as_ref().is_ok_and(|outcome| outcome.success),
            "managed_worktree={managed_worktree}: {outcome:?}"
        );
        let launched: Value = serde_json::from_slice(&std::fs::read(invocation).unwrap()).unwrap();
        assert_eq!(launched["cwd"], expected_cwd.to_str().unwrap());
        if managed_worktree {
            assert_eq!(launched["input"]["repo_root"], launched["cwd"]);
            assert_eq!(launched["input"]["workspace_path"], launched["cwd"]);
        } else {
            assert!(launched["input"].get("repo_root").is_none(), "{launched}");
        }
        fx.checkpoint(&brief, &run);
        fx.finish_cycle(&run);
    }
}
