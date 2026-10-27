//! Path-lease operations at the runtime boundary: request, renew, release,
//! show, list and check, with the `lease.*` policy read from workspace config.

use super::tool_support::{create_context_task, invalid_input_message};
use orbit_common::test_env::ScopedEnv;
use orbit_core::runtime::OrbitRuntime;
use orbit_core::runtime::task::locks::lease;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use std::path::PathBuf;
use tempfile::tempdir_in;

/// The managed-run envelope plus every variable `agent_context_declared`
/// reads.
const CALLER_ENV: [&str; 9] = [
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_RUN_ID",
    "ORBIT_ACTIVITY_ID",
    "ORBIT_STEP_INDEX",
    "ORBIT_TASK_ACTOR_KIND",
];

/// Clear the caller envelope, declaring `agent` as `ORBIT_AGENT_NAME` when
/// given. One guard at a time: guards share a process-wide lock.
fn caller_env(agent: Option<&'static str>) -> ScopedEnv {
    orbit_common::test_env::scoped(
        CALLER_ENV.map(|name| (name, agent.filter(|_| name == "ORBIT_AGENT_NAME"))),
    )
}

/// A runtime whose workspace `config.toml` is `config`, with `src/lib.rs`,
/// `src/other.rs` and `docs/guide.md` on disk.
fn lease_runtime(config: &str) -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    let root = tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(workspace_root.join("config.toml"), config).expect("write workspace config");
    for (path, body) in [
        ("src/lib.rs", "pub fn ok() {}\n"),
        ("src/other.rs", "pub fn other() {}\n"),
        ("docs/guide.md", "# Guide\n"),
    ] {
        let path = repo_root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).expect("create parent");
        std::fs::write(path, body).expect("write fixture file");
    }
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

fn enabled_runtime() -> (tempfile::TempDir, OrbitRuntime, PathBuf) {
    lease_runtime("[lease]\nenabled = true\n")
}

fn request(runtime: &OrbitRuntime, mut input: Value) -> Value {
    input["model"] = json!("codex");
    super::tool_support::run_tool_as_operator(runtime, "orbit.lease.request", input)
        .expect("lease request")
}

fn holder_ids(conflicts: &Value) -> Vec<(String, String)> {
    conflicts
        .as_array()
        .expect("holder array")
        .iter()
        .map(|holder| {
            (
                holder["held_by"].as_str().unwrap().to_string(),
                holder["held_by_id"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn lease_audit_commands(runtime: &OrbitRuntime) -> Vec<String> {
    runtime
        .list_audit_events_with_kind(None, None, Some("lease".into()), None, None, 200)
        .expect("list lease audit events")
        .into_iter()
        .map(|event| event.command)
        .collect()
}

/// Move a lease's `expires_at` into the past without ending it.
fn backdate(runtime: &OrbitRuntime, lease_id: &str) {
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    let expires_at = chrono::Utc::now() - chrono::Duration::minutes(1);
    assert_eq!(
        connection
            .execute(
                "UPDATE task_reservations SET expires_at=?1 WHERE reservation_id=?2",
                rusqlite::params![expires_at.to_rfc3339(), lease_id],
            )
            .unwrap(),
        1
    );
}

#[test]
fn notices_survive_restart_and_capture_lease_and_task_transitions() {
    if !super::isolate(
        module_path!(),
        "notices_survive_restart_and_capture_lease_and_task_transitions",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) =
        lease_runtime("[lease]\nenabled = true\n[lease.hard]\nhuman = \"never\"\n");
    let first = request(
        &runtime,
        json!({"principal":"alice", "selectors":["dir:src"], "mode":"hard"}),
    );
    assert_eq!(first["downgraded"], true);
    let id = first["lease"]["lease_id"].as_str().unwrap();
    let second = request(
        &runtime,
        json!({"principal":"bob", "selectors":["file:src/lib.rs"]}),
    );
    let other_id = second["lease"]["lease_id"].as_str().unwrap();
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["file:src/lib.rs"],
    );
    super::tool_support::run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({"files":["file:src/other.rs"], "model":"codex"}),
    )
    .unwrap();
    lease::release(&runtime, json!({"lease_id":id}), None, None).unwrap();
    lease::release(&runtime, json!({"lease_id":id}), None, None).unwrap();
    backdate(&runtime, other_id);
    lease::show(&runtime, json!({"lease_id":other_id})).unwrap();
    lease::show(&runtime, json!({"lease_id":other_id})).unwrap();
    let read = |runtime: &OrbitRuntime| {
        super::tool_support::run_tool_as_operator(
            runtime,
            "orbit.lease.notices",
            json!({"after":0, "limit":500}),
        )
        .unwrap()
    };
    let page = read(&runtime);
    let notices = page["notices"].as_array().unwrap();
    assert!(
        notices
            .windows(2)
            .all(|pair| pair[0]["seq"].as_u64() < pair[1]["seq"].as_u64())
    );
    assert!(
        notices
            .iter()
            .all(|n| n["lease_id"] == id || n["lease_id"] == other_id),
        "drain tasks receive no notices"
    );
    assert_eq!(
        notices.iter().filter(|n| n["kind"] == "released").count(),
        1
    );
    assert_eq!(notices.iter().filter(|n| n["kind"] == "expired").count(), 1);
    assert!(
        notices
            .iter()
            .any(|n| n["kind"] == "overlap" && n["payload"]["task_id"] == task.id)
    );
    assert!(
        notices
            .iter()
            .any(|n| n["kind"] == "overlap" && n["payload"]["reservation_id"].is_string())
    );
    assert!(notices.iter().any(|n| n["kind"] == "granted"
        && n["lease_id"] == id
        && n["payload"]["mode"] == "advisory"));
    assert!(
        super::tool_support::run_tool_as_operator(
            &runtime,
            "orbit.lease.notices",
            json!({"after": page["cursor"]})
        )
        .unwrap()["notices"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let reopened =
        OrbitRuntime::from_roots(&runtime.global_root(), &repo_root.join(".orbit")).unwrap();
    assert_eq!(read(&reopened), page);
    let other_workspace = repo_root.parent().unwrap().join("another/.orbit");
    std::fs::create_dir_all(&other_workspace).unwrap();
    std::fs::write(
        other_workspace.join("config.toml"),
        "[lease]\nenabled = true\n",
    )
    .unwrap();
    let isolated = OrbitRuntime::from_roots(&runtime.global_root(), &other_workspace).unwrap();
    assert!(read(&isolated)["notices"].as_array().unwrap().is_empty());
}

#[test]
fn landed_facts_notify_only_matching_bases_once() {
    if !super::isolate(
        module_path!(),
        "landed_facts_notify_only_matching_bases_once",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) = enabled_runtime();
    let repo = git2::Repository::init(&repo_root).unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("src/lib.rs")).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let initial = repo
        .commit(
            Some("refs/heads/agent-main"),
            &signature,
            &signature,
            "initial",
            &tree,
            &[],
        )
        .unwrap();
    repo.reference("refs/heads/unrelated", initial, true, "fixture")
        .unwrap();
    let first = request(
        &runtime,
        json!({"principal":"alice", "selectors":["dir:src"], "base":"agent-main"}),
    );
    let id = first["lease"]["lease_id"].as_str().unwrap();
    request(
        &runtime,
        json!({"principal":"bob", "selectors":["dir:src"], "base":"unrelated"}),
    );
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();
    index.add_path(std::path::Path::new("src/lib.rs")).unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let commit = repo
        .commit(
            Some("refs/heads/agent-main"),
            &signature,
            &signature,
            "landed",
            &tree,
            &[&repo.find_commit(initial).unwrap()],
        )
        .unwrap();
    let task = create_context_task(&runtime, &repo_root, TaskStatus::Done, &["file:src/lib.rs"]);
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(&runtime.global_root()),
    )
    .unwrap();
    orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task
    .history
    .update_task_history(
        &task.id,
        orbit_store::contracts::TaskHistoryUpdateParams {
            actor: "fixture".into(),
            append_history: vec![orbit_types::task::TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: "fixture".into(),
                event: "landing_completed".into(),
                note: Some(
                    json!({"delivery":"local_candidate", "candidate":commit.to_string()})
                        .to_string(),
                ),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        },
    )
    .unwrap();
    for _ in 0..2 {
        runtime
            .run_tool("orbit.codebase.show", json!({"path":"src/lib.rs"}))
            .unwrap();
    }
    let page =
        super::tool_support::run_tool_as_operator(&runtime, "orbit.lease.notices", json!({}))
            .unwrap();
    let landings: Vec<_> = page["notices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "overlap_landed")
        .collect();
    assert_eq!(
        landings.len(),
        1,
        "repeated facts scans must not duplicate or notify a base that does not contain the commit: {page}"
    );
    assert_eq!(landings[0]["lease_id"], id);
    assert_eq!(landings[0]["payload"]["task_id"], task.id);
    assert_eq!(landings[0]["payload"]["commit"], commit.to_string());
    assert_eq!(landings[0]["payload"]["paths"], json!(["file:src/lib.rs"]));
}

#[test]
fn hard_requests_are_refused_by_holders_and_advisory_ones_are_granted() {
    if !super::isolate(
        module_path!(),
        "hard_requests_are_refused_by_holders_and_advisory_ones_are_granted",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) = enabled_runtime();
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["file:src/lib.rs"],
    );

    let advisory = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/lib.rs"]}),
    );
    assert_eq!(advisory["granted"], true, "{advisory}");
    assert_eq!(advisory["lease"]["mode"], "advisory", "{advisory}");
    assert_eq!(advisory["lease"]["state"], "granted", "{advisory}");
    assert_eq!(
        holder_ids(&advisory["overlaps"]),
        [("task".to_string(), task.id.clone())],
        "an advisory grant names the holders it overlaps: {advisory}"
    );

    let refused = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/lib.rs"], "mode": "hard", "wait": false}),
    );
    assert_eq!(refused["granted"], false, "{refused}");
    assert_eq!(refused["queued"], false, "{refused}");
    assert_eq!(
        holder_ids(&refused["conflicts"]),
        [("task".to_string(), task.id.clone())],
        "{refused}"
    );

    let hard = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/other.rs"], "mode": "hard"}),
    );
    assert_eq!(hard["granted"], true, "{hard}");
    assert_eq!(hard["lease"]["mode"], "hard", "{hard}");
    let hard_id = hard["lease"]["lease_id"].as_str().unwrap().to_string();
    let listing =
        super::tool_support::run_tool_as_operator(&runtime, "orbit.task.locks", json!({}))
            .expect("list locks");
    let listed = listing["by_lease"]
        .as_array()
        .expect("lease holders in lock listing")
        .iter()
        .find(|lease| lease["lease_id"] == hard_id)
        .expect("active hard lease listed");
    assert_eq!(listed["mode"], "hard");
    assert_eq!(listed["principal"], "alice");
    assert_eq!(listed["priority"], "normal");
    assert!(listed["expires_at"].is_string());

    let second = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["dir:src"], "mode": "hard", "wait": false}),
    );
    assert_eq!(second["granted"], false, "{second}");
    assert!(
        holder_ids(&second["conflicts"]).contains(&("task".to_string(), task.id.clone())),
        "{second}"
    );
    let overlapping = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["file:src/other.rs"], "mode": "hard", "wait": false}),
    );
    assert_eq!(overlapping["granted"], false, "{overlapping}");
    assert_eq!(
        holder_ids(&overlapping["conflicts"]),
        [("lease".to_string(), hard_id.clone())],
        "a later overlapping hard request is refused by the hard lease: {overlapping}"
    );

    let past_hard = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["file:src/other.rs"]}),
    );
    assert_eq!(past_hard["granted"], true, "{past_hard}");
    assert_eq!(
        holder_ids(&past_hard["overlaps"]),
        [("lease".to_string(), hard_id)],
        "{past_hard}"
    );

    let commands = lease_audit_commands(&runtime);
    for command in ["lease.request.granted", "lease.request.refused"] {
        assert!(
            commands.iter().any(|c| c == command),
            "{command}: {commands:?}"
        );
    }
}

#[test]
fn hard_surface_drops_unlocked_context_paths() {
    if !super::isolate(module_path!(), "hard_surface_drops_unlocked_context_paths") {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) = lease_runtime(
        "[lease]\nenabled = true\n\n[workflow]\nunlocked_context_paths = [\"docs/**\"]\n",
    );
    // A docs-only task keeps its unlocked entries, so it holds the guide.
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["file:docs/guide.md"],
    );

    let granted = request(
        &runtime,
        json!({
            "principal": "alice",
            "selectors": ["file:docs/guide.md", "file:src/lib.rs"],
            "mode": "hard",
        }),
    );
    assert_eq!(
        granted["granted"], true,
        "the only overlap with {} is on an unlocked path: {granted}",
        task.id
    );
    assert_eq!(
        granted["lease"]["selectors"],
        json!(["file:src/lib.rs"]),
        "{granted}"
    );
    assert_eq!(
        granted["unlocked_selectors"],
        json!(["file:docs/guide.md"]),
        "{granted}"
    );
    let lease_id = granted["lease"]["lease_id"].as_str().unwrap();
    let shown = lease::show(&runtime, json!({"lease_id": lease_id})).expect("show lease");
    assert_eq!(
        shown["lease"]["selectors"],
        json!(["file:src/lib.rs"]),
        "the stored hard surface excludes the unlocked path: {shown}"
    );
}

#[test]
fn principal_class_policy_downgrades_hard_and_caps_priority() {
    if !super::isolate(
        module_path!(),
        "principal_class_policy_downgrades_hard_and_caps_priority",
    ) {
        return;
    }
    let (_root, runtime, _repo_root) = {
        let _env = caller_env(None);
        lease_runtime(
            "[lease]\nenabled = true\nhard = { agent = \"never\" }\nmax_priority = { agent = \"normal\" }\n",
        )
    };

    {
        let _agent = caller_env(Some("claude"));
        let downgraded = request(
            &runtime,
            json!({"principal": "alice/claude-code", "selectors": ["file:src/lib.rs"], "mode": "hard"}),
        );
        assert_eq!(downgraded["granted"], true, "{downgraded}");
        assert_eq!(
            downgraded["lease"]["principal_class"], "agent",
            "{downgraded}"
        );
        assert_eq!(downgraded["requested_mode"], "hard", "{downgraded}");
        assert_eq!(downgraded["lease"]["mode"], "advisory", "{downgraded}");
        assert_eq!(downgraded["downgraded"], true, "{downgraded}");
        assert!(
            downgraded["notice"]
                .as_str()
                .is_some_and(|notice| notice.contains("granted as advisory")
                    && notice.contains("lease.hard.agent")),
            "{downgraded}"
        );

        let message = invalid_input_message(lease::request(
            &runtime,
            json!({"principal": "alice/claude-code", "selectors": ["file:src/lib.rs"], "priority": "high"}),
            None,
            Some("codex".into()),
        ));
        assert!(message.contains("lease.max_priority.agent"), "{message}");
    }

    let _human = caller_env(None);
    let human = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/other.rs"], "mode": "hard", "priority": "high"}),
    );
    assert_eq!(human["granted"], true, "{human}");
    assert_eq!(human["lease"]["principal_class"], "human", "{human}");
    assert_eq!(human["lease"]["mode"], "hard", "{human}");
    assert_eq!(human["lease"]["priority"], "high", "{human}");
    assert_eq!(human["downgraded"], false, "{human}");
}

#[test]
fn renew_release_and_expiry_end_leases_and_are_audited() {
    if !super::isolate(
        module_path!(),
        "renew_release_and_expiry_end_leases_and_are_audited",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, _repo_root) = enabled_runtime();
    let expires = |lease: &Value| {
        chrono::DateTime::parse_from_rfc3339(lease["expires_at"].as_str().unwrap()).unwrap()
    };

    let granted = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/lib.rs"], "mode": "hard", "ttl_seconds": 60}),
    );
    let lease_id = granted["lease"]["lease_id"].as_str().unwrap().to_string();
    let renewed = lease::renew(&runtime, json!({"lease_id": lease_id, "ttl_seconds": 3600}))
        .expect("renew lease");
    assert_eq!(renewed["renewed"], true, "{renewed}");
    assert!(
        expires(&renewed["lease"]) > expires(&granted["lease"]),
        "renew moves expires_at forward: {granted} -> {renewed}"
    );
    let message = invalid_input_message(lease::renew(
        &runtime,
        json!({"lease_id": lease_id, "ttl_seconds": 14401}),
    ));
    assert!(message.contains("ttl_seconds"), "{message}");

    let released = lease::release(
        &runtime,
        json!({"lease_id": lease_id}),
        None,
        Some("codex".into()),
    )
    .expect("release lease");
    assert_eq!(released["released"], true, "{released}");
    assert_eq!(released["lease"]["state"], "released", "{released}");
    let again = lease::release(
        &runtime,
        json!({"lease_id": lease_id}),
        None,
        Some("codex".into()),
    )
    .expect("release twice");
    assert_eq!(again["released"], false, "{again}");
    let message = invalid_input_message(lease::renew(&runtime, json!({"lease_id": lease_id})));
    assert!(message.contains("not active"), "{message}");

    let unrenewed = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["file:src/lib.rs"], "mode": "hard"}),
    );
    assert_eq!(
        unrenewed["granted"], true,
        "the released lease holds nothing: {unrenewed}"
    );
    let unrenewed_id = unrenewed["lease"]["lease_id"].as_str().unwrap().to_string();
    backdate(&runtime, &unrenewed_id);
    let listed = lease::list(&runtime).expect("list leases");
    assert_eq!(listed["leases"], json!([]), "{listed}");
    let shown = lease::show(&runtime, json!({"lease_id": unrenewed_id})).expect("show lease");
    assert_eq!(shown["lease"]["state"], "expired", "{shown}");
    assert_eq!(shown["lease"]["release_reason"], "ttl_expired", "{shown}");

    let commands = lease_audit_commands(&runtime);
    for command in ["lease.renewed", "lease.released", "lease.expired"] {
        assert!(
            commands.iter().any(|c| c == command),
            "{command}: {commands:?}"
        );
    }
}

#[test]
fn check_reports_coverage_and_other_holders_without_changing_state() {
    if !super::isolate(
        module_path!(),
        "check_reports_coverage_and_other_holders_without_changing_state",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) = enabled_runtime();
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["file:src/lib.rs"],
    );
    let mine = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["dir:src"]}),
    );
    let mine_id = mine["lease"]["lease_id"].as_str().unwrap().to_string();
    let theirs = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["file:src/lib.rs"]}),
    );
    let theirs_id = theirs["lease"]["lease_id"].as_str().unwrap().to_string();
    let stale = request(
        &runtime,
        json!({"principal": "carol", "selectors": ["file:src/other.rs"], "mode": "hard"}),
    );
    let stale_id = stale["lease"]["lease_id"].as_str().unwrap().to_string();
    backdate(&runtime, &stale_id);
    let audit_before = lease_audit_commands(&runtime);

    let checked = lease::check(
        &runtime,
        json!({
            "lease_id": mine_id,
            "paths": ["file:src/other.rs", "file:src/lib.rs", "file:docs/guide.md"],
        }),
    )
    .expect("check paths");
    assert_eq!(checked["lease_active"], true, "{checked}");
    let paths = checked["paths"].as_array().unwrap();
    let entry = |path: &str| {
        paths
            .iter()
            .find(|entry| entry["path"] == path)
            .unwrap_or_else(|| panic!("{path} in {checked}"))
    };
    let other = entry("file:src/other.rs");
    assert_eq!(other["covered"], true, "{checked}");
    assert_eq!(holder_ids(&other["holders"]), [], "{checked}");
    let lib = entry("file:src/lib.rs");
    assert_eq!(lib["covered"], true, "{checked}");
    assert_eq!(
        holder_ids(&lib["holders"]),
        [
            ("task".to_string(), task.id.clone()),
            ("lease".to_string(), theirs_id.clone()),
        ],
        "{checked}"
    );
    let guide = entry("file:docs/guide.md");
    assert_eq!(guide["covered"], false, "{checked}");
    assert_eq!(guide["unlocked"], true, "{checked}");

    let narrow = lease::check(
        &runtime,
        json!({"lease_id": theirs_id, "paths": ["dir:src"]}),
    )
    .expect("check directory");
    assert_eq!(
        narrow["paths"][0]["covered"], false,
        "a file lease does not cover its directory: {narrow}"
    );

    assert_eq!(
        lease_audit_commands(&runtime),
        audit_before,
        "check writes no audit event"
    );
    let stale_row = runtime
        .stores()
        .task_reservations()
        .get_lease(
            &runtime.paths().orbit_dir.to_string_lossy(),
            Some(&runtime.workspace_id().unwrap()),
            &stale_id,
        )
        .unwrap()
        .expect("stale lease row");
    assert_eq!(
        stale_row.released_at, None,
        "check leaves a lapsed lease for lazy expiry to end"
    );
}

#[test]
fn disabled_leases_refuse_every_operation_naming_the_key() {
    if !super::isolate(
        module_path!(),
        "disabled_leases_refuse_every_operation_naming_the_key",
    ) {
        return;
    }
    let _env = caller_env(None);
    for config in ["", "[lease]\nenabled = false\n"] {
        let (_root, runtime, _repo_root) = lease_runtime(config);
        let lease_id = json!({"lease_id": "lease-1"});
        let selectors = json!({"principal": "alice", "selectors": ["file:src/lib.rs"]});
        let results = [
            lease::request(&runtime, selectors, None, None),
            lease::renew(&runtime, lease_id.clone()),
            lease::release(&runtime, lease_id.clone(), None, None),
            lease::show(&runtime, lease_id),
            lease::list(&runtime),
            lease::check(&runtime, json!({"paths": ["file:src/lib.rs"]})),
        ];
        for result in results {
            let message = invalid_input_message(result);
            assert!(message.contains("lease.enabled"), "{config:?}: {message}");
        }
    }
}

fn lease_id(answer: &Value) -> String {
    answer["lease"]["lease_id"].as_str().unwrap().to_string()
}

fn lease_notices(runtime: &OrbitRuntime, lease_id: &str) -> Vec<Value> {
    super::tool_support::run_tool_as_operator(
        runtime,
        "orbit.lease.notices",
        json!({"after": 0, "limit": 500, "lease_id": lease_id}),
    )
    .expect("read lease notices")["notices"]
        .as_array()
        .unwrap()
        .clone()
}

/// Write `lib/core.rs`, a path outside the default unlocked context paths
/// that the fixture's own files leave free.
fn write_locked_fixture(repo_root: &std::path::Path) {
    std::fs::create_dir_all(repo_root.join("lib")).unwrap();
    std::fs::write(repo_root.join("lib/core.rs"), "pub fn core() {}\n").unwrap();
}

/// Move a queued lease's `queued_at` back by `minutes`.
fn age_queue_entry(runtime: &OrbitRuntime, lease_id: &str, minutes: i64) {
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    let queued_at = chrono::Utc::now() - chrono::Duration::minutes(minutes);
    assert_eq!(
        connection
            .execute(
                "UPDATE task_reservations SET queued_at=?1 WHERE reservation_id=?2",
                rusqlite::params![queued_at.to_rfc3339(), lease_id],
            )
            .unwrap(),
        1
    );
}

#[test]
fn hard_requests_queue_by_priority_and_age_and_are_granted_when_blockers_end() {
    if !super::isolate(
        module_path!(),
        "hard_requests_queue_by_priority_and_age_and_are_granted_when_blockers_end",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) = enabled_runtime();
    write_locked_fixture(&repo_root);
    let holder = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/lib.rs"], "mode": "hard"}),
    );
    let holder_id = lease_id(&holder);

    let normal = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["dir:src"], "mode": "hard"}),
    );
    assert_eq!(normal["granted"], false, "{normal}");
    assert_eq!(normal["queued"], true, "{normal}");
    assert_eq!(normal["queue_position"], 1, "{normal}");
    assert_eq!(normal["lease"]["state"], "queued", "{normal}");
    assert_eq!(
        holder_ids(&normal["conflicts"]),
        vec![("lease".to_string(), holder_id.clone())],
        "{normal}"
    );
    let normal_id = lease_id(&normal);
    let queued = lease_notices(&runtime, &normal_id);
    assert!(
        queued.iter().any(|n| n["kind"] == "queued"
            && n["payload"]["position"] == 1
            && n["payload"]["holders"][0]["held_by_id"] == holder_id.as_str()),
        "the queued notice names the holders and the position: {queued:?}"
    );

    let high = request(
        &runtime,
        json!({"principal": "carol", "selectors": ["file:src/lib.rs"], "mode": "hard", "priority": "high"}),
    );
    assert_eq!(high["queued"], true, "{high}");
    assert_eq!(
        high["queue_position"], 1,
        "a higher priority request queues ahead of an older normal one: {high}"
    );
    let high_id = lease_id(&high);

    let refused = request(
        &runtime,
        json!({"principal": "dave", "selectors": ["file:src/lib.rs"], "mode": "hard", "wait": false}),
    );
    assert_eq!(refused["granted"], false, "{refused}");
    assert_eq!(refused["queued"], false, "{refused}");
    assert!(refused["lease"].is_null(), "{refused}");
    assert_eq!(
        holder_ids(&refused["conflicts"]),
        vec![("lease".to_string(), holder_id.clone())],
        "{refused}"
    );

    let reserved = super::tool_support::run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({"files": ["file:src/other.rs"], "model": "codex"}),
    )
    .unwrap();
    assert_eq!(
        reserved["reserved"], true,
        "a queued lease on dir:src holds nothing, so a reservation inside it is granted: {reserved}"
    );
    let listed = lease::list(&runtime).unwrap();
    let states = listed["leases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|lease| {
            (
                lease["lease_id"].as_str().unwrap().to_string(),
                lease["state"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        states.contains(&(normal_id.clone(), json!("queued"))),
        "{listed}"
    );

    super::tool_support::run_tool_as_operator(
        &runtime,
        "orbit.task.locks.release",
        json!({"reservation_id": reserved["reservation_id"]}),
    )
    .unwrap();
    age_queue_entry(&runtime, &normal_id, 31);
    lease::release(&runtime, json!({"lease_id": holder_id}), None, None).unwrap();
    let aged = lease::show(&runtime, json!({"lease_id": normal_id})).unwrap();
    assert_eq!(
        aged["lease"]["state"], "granted",
        "past lease.aging_minutes the older request moves up to high and, older, goes first: {aged}"
    );
    assert_eq!(aged["lease"]["selectors"], json!(["dir:src"]), "{aged}");
    assert!(
        lease_notices(&runtime, &normal_id)
            .iter()
            .any(|n| n["kind"] == "granted" && n["payload"]["from_queue"] == true)
    );
    let waiting = lease::show(&runtime, json!({"lease_id": high_id})).unwrap();
    assert_eq!(waiting["lease"]["state"], "queued", "{waiting}");
    assert!(
        !lease_notices(&runtime, &high_id)
            .iter()
            .any(|n| n["kind"] == "granted")
    );

    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["file:lib/core.rs"],
    );
    let behind_task = request(
        &runtime,
        json!({"principal": "erin", "selectors": ["file:lib/core.rs"], "mode": "hard"}),
    );
    assert_eq!(behind_task["queued"], true, "{behind_task}");
    assert_eq!(
        holder_ids(&behind_task["conflicts"]),
        vec![("task".to_string(), task.id.clone())],
        "{behind_task}"
    );
    let behind_id = lease_id(&behind_task);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    lease::sweep_lease_lifetimes(&runtime, deadline).unwrap();
    assert_eq!(
        lease::show(&runtime, json!({"lease_id": behind_id})).unwrap()["lease"]["state"],
        "queued",
        "a lease never preempts an in-progress task"
    );
    orbit_engine::RuntimeHost::apply_task_automation_update(
        &runtime,
        &task.id,
        orbit_engine::TaskAutomationUpdate {
            status: Some(TaskStatus::Done),
            ..orbit_engine::TaskAutomationUpdate::default()
        },
    )
    .expect("settle the task");
    lease::sweep_lease_lifetimes(&runtime, deadline).unwrap();
    let settled = lease::show(&runtime, json!({"lease_id": behind_id})).unwrap();
    assert_eq!(
        settled["lease"]["state"], "granted",
        "the clock grants the lease once the task settles: {settled}"
    );
    let commands = lease_audit_commands(&runtime);
    for command in ["lease.request.queued", "lease.request.granted"] {
        assert!(
            commands.iter().any(|c| c == command),
            "{command}: {commands:?}"
        );
    }
}

#[test]
fn expand_adds_free_paths_queues_blocked_ones_and_refuses_a_deadlock() {
    if !super::isolate(
        module_path!(),
        "expand_adds_free_paths_queues_blocked_ones_and_refuses_a_deadlock",
    ) {
        return;
    }
    let _env = caller_env(None);
    let (_root, runtime, repo_root) = enabled_runtime();
    write_locked_fixture(&repo_root);
    let first = request(
        &runtime,
        json!({"principal": "alice", "selectors": ["file:src/lib.rs"], "mode": "hard"}),
    );
    let first_id = lease_id(&first);
    let second = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["file:src/other.rs"], "mode": "hard"}),
    );
    let second_id = lease_id(&second);
    let expand = |lease_id: &str, selectors: Value| {
        lease::expand(
            &runtime,
            json!({"lease_id": lease_id, "selectors": selectors}),
            None,
            Some("codex".into()),
        )
        .expect("expand lease")
    };

    let widened = expand(&first_id, json!(["file:src/other.rs", "file:lib/core.rs"]));
    assert_eq!(widened["added"], json!(["file:lib/core.rs"]), "{widened}");
    assert_eq!(widened["queued"], json!(["file:src/other.rs"]), "{widened}");
    assert_eq!(widened["queue_position"], 1, "{widened}");
    assert!(widened["deadlock"].is_null(), "{widened}");
    assert_eq!(
        widened["lease"]["selectors"],
        json!(["file:lib/core.rs", "file:src/lib.rs"]),
        "the lease keeps what it holds and gains the free path: {widened}"
    );
    assert_eq!(
        holder_ids(&widened["conflicts"]),
        vec![("lease".to_string(), second_id.clone())],
        "{widened}"
    );

    let crossing = expand(&second_id, json!(["file:src/lib.rs"]));
    assert_eq!(
        crossing["deadlock"]["lease_id"],
        second_id.as_str(),
        "the younger expansion closing the cycle is refused: {crossing}"
    );
    assert_eq!(
        crossing["lease"]["queued_selectors"],
        json!([]),
        "{crossing}"
    );
    assert!(
        lease_notices(&runtime, &second_id)
            .iter()
            .any(|n| n["kind"] == "deadlock")
    );
    assert_eq!(
        lease::show(&runtime, json!({"lease_id": first_id})).unwrap()["lease"]["queued_selectors"],
        json!(["file:src/other.rs"]),
        "the older expansion keeps waiting"
    );

    lease::release(&runtime, json!({"lease_id": second_id}), None, None).unwrap();
    let granted = lease::show(&runtime, json!({"lease_id": first_id})).unwrap();
    assert_eq!(
        granted["lease"]["selectors"],
        json!(["file:lib/core.rs", "file:src/lib.rs", "file:src/other.rs"]),
        "the queued expansion is granted when its holder releases: {granted}"
    );
    assert!(
        lease_notices(&runtime, &first_id)
            .iter()
            .any(|n| n["kind"] == "granted" && n["payload"]["expansion"] == true)
    );
    let commands = lease_audit_commands(&runtime);
    for command in [
        "lease.expanded",
        "lease.expand.deadlock",
        "lease.expand.granted",
    ] {
        assert!(
            commands.iter().any(|c| c == command),
            "{command}: {commands:?}"
        );
    }
}
