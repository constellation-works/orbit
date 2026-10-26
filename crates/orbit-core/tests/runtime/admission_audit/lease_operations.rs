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
        json!({"principal": "alice", "selectors": ["file:src/lib.rs"], "mode": "hard"}),
    );
    assert_eq!(refused["granted"], false, "{refused}");
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
        json!({"principal": "bob", "selectors": ["dir:src"], "mode": "hard"}),
    );
    assert_eq!(second["granted"], false, "{second}");
    assert!(
        holder_ids(&second["conflicts"]).contains(&("task".to_string(), task.id.clone())),
        "{second}"
    );
    let overlapping = request(
        &runtime,
        json!({"principal": "bob", "selectors": ["file:src/other.rs"], "mode": "hard"}),
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
