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

        launch_cycle_step(&fx, &run, &brief);
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

fn launch_cycle_step(fx: &Fixture, run: &str, brief: &Value) {
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
    let input = json!({"task_ids": [brief["task_id"]]});
    let mut resume = PipelineState::new(
        run.to_string(),
        "resident_cycle_pipeline".into(),
        input.clone(),
    );
    resume.step_states.insert(0, JobRunState::Success);
    resume.step_outputs.insert(0, brief.clone());
    let audit = V2AuditWriter::with_disk_sinks(
        &fx._root.path().join("audit"),
        orbit_store::Store::open_in_memory().unwrap(),
        fx.runtime.workspace_id().unwrap(),
        run,
        "codex:test",
        None,
    )
    .unwrap();
    let outcome = execute_job_with_resume(&job, input, run, audit, &fx.runtime, Some(&resume));
    assert!(
        outcome.as_ref().is_ok_and(|outcome| outcome.success),
        "{outcome:?}"
    );
}

#[test]
fn codex_resident_discovers_granted_delivery_tools_and_reads_status_through_its_broker() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::launch::codex_resident_discovers_granted_delivery_tools_and_reads_status_through_its_broker",
    ) {
        return;
    }
    for (case, rights, observe_only, expired) in [
        (
            "granted",
            &["dispatch", "merge", "refresh", "recover"][..],
            false,
            false,
        ),
        ("missing", &["promote"][..], false, false),
        (
            "observe-only",
            &["dispatch", "merge", "refresh", "recover"][..],
            true,
            false,
        ),
        (
            "expired",
            &["dispatch", "merge", "refresh", "recover"][..],
            false,
            true,
        ),
    ] {
        let fx = fixture();
        let (watched, mandate) = fx.watched_mandate("Ship the next slice");
        let workspace = fx.runtime.workspace_id().unwrap();
        fx.grant(&mandate, rights);
        if observe_only {
            fx.operator_call(
                "orbit.notice.post",
                json!({"kind": "maintenance", "title": "Maintenance", "expires_in_hours": 2}),
            );
        }
        let run = fx.cycle_run(&mandate, case);
        let brief = fx.begin_cycle(&run, Utc::now());
        if expired {
            fx.expire_cycle(&mandate, &run, &brief);
        }
        assert_eq!(
            brief["authority"]["observe_only"], observe_only,
            "{case}: {brief}"
        );
        let broker = delivery_broker(&fx, &run, &brief);
        let invocation = fx._root.path().join("delivery-tools.json");
        let calls = json!({
            "status": ["orbit.workflow.auto", {"action": "status", "workspace": workspace}],
            "control": ["orbit.workflow.auto", {"action": "start", "workspace": workspace, "for_seconds": 60}],
            "foreign": ["orbit.workflow.land", {"task_id": watched, "workspace": "ws_not_granted"}],
            "land": ["orbit.workflow.land", {"task_id": watched, "workspace": workspace}],
            "recover": ["orbit.task.resume_delivery", {"id": watched, "run_id": "jrun-unobserved",
                "diagnosis": "Recover the stopped delivery", "workspace": workspace}],
        });
        fx.install_codex(
            &fx._root.path().join("codex"),
            format!(
                r#"#!/usr/bin/env python3
import json, os, pathlib, socket, struct, sys

def call(tool, input):
    request = json.dumps({{"schema_version": 1, "tool": tool, "input": input, "cwd": os.getcwd()}}).encode()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(10)
        client.connect({socket_path:?})
        client.sendall(struct.pack(">I", len(request)) + request)
        reply = client.makefile("rb")
        (length,) = struct.unpack(">I", reply.read(4))
        return json.loads(reply.read(length))

envelope = json.loads(sys.stdin.read().strip().splitlines()[-1])
calls = json.loads({calls:?})
pathlib.Path({invocation:?}).write_text(json.dumps({{
    "tools": envelope["tools"],
    "toolsets": os.environ["ORBIT_ACTIVITY_MCP_TOOLSETS"].split(","),
    "replies": {{name: call(*args) for name, args in calls.items()}},
}}))
print(json.dumps({{"schemaVersion": 1, "status": "success", "result": {{"outcome": "idle"}}, "error": None}}))
"#,
                socket_path = orbit_engine::PluginBrokerHandle::socket_path(&broker).to_str().unwrap(),
                calls = calls.to_string(),
                invocation = invocation.to_str().unwrap(),
            ),
        );
        launch_cycle_step(&fx, &run, &brief);
        let launched: Value = serde_json::from_slice(&std::fs::read(invocation).unwrap()).unwrap();
        let granted = case == "granted";
        for family in ["orbit.workflow", "orbit.task.recover"] {
            assert_eq!(
                launched["tools"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(family)),
                granted,
                "{case}: Codex envelope for {family}: {launched}"
            );
            assert_eq!(
                launched["toolsets"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("tool:{family}"))),
                granted,
                "{case}: nested MCP grant for {family}"
            );
        }
        let replies = &launched["replies"];
        if expired {
            assert_eq!(replies["status"]["ok"], false, "{replies}");
        } else {
            assert_eq!(replies["status"]["ok"], true, "{replies}");
            assert_eq!(replies["status"]["output"]["scope"], "drain");
            assert_eq!(replies["status"]["output"]["workspace"], workspace);
            assert_eq!(replies["status"]["output"]["controls_authorized"], false);
        }
        assert_eq!(replies["control"]["ok"], false, "{replies}");
        assert!(fx.runtime.workspace_drain_intent().unwrap().is_none());
        for action in ["land", "recover"] {
            assert_eq!(replies[action]["ok"], false, "{case}: {replies}");
            if !granted {
                let expected = if expired {
                    "resident_cycle_stale"
                } else {
                    "resident_right_refused"
                };
                assert!(
                    replies[action]["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(expected),
                    "{case}: {replies}"
                );
            }
        }
        if granted {
            assert!(
                replies["foreign"]["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("resident_scope_refused"),
                "{replies}"
            );
            for (action, reason) in [
                ("land", "only a task in review lands"),
                ("recover", "resident recovery requires a blocked task"),
            ] {
                assert!(
                    replies[action]["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(reason),
                    "{replies}"
                );
            }
        }
        fx.runtime.finish_resident_cycle(&run).unwrap();
    }
}

fn delivery_broker(
    fx: &Fixture,
    run: &str,
    brief: &Value,
) -> orbit_core::runtime::plugin::broker::PluginBroker {
    use orbit_engine::PluginBrokerRun;
    use orbit_tools::plugin::BrokeredCaller;
    use orbit_types::policy::ResolvedFsProfile;
    use orbit_types::workflow::{ActivityToolDenyPolicy, ActivityV2Spec};

    let activity = load_activity_asset(include_str!(
        "../../../assets/activities/resident_cycle.yaml"
    ))
    .unwrap();
    let ActivityV2Spec::AgentLoop(spec) = activity.spec.spec else {
        panic!("the resident cycle is an agent loop");
    };
    // Use the production broker and dispatch, with the isolated test process
    // as its peer anchor so the fixture can also run inside a host sandbox.
    orbit_core::test_support::start_test_run_broker(
        &fx.runtime,
        PluginBrokerRun {
            run_id: run.into(),
            job_run_id: Some(run.into()),
            task_id: Some(brief["task_id"].as_str().unwrap().into()),
            activity_name: activity.name.clone(),
            agent_name: Some("codex".into()),
            model_name: Some("test".into()),
            workspace: Some(fx.runtime.workspace_id().unwrap()),
            allowed_tools: vec![],
            tool_deny_policy: Some(ActivityToolDenyPolicy {
                activity: activity.name,
                disallow_list: spec.tool_disallow_list.unwrap(),
            }),
            caller: BrokeredCaller {
                worktree: PathBuf::from(brief["workspace_path"].as_str().unwrap()),
                fs_profile: ResolvedFsProfile {
                    name: "reviewer".into(),
                    read: vec![],
                    modify: vec![],
                },
                proc_allowed_programs: vec![],
                proc_disallowed_programs: None,
            },
        },
    )
    .unwrap()
}
