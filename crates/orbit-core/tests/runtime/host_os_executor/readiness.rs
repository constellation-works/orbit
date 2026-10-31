//! Owner readiness must distinguish its local OS refusal from a follower's lock wait.
use super::*;
use orbit_store::contracts::{AdmissionCrewCapability, ExecutorCapability};

fn mixed_host_lock_readiness() {
    let (_root, owner) = runtime_on(HostOs::Linux, POOL);
    let owner = owner.with_registered_host_oses([("hm_mac".to_string(), HostOs::Macos)]);
    let second = add(&owner, "mixed-second-holder", Some("sol"), &[]).unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": second.id, "status": "in-progress", "model": "codex"}),
        )
        .unwrap();
    let second = owner.get_task(&second.id).unwrap();
    let holder = add(&owner, "mixed-holder", Some("sol"), &[]).unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({
                "id": holder.id, "status": "in-progress", "model": "codex",
            }),
        )
        .unwrap();
    let holder = owner.get_task(&holder.id).unwrap();
    let waiting = add(&owner, "mixed-waiter", Some("sol"), &["os:macos"]).unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({
                "id": waiting.id, "context_files": [holder.context_files[0], second.context_files[0]], "model": "codex",
            }),
        )
        .unwrap();
    let waiting = owner.get_task(&waiting.id).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "task_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({"task_ids": [holder.id, second.id]})),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .unwrap();
    owner
        .write_run_state(
            &run.run_id,
            &PipelineState::new(
                run.run_id.clone(),
                run.job_id,
                json!({"task_ids": [holder.id, second.id]}),
            ),
        )
        .unwrap();
    let connection = rusqlite::Connection::open(owner.global_root().join("orbit.db")).unwrap();
    let capability = ExecutorCapability {
        os: Some(HostOs::Macos),
        browser_kit: true,
        ..Default::default()
    };
    let seed = |capability: &ExecutorCapability, age: i64| {
        connection.execute("INSERT OR REPLACE INTO executor_census(workspace_id, machine_id, capability_json, run_id, seen_at) VALUES (?1, 'hm_mac', ?2, 'mac-drain', ?3)",
            rusqlite::params![owner.workspace_id().unwrap(), serde_json::to_string(capability).unwrap(),
                (chrono::Utc::now() - chrono::Duration::seconds(age)).to_rfc3339()]).unwrap();
    };
    let read = || {
        owner
            .workspace_auto_readiness(std::slice::from_ref(&waiting.id), None, 50, &[])
            .unwrap()["tasks"][0]
            .clone()
    };
    let missing = read();
    assert_eq!(missing["reason"], "host_os_mismatch");
    seed(&capability, 0);
    let entry = read();
    assert_eq!(
        entry["reason"], "context_lock_conflict",
        "a compatible Mac follower makes the shared footprint the actionable wait: {entry}"
    );
    assert_eq!(
        entry["eligible"], false,
        "owner must never admit a macOS task"
    );
    assert_eq!(entry["conflicts"][0]["locking_task_id"], holder.id);
    assert_eq!(entry["conflicts"][1]["locking_task_id"], second.id);
    assert_eq!(
        entry["conflicts"][0]["requested_file"],
        holder.context_files[0]
    );
    assert_eq!(entry["run_ids"], json!([run.run_id]));
    assert_eq!(entry["host_scope"], "local");
    assert_eq!(entry["executor_evidence"]["status"], "compatible_recent");
    assert!(
        admitted(&backlog(&owner)).is_empty(),
        "diagnostics cannot weaken OS admission"
    );

    // Stopping requests ages out evidence, rather than treating a week-old claim
    // or host registration as proof of an active eligible executor.
    seed(&capability, 601);
    let stale = read();
    assert_eq!(stale["reason"], "host_os_mismatch", "{stale}");
    assert_eq!(stale["executor_evidence"]["executors"][0]["state"], "stale");
    let mut excluded_crew = capability.clone();
    excluded_crew.crews = Some(AdmissionCrewCapability {
        runnable: Some(Vec::new()),
        default_crew: None,
        excluded: Vec::new(),
        usage_pressure: Vec::new(),
    });
    seed(&excluded_crew, 0);
    let ineligible = read();
    assert_eq!(
        ineligible["reason"], "host_os_mismatch",
        "OS alone is not an eligible executor"
    );
    seed(&capability, 0);
    assert_eq!(
        read()["reason"],
        "context_lock_conflict",
        "a new compatible pull restores the lock wait"
    );

    // Releasing the footprint preserves the local OS refusal and never claims
    // that the follower can pass every other admission gate.
    jobs.finalize_job_run(&run.run_id, JobRunState::Success, chrono::Utc::now(), None)
        .unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": holder.id, "status": "backlog", "model": "codex"}),
        )
        .unwrap();
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": second.id, "status": "backlog", "model": "codex"}),
        )
        .unwrap();
    let free = read();
    assert_eq!(free["reason"], "follower_admission_pending", "{free}");
    assert_eq!(free["eligible"], false);
    assert!(
        free.get("conflicts").is_none(),
        "released locks cannot linger: {free}"
    );

    // Read errors are disclosed, with no optimistic fallback to registration.
    connection.execute_batch("ALTER TABLE executor_census RENAME TO broken_census; CREATE TABLE executor_census(workspace_id TEXT);").unwrap();
    let unknown = read();
    assert_eq!(unknown["reason"], "host_os_mismatch", "{unknown}");
    assert_eq!(unknown["executor_evidence"]["status"], "unavailable");
    if let Some(path) = std::env::var_os("ORBIT_MIXED_HOST_READINESS_EVIDENCE") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({
                "task": waiting, "holders": [holder, second],
                "locked": entry, "missing": missing, "stale": stale,
                "ineligible": ineligible, "free": free, "unavailable": unknown,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

crate::scenario_table!(mixed_host_readiness_scenarios, mixed_host_lock_readiness);
