//! Owner-side placement and frozen-batch ordering through the pull tool.

use orbit_types::task::HostOs;

use super::*;

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

#[test]
fn macos_pull_prefers_work_the_linux_owner_cannot_run() {
    if !isolated(
        module_path!(),
        "macos_pull_prefers_work_the_linux_owner_cannot_run",
    ) {
        return;
    }
    let root = Arc::new(TempDir::new().unwrap());
    let (owner, repo) = open_runtime(root.path(), OWNER);
    let owner = owner.with_host_os(Some(HostOs::Linux));
    let older = backlog_task(&owner, &repo, "src/older.rs", None);
    let mac = backlog_task(&owner, &repo, "src/mac.rs", None);
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": mac, "tags": ["os:macos"], "model": "codex"}),
        )
        .unwrap();
    let pair = Pair::follower_of(
        root,
        owner,
        repo,
        vec![older.clone(), mac.clone()],
        FOLLOWER,
    );
    let reply = pull(&pair, "mac-first", HostOs::Macos);
    assert_eq!(reply["receipt"]["claim"]["task_id"], mac, "{reply}");
    assert_eq!(pair.owner_status(&older), "backlog");
}

#[test]
fn pull_matches_local_dispatch_for_an_expiring_frozen_batch() {
    if !isolated(
        module_path!(),
        "pull_matches_local_dispatch_for_an_expiring_frozen_batch",
    ) {
        return;
    }
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
