use orbit_engine::Dispatch;
use orbit_tools::ToolContext;
use orbit_types::task::TaskType;

use super::*;

// Apply through the same deterministic action as the pilot job so the
// resident reads a sealed, artifact-backed receipt.
fn assess(fx: &Fixture, task: &str, warnings: Value) -> String {
    assess_with(fx, &fx.runtime, task, json!({"utility_warnings": warnings}))
}

fn assess_with(fx: &Fixture, runtime: &OrbitRuntime, task: &str, overrides: Value) -> String {
    let run_id = fx
        .jobs
        .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
        .unwrap()
        .run_id;
    let action = |name, input| {
        orbit_core::test_support::EngineHost::new(runtime)
            .run_deterministic(name, &json!({}), &input, ToolContext::default())
            .unwrap()
    };
    let prepared = action(
        "prepare_task_pilot",
        json!({"task_ids": [task], "workspace_path": fx.repo, "base_branch": "main"}),
    );
    assert_eq!(prepared["task_ids"], json!([task]), "{prepared}");
    let revision = prepared["source"]["source_revision"].as_str().unwrap();
    let tree =
        crate::git_test_support::run(&fx.repo, &["rev-parse", &format!("{revision}^{{tree}}")])
            .unwrap();
    assert!(tree.success, "{}", tree.stderr);
    let mut task_result = json!({
        "task_id": task,
        "context_files_before": prepared["tasks"][0]["context_files_before"],
        "context_files_after": ["file:.gitignore"],
        "disposition": "selectors",
        "recommended_crew": "sol",
        "recommended_complexity": "low",
        "confidence": "high",
        "assessment_rationale": "The ignore rule determines which scratch files can be committed.",
        "validation_approach": "Check the matched ignore paths.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    for (key, value) in overrides.as_object().unwrap() {
        task_result[key] = value.clone();
    }
    let applied = action(
        "apply_task_pilot_results",
        json!({
            "run_id": run_id,
            "workspace_path": fx.repo,
            "prepared": prepared,
            "results": [{
                "partition_index": prepared["partitions"][0]["partition_index"],
                "task_ids": [task],
                "observed_revision": revision,
                "observed_tree": tree.stdout.trim(),
                "tasks": [task_result],
            }],
        }),
    );
    assert_eq!(applied["applied_count"], 1, "{applied}");
    fx.stop_run(&run_id, JobRunState::Success);
    fx.runtime
        .get_task_comments(task)
        .unwrap()
        .last()
        .unwrap()
        .operation_id
        .clone()
        .unwrap()
}

#[test]
fn resident_backlog_update_honors_the_proposals_approval_opt_out() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::promote_held::resident_backlog_update_honors_the_proposals_approval_opt_out",
    ) {
        return;
    }
    let fx = fixture();
    let proposal = |title: &str, tags: &[&str]| {
        fx.add_task(TaskAddParams {
            title: title.into(),
            description: "Adjust the ignore rule for workspace scratch output.".into(),
            acceptance_criteria: vec!["Scratch output is ignored.".into()],
            task_type: Some(TaskType::Chore),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
    };
    let clean = proposal("Clean pilot", &["no-auto-approve"]);
    let warned = proposal("Latest pilot has a warning", &["no-auto-approve"]);
    let stale = proposal("Stale pilot", &["no-auto-approve"]);
    let missing = proposal("Missing pilot", &["no-auto-approve"]);
    let design = proposal("Design-tagged proposal", &["no-auto-approve", "design"]);
    let breaking = proposal("Breaking proposal", &["no-auto-approve", "breaking-change"]);
    let no_drain = proposal("No-drain proposal", &["no-auto-approve", "no-drain"]);
    let no_drain_only = proposal("No-drain-only proposal", &["no-drain"]);
    let ordinary = proposal("Ordinary proposal", &[]);
    let promote =
        |id: &str| fx.agent_call("orbit.task.update", json!({"id": id, "status": "backlog"}));

    let assessment = assess(&fx, &clean, json!([]));
    let ordinary_mandate = fx.mandate(&clean);
    fx.grant(&ordinary_mandate, &["promote", "dispatch"]);
    let ordinary_run = fx.cycle_run(&ordinary_mandate, "ordinary-promotion");
    fx.begin_cycle(&ordinary_run, Utc::now());
    fx.as_cycle_agent(&ordinary_run, || {
        for held_id in [&clean, &no_drain_only] {
            let error = promote(held_id).unwrap_err();
            assert!(
                error.contains("needs the `promote_held` right"),
                "{held_id}: {error}"
            );
            let error = fx
                .agent_call(
                    "orbit.task.update",
                    json!({"id": held_id, "status": "in-progress"}),
                )
                .unwrap_err();
            assert!(
                error.contains("resident_held_promotion_refused"),
                "{held_id}: {error}"
            );
            let error = fx
                .agent_call("orbit.task.update", json!({"id": held_id, "tags": []}))
                .unwrap_err();
            assert!(
                error.contains("resident_held_promotion_refused"),
                "{held_id}: {error}"
            );
            assert_eq!(
                fx.runtime.get_task(held_id).unwrap().status,
                TaskStatus::Proposed
            );
        }
        assert_eq!(
            fx.runtime.get_task(&no_drain_only).unwrap().tags,
            vec!["no-drain"]
        );
        assert_eq!(promote(&ordinary).unwrap()["status"], "backlog");
    });

    assess(&fx, &warned, json!([]));
    assess(
        &fx,
        &warned,
        json!([{
            "text": "Adjacent cleanup is optional.",
            "holds_promotion": false,
        }]),
    );
    assess(&fx, &stale, json!([]));
    fx.update_task(
        &stale,
        TaskUpdateParams {
            description: Some("Implement a different change after the pilot.".into()),
            ..Default::default()
        },
    );
    let mandate = fx.mandate(&clean);
    fx.grant(&mandate, &["promote_held"]);
    let run = fx.cycle_run(&mandate, "held-promotion");
    let brief = fx.begin_cycle(&run, Utc::now());
    fx.as_cycle_agent(&run, || {
        for (id, reason) in [
            (&warned, "warnings"),
            (&stale, "changed_since_pilot"),
            (&missing, "current, clean task pilot"),
            (&design, "design"),
            (&breaking, "breaking-change"),
            (&no_drain, "no-drain"),
            (&no_drain_only, "no-drain"),
        ] {
            let error = promote(id).unwrap_err();
            assert!(
                error.contains("resident_held_promotion_refused") && error.contains(reason),
                "{id}: {error}"
            );
            assert_eq!(
                fx.runtime.get_task(id).unwrap().status,
                TaskStatus::Proposed
            );
        }
        let error = promote(&proposal("Another ordinary proposal", &[])).unwrap_err();
        assert!(error.contains("needs the `promote` right"), "{error}");
        let result = promote(&clean).unwrap();
        assert_eq!(result["status"], "backlog", "{result}");
        let comments = fx.runtime.get_task_comments(&clean).unwrap();
        let audit = &comments.last().unwrap().message;
        assert!(audit.contains(&assessment), "{audit}");
        assert!(audit.contains("promote_held"), "{audit}");
        let cycle = brief["cycle"].as_str().unwrap();
        assert!(
            audit.contains(&format!("resident:{mandate}:{cycle}:")),
            "{audit}"
        );
    });
}

#[test]
fn resident_promote_held_evaluates_active_pilot_admission_holds() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::promote_held::resident_promote_held_evaluates_active_pilot_admission_holds",
    ) {
        return;
    }
    let fx = fixture();
    let proposal = |title: &str, task_type: TaskType, tags: &[&str], criterion: &str| {
        fx.add_task(TaskAddParams {
            title: title.into(),
            description: "Adjust host-specific validation handling.".into(),
            acceptance_criteria: vec![criterion.into()],
            task_type: Some(task_type),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
    };

    let os_routed = proposal(
        "Linux-only bug fix",
        TaskType::Bug,
        &["no-auto-approve"],
        "Linux sandbox validation succeeds.",
    );
    let untagged_os = proposal(
        "Conflicting OS tag leaves required_os untagged",
        TaskType::Bug,
        &["no-auto-approve", "os:macos"],
        "Linux sandbox validation succeeds.",
    );
    let machine_bound = proposal(
        "Owner-machine validation required",
        TaskType::Chore,
        &["no-auto-approve"],
        "Owner machine live store check passes.",
    );
    let host_operational = proposal(
        "Host-operational maintenance",
        TaskType::Chore,
        &["no-auto-approve"],
        "Host daemon is restarted.",
    );
    let operator_validation = proposal(
        "Operator-only tool validation",
        TaskType::Chore,
        &["no-auto-approve"],
        "Run orbit.pipeline.invoke to verify pipeline startup.",
    );

    let os_routed_assessment = assess_with(
        &fx,
        &fx.runtime,
        &os_routed,
        json!({
            "required_os": [{
                "criterion": 1,
                "os": "linux",
                "evidence": "Criterion 1 requires a native Linux sandbox.",
            }],
        }),
    );
    let routed_task = fx.runtime.get_task(&os_routed).unwrap();
    assert!(
        routed_task.tags.iter().any(|tag| tag == "os:linux"),
        "pilot apply should add os:linux: {:?}",
        routed_task.tags
    );
    let receipt: Value = serde_json::from_str(
        &fx.runtime
            .get_task_comments(&os_routed)
            .unwrap()
            .last()
            .unwrap()
            .message,
    )
    .unwrap();
    let audit_path = receipt["assessment_artifact"].as_str().unwrap();
    let audit_artifact = fx
        .runtime
        .get_task_artifact(&os_routed, audit_path)
        .unwrap()
        .expect("sealed assessment artifact");
    let audit: Value = serde_json::from_slice(&audit_artifact.content).unwrap();
    assert!(
        !audit["native_os_hold"].is_null(),
        "audit should still record native_os_hold: {audit}"
    );

    assess_with(
        &fx,
        &fx.runtime,
        &untagged_os,
        json!({
            "required_os": [{
                "criterion": 1,
                "os": "linux",
                "evidence": "Criterion 1 requires a native Linux sandbox.",
            }],
        }),
    );
    assert!(
        !fx.runtime
            .get_task(&untagged_os)
            .unwrap()
            .tags
            .iter()
            .any(|tag| tag == "os:linux"),
        "existing os:macos tag must keep required_os untagged"
    );

    let automation = fx.automation_runtime();
    assess_with(
        &fx,
        &automation,
        &machine_bound,
        json!({
            "required_machine": [{
                "criterion": 1,
                "machine": "fixture-machine",
                "evidence": "Criterion 1 reads live state on fixture-machine.",
            }],
        }),
    );

    assess_with(
        &fx,
        &fx.runtime,
        &host_operational,
        json!({
            "disposition": "host_operational",
            "context_files_after": [],
            "evidence": "Restart the host daemon directly on the machine.",
        }),
    );

    assess_with(&fx, &fx.runtime, &operator_validation, json!({}));

    let mandate = fx.mandate(&os_routed);
    fx.grant(&mandate, &["promote_held"]);
    let run = fx.cycle_run(&mandate, "held-promotion-admission-holds");
    fx.begin_cycle(&run, Utc::now());
    let promote =
        |id: &str| fx.agent_call("orbit.task.update", json!({"id": id, "status": "backlog"}));
    fx.as_cycle_agent(&run, || {
        for id in [
            &untagged_os,
            &machine_bound,
            &host_operational,
            &operator_validation,
        ] {
            let error = promote(id).unwrap_err();
            assert!(error.contains("pilot_admission_hold"), "{id}: {error}");
            assert_eq!(
                fx.runtime.get_task(id).unwrap().status,
                TaskStatus::Proposed
            );
        }

        let result = promote(&os_routed).unwrap();
        assert_eq!(result["status"], "backlog", "{result}");
        let comments = fx.runtime.get_task_comments(&os_routed).unwrap();
        let audit_comment = &comments.last().unwrap().message;
        assert!(
            audit_comment.contains(&os_routed_assessment),
            "{audit_comment}"
        );
        assert!(audit_comment.contains("promote_held"), "{audit_comment}");
    });
}

#[test]
fn resident_promote_held_keeps_native_holds_despite_no_pilot() {
    if !crate::dispatch_admission::isolated(
        "resident_cycle::promote_held::resident_promote_held_keeps_native_holds_despite_no_pilot",
    ) {
        return;
    }
    let fx = fixture();
    let proposal = |title: &str, task_type: TaskType, tags: &[&str], criterion: &str| {
        fx.add_task(TaskAddParams {
            title: title.into(),
            description: "Adjust host-specific validation handling.".into(),
            acceptance_criteria: vec![criterion.into()],
            task_type: Some(task_type),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
    };

    // Already carrying `no-pilot` before the explicit assessment: preparation
    // still assesses the proposal and apply seals the existing tags with the
    // native finding, so the freshness checks succeed.
    let untagged_no_pilot = proposal(
        "No-pilot conflicting OS tag leaves required_os untagged",
        TaskType::Bug,
        &["no-auto-approve", "no-pilot", "os:macos"],
        "Linux sandbox validation succeeds.",
    );
    let machine_no_pilot = proposal(
        "No-pilot owner-machine validation required",
        TaskType::Chore,
        &["no-auto-approve", "no-pilot"],
        "Owner machine live store check passes.",
    );
    let satisfied_no_pilot = proposal(
        "No-pilot Linux-only bug fix",
        TaskType::Bug,
        &["no-auto-approve", "no-pilot"],
        "Linux sandbox validation succeeds.",
    );

    assess_with(
        &fx,
        &fx.runtime,
        &untagged_no_pilot,
        json!({
            "required_os": [{
                "criterion": 1,
                "os": "linux",
                "evidence": "Criterion 1 requires a native Linux sandbox.",
            }],
        }),
    );
    assert!(
        !fx.runtime
            .get_task(&untagged_no_pilot)
            .unwrap()
            .tags
            .iter()
            .any(|tag| tag == "os:linux"),
        "existing os:macos tag must keep required_os untagged"
    );
    assert!(
        fx.runtime
            .get_task(&untagged_no_pilot)
            .unwrap()
            .tags
            .iter()
            .any(|tag| tag == "no-pilot"),
        "apply must seal the existing no-pilot tag"
    );

    let automation = fx.automation_runtime();
    assess_with(
        &fx,
        &automation,
        &machine_no_pilot,
        json!({
            "required_machine": [{
                "criterion": 1,
                "machine": "fixture-machine",
                "evidence": "Criterion 1 reads live state on fixture-machine.",
            }],
        }),
    );
    assert!(
        fx.runtime
            .get_task(&machine_no_pilot)
            .unwrap()
            .tags
            .iter()
            .any(|tag| tag == "no-pilot"),
        "apply must seal the existing no-pilot tag"
    );

    let satisfied_assessment = assess_with(
        &fx,
        &fx.runtime,
        &satisfied_no_pilot,
        json!({
            "required_os": [{
                "criterion": 1,
                "os": "linux",
                "evidence": "Criterion 1 requires a native Linux sandbox.",
            }],
        }),
    );
    let satisfied_task = fx.runtime.get_task(&satisfied_no_pilot).unwrap();
    assert!(
        satisfied_task.tags.iter().any(|tag| tag == "os:linux"),
        "pilot apply should add os:linux: {:?}",
        satisfied_task.tags
    );
    assert!(
        satisfied_task.tags.iter().any(|tag| tag == "no-pilot"),
        "apply must seal the existing no-pilot tag: {:?}",
        satisfied_task.tags
    );

    let mandate = fx.mandate(&satisfied_no_pilot);
    fx.grant(&mandate, &["promote_held"]);
    let run = fx.cycle_run(&mandate, "held-promotion-no-pilot-native");
    fx.begin_cycle(&run, Utc::now());
    let promote =
        |id: &str| fx.agent_call("orbit.task.update", json!({"id": id, "status": "backlog"}));
    fx.as_cycle_agent(&run, || {
        for id in [&untagged_no_pilot, &machine_no_pilot] {
            let error = promote(id).unwrap_err();
            assert!(error.contains("pilot_admission_hold"), "{id}: {error}");
            assert_eq!(
                fx.runtime.get_task(id).unwrap().status,
                TaskStatus::Proposed
            );
        }

        let result = promote(&satisfied_no_pilot).unwrap();
        assert_eq!(result["status"], "backlog", "{result}");
        let comments = fx.runtime.get_task_comments(&satisfied_no_pilot).unwrap();
        let audit_comment = &comments.last().unwrap().message;
        assert!(
            audit_comment.contains(&satisfied_assessment),
            "{audit_comment}"
        );
        assert!(audit_comment.contains("promote_held"), "{audit_comment}");
    });
}
