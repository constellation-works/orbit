//! Owner-side placement against the executor census, and frozen-batch
//! ordering, through the pull tool.

use orbit_types::task::HostOs;

use super::*;

const DK: &str = "dk-server-1";

fn pull(pair: &Pair, request_id: &str, os: HostOs) -> Value {
    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    pair.wire
        .call(
            "",
            "orbit.task.pull",
            json!({
                "request_id": request_id,
                "caller_version": probe["binary_version"],
                "caller_schema": probe["protocol_schema"],
                "caller_fingerprint": probe["protocol_fingerprint"],
                "review_gate": true,
                "ship": probe["ship"],
                "run_context": {"run_id": "order-drain", "job_name": "workspace_pull_pipeline"},
                "os": os,
            }),
        )
        .unwrap()
}

/// A running local drain on the owner, whose admission pass declares the
/// owner's census row.
fn owner_drain(owner: &OrbitRuntime) -> String {
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({})),
            None,
        )
        .unwrap();
    owner
        .write_run_state(
            &run.run_id,
            &PipelineState::new(run.run_id.clone(), run.job_id, json!({})),
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, chrono::Utc::now(), std::process::id())
        .unwrap();
    run.run_id
}

fn local_order(owner: &OrbitRuntime) -> Value {
    owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap()["task_ids"]
        .clone()
}

/// The Mac / dk-server-1 / owner mix [ORB-17304]. Each executor takes, within
/// a band, what only it can run among the live census: the Mac its macOS
/// task over higher-priority fungible work, the owner (locally) its Linux
/// task once the Mac is live, and dk-server-1 the fungible task it shares
/// with both. Each receipt records the class and the census.
fn census_orders_the_mac_dk_server_and_owner_mix() {
    let root = Arc::new(TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap());
    let (owner, repo) = open_runtime(root.path(), OWNER);
    let owner = owner.with_host_os(Some(HostOs::Linux));
    // Keep the census independent of provider CLIs installed on the test
    // host: the owner can run the shared Sol crew, but its crew preflight
    // excludes Opus; followers below get launchable provider stand-ins.
    launchable_providers(&owner);
    let missing_claude = root.path().join("missing-claude-cli");
    follower_cli(&owner, "claude", &missing_claude.to_string_lossy());
    let fungible = backlog_task(&owner, &repo, "src/fungible.rs", Some("sol"));
    let linux = backlog_task(&owner, &repo, "src/linux.rs", Some("sol"));
    let mac = backlog_task(&owner, &repo, "src/mac.rs", Some("opus"));
    for (task, priority, tags) in [
        (&fungible, "high", json!([])),
        (&linux, "low", json!(["os:linux"])),
        (&mac, "medium", json!(["os:macos"])),
    ] {
        owner
            .run_tool(
                "orbit.task.update",
                json!({"id": task, "priority": priority, "tags": tags, "model": "codex"}),
            )
            .unwrap();
    }
    // A pass with no free slot declares the owner's row and admits nothing,
    // so no owner-wave footprint holds the tasks back from the followers.
    let drain = owner_drain(&owner);
    owner
        .run_deterministic(
            "classify_workspace_auto_tasks",
            &json!({}),
            &json!({"run_id": drain, "max_active_leaf_runs": 0}),
            ToolContext::default(),
        )
        .unwrap();
    // The owner alone in the census: the canonical order.
    assert_eq!(local_order(&owner), json!([fungible, linux]));

    let pair = Pair::follower_of(
        root,
        owner,
        repo,
        vec![fungible.clone(), linux.clone(), mac.clone()],
        FOLLOWER,
    );
    let reply = pull(&pair, "mac-first", HostOs::Macos);
    let receipt = &reply["receipt"];
    assert_eq!(receipt["claim"]["task_id"], mac, "{reply}");
    assert_eq!(receipt["affinity"]["class"], "exclusive", "{reply}");
    assert_eq!(
        receipt["affinity"]["census"],
        json!([FOLLOWER, OWNER]),
        "{reply}"
    );
    assert_eq!(pair.owner_status(&fungible), "backlog");

    // With the Mac live, the Linux task is the owner's alone.
    assert_eq!(local_order(&pair.wire.owner), json!([linux, fungible]));

    let dk = pair.another_host(DK);
    let reply = pull(&dk, "dk-next", HostOs::Linux);
    let receipt = &reply["receipt"];
    assert_eq!(receipt["claim"]["task_id"], fungible, "{reply}");
    assert_eq!(receipt["affinity"]["class"], "fungible", "{reply}");
    assert_eq!(receipt["affinity"]["eligible_executors"], 3, "{reply}");
    assert_eq!(
        receipt["affinity"]["census"],
        json!([DK, FOLLOWER, OWNER]),
        "{reply}"
    );
    assert_eq!(pair.owner_status(&linux), "backlog");
}

fn pull_matches_local_dispatch_for_an_expiring_frozen_batch() {
    let pair = Pair::new(3);
    let (older, expiring, distant) = (&pair.tasks[0], &pair.tasks[1], &pair.tasks[2]);
    let owner = &pair.wire.owner;
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": older, "type": "bug", "model": "codex"}),
        )
        .unwrap();
    for task in [expiring, distant] {
        owner
            .run_tool(
                "orbit.task.update",
                json!({"id": task, "tags": ["no-diff-expected"], "model": "codex"}),
            )
            .unwrap();
    }
    crate::dispatch_admission::admitted_frozen_batch(
        owner,
        "code-review",
        expiring,
        chrono::Duration::minutes(90),
    );
    crate::dispatch_admission::admitted_frozen_batch(
        owner,
        "friction-curation",
        distant,
        chrono::Duration::hours(5),
    );
    let local = owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    assert_eq!(
        local["task_ids"],
        json!([expiring, older, distant]),
        "{local}"
    );
    let reply = pull(&pair, "expiry-first", HostOs::Linux);
    assert_eq!(
        reply["receipt"]["claim"]["task_id"], local["task_ids"][0],
        "{reply}"
    );
    assert_eq!(pair.owner_status(older), "backlog");
    assert_eq!(pair.owner_status(distant), "backlog");
}

/// [ORB-17773] A follower that reports weekly pressure is offered first
/// the tied task whose pool contains its highest-pressure crew, while
/// owner-local dispatch — which has no pressure input — keeps age order
/// for the same backlog.
fn follower_pressure_offers_the_pressured_pool_first_but_local_order_is_unchanged() {
    const CONFIG: &str = "\
[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\"]\n\
hard_complexity_crews = [\"opus\", \"sol\"]\n\n\
[crews.sol]\nprovider = \"codex\"\nmodel = \"gpt-sol\"\n\n\
[crews.opus]\nprovider = \"claude\"\nmodel = \"claude-opus\"\n";
    let pair = Pair::with_configs(CONFIG, CONFIG, &[Some("sol")]);
    let older = pair.tasks[0].clone();
    let newer = backlog_task_with_input(
        &pair.wire.owner,
        &pair.owner_repo,
        "src/pressured.rs",
        Some("sol"),
        json!({"complexity": "hard"}),
    );
    assert_eq!(
        local_order(&pair.wire.owner),
        json!([older, newer]),
        "owner-local dispatch has no pressure input"
    );

    let probe = pair.wire.call("", "orbit.drain.probe", json!({})).unwrap();
    let reply = pair
        .wire
        .call(
            "",
            "orbit.task.pull",
            json!({
                "request_id": "pressure-first",
                "caller_version": probe["binary_version"],
                "caller_schema": probe["protocol_schema"],
                "caller_fingerprint": probe["protocol_fingerprint"],
                "review_gate": true,
                "ship": probe["ship"],
                "run_context": {"run_id": "pressure-drain", "job_name": "workspace_pull_pipeline"},
                "os": HostOs::Linux,
                "crews": {
                    "runnable": ["opus", "sol"],
                    "default_crew": "sol",
                    "usage_pressure": [
                        {"crew": "opus", "pressure_milli": 25000},
                        {"crew": "sol", "pressure_milli": 1000},
                    ],
                },
            }),
        )
        .unwrap();
    assert_eq!(reply["receipt"]["claim"]["task_id"], newer, "{reply}");
    assert_eq!(pair.owner_status(&older), "backlog");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    census_orders_the_mac_dk_server_and_owner_mix_scenarios,
    census_orders_the_mac_dk_server_and_owner_mix,
    pull_matches_local_dispatch_for_an_expiring_frozen_batch,
    follower_pressure_offers_the_pressured_pool_first_but_local_order_is_unchanged
);
