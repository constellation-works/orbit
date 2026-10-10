//! Exercise the rendered recovery envelope at the provider subprocess boundary.

use std::os::unix::fs::PermissionsExt;

use orbit_types::workflow::activity_job::{ActivityV2Spec, Provider};

use super::*;

#[test]
fn the_recovery_prompt_lists_only_the_steps_admitted_for_this_failure() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let primary = root_path.join("primary");
    let assigned = root_path.join("assigned");
    std::fs::create_dir_all(&primary).unwrap();
    super::super::git_fixture::init(&primary);
    super::super::git_fixture::run(
        &primary,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@orbit.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    );
    super::super::git_fixture::run(
        &primary,
        &[
            "worktree",
            "add",
            "--detach",
            assigned.to_str().unwrap(),
            "HEAD",
        ],
    );
    let cli = root_path.join("claude");
    // Claude receives the execution envelope on stdin. Capture
    // the actual provider input, then return a valid final-recovery response.
    std::fs::write(
        &cli,
        "#!/bin/sh\ncat > envelope.json\n\
         printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"decision\":\"resume\",\"step_id\":\"work\",\"rationale\":\"rerun the contract\"},\"error\":null}'\n",
    ).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut host = RecoveryHost::new([(
        "deliver",
        vec![Reply::Fail, Reply::Ok(json!({"delivered": true}))],
    )]);
    host.workspace_path = assigned.to_string_lossy().into_owned();
    host.primary_root = Some(primary);
    host.cli = Some(orbit_engine::ResolvedCliExecutor {
        command: cli.to_string_lossy().into_owned(),
        args: Vec::new(),
    });
    let mut job = pipeline(false);
    // Load the canonical activity rather than reproducing its instruction.
    let activity_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../orbit-core/assets/activities/final_recovery.yaml");
    let mut activity =
        load_activity_asset(&std::fs::read_to_string(activity_path).unwrap()).unwrap();
    let ActivityV2Spec::AgentLoop(spec) = &mut activity.spec.spec else {
        panic!("final recovery must dispatch an agent");
    };
    spec.provider = Provider::Claude;
    spec.tools = Vec::new();
    spec.tool_disallow_list = None;
    spec.wall_clock_timeout_seconds = 30;
    job.final_recovery_activity = Some("final_recovery".to_string());
    job.resolved_final_recovery_activity = Some(activity.spec);

    let run = run(&job, &host, None);
    assert!(
        run.result.as_ref().is_ok_and(|outcome| outcome.success),
        "result: {:?}, recovery events: {:?}, applications: {:?}",
        run.result,
        run.details,
        host.applications()
    );
    let prompt = std::fs::read_to_string(assigned.join("envelope.json")).unwrap();
    let envelope: Value = serde_json::from_str(prompt.lines().last().unwrap()).unwrap();
    let allowed = json!(["work", "deliver"]);
    assert_eq!(envelope["input"]["allowed_resume_step_ids"], allowed);
    // The dynamic instruction ends in a JSON list. Parse that payload so
    // this checks the allowed targets, without pinning prompt prose.
    let instruction = envelope["instruction"].as_str().unwrap();
    let (_, rendered_ids) = instruction.lines().last().unwrap().split_once(':').unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(rendered_ids.trim()).unwrap(),
        allowed
    );
    assert_eq!(
        host.actions(),
        ["setup", "work", "deliver", "work", "deliver"]
    );
}
