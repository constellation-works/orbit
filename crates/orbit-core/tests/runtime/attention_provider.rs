//! `ci_red` and `friction_untriaged` at the public `orbit.attention` boundary.
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [McpCapability::Operator].into(),
        ..Default::default()
    }
}

fn call(runtime: &OrbitRuntime, input: Value) -> Value {
    call_with(runtime, input, session())
}

fn aggregator_session() -> ToolSessionContext {
    let mut session = session();
    session.mcp_client_name = Some("orbit-federated-mux".into());
    session
}

fn call_with(runtime: &OrbitRuntime, input: Value, session: ToolSessionContext) -> Value {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session,
        )
        .map(|outcome| outcome.value)
        .unwrap_or_else(|error| panic!("attention call failed: {error}"))
}

fn add_open_friction(runtime: &OrbitRuntime) -> String {
    use orbit_store::contracts::FrictionAddParams;
    let store = orbit_core::runtime::friction::store_for(runtime).unwrap();
    store
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Open friction".into()),
            body: "Open friction body".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Open,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap()
        .record
        .id
}

fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    _root: tempfile::TempDir,
    runtime: OrbitRuntime,
    repo: PathBuf,
    head: String,
    state: PathBuf,
}

impl Fixture {
    fn new(auth_ok: bool) -> Self {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let state = root.path().join("gh-state");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(repo.join(".orbit/config.toml"), "").unwrap();
        std::fs::write(repo.join("file.txt"), "hello\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.test"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "initial"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/orbit.git"],
        );
        let head = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        // The fetched remote-tracking ref starts at the local head; a
        // scenario advances it to model a fetched base moving ahead.
        git(&repo, &["update-ref", "refs/remotes/origin/main", &head]);
        // Fake `gh` first on PATH for this isolated scenario process.
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let auth_exit = if auth_ok { 0 } else { 1 };
        let script = format!(
            "#!/bin/sh\nstate=${{ORBIT_FAKE_GH_STATE:?}}\nprintf '%s\\n' \"$*\" >> \"$state/calls\"\ncase \"$1 ${{2:-}}\" in\n  \"auth status\") exit {auth_exit} ;;\n  \"run list\") cat \"$state/runs.json\" ;;\n  *) echo \"fake gh: unsupported: $*\" >&2; exit 2 ;;\nesac\n"
        );
        let gh = bin.join("gh");
        std::fs::write(&gh, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        // SAFETY: scenario tables run in an isolated child process.
        unsafe {
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
            std::env::set_var("ORBIT_FAKE_GH_STATE", &state);
        }
        std::fs::write(state.join("calls"), "").unwrap();
        std::fs::write(state.join("runs.json"), "[]").unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        Self {
            _root: root,
            runtime,
            repo,
            head,
            state,
        }
    }

    /// Advance only the remote-tracking base ref to a new commit on top
    /// of the current head, leaving the local base branch behind.
    fn advance_remote_head(&self) -> String {
        let output = std::process::Command::new("git")
            .args([
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                "HEAD",
                "-m",
                "advance remote base",
            ])
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git commit-tree: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let advanced = String::from_utf8(output.stdout).unwrap().trim().to_string();
        git(
            &self.repo,
            &["update-ref", "refs/remotes/origin/main", &advanced],
        );
        advanced
    }

    fn delete_remote_head(&self) {
        git(
            &self.repo,
            &["update-ref", "-d", "refs/remotes/origin/main"],
        );
    }

    fn set_runs(&self, runs: Value) {
        std::fs::write(
            self.state.join("runs.json"),
            serde_json::to_string(&runs).unwrap(),
        )
        .unwrap();
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.state.join("calls")).unwrap_or_default()
    }

    fn run(&self, id: u64, conclusion: &str, updated: &str, sha: &str) -> Value {
        json!({
            "databaseId": id, "number": id, "workflowName": "CI",
            "displayTitle": "CI failed", "status": "completed",
            "conclusion": conclusion, "event": "push", "headBranch": "main",
            "headSha": sha, "createdAt": updated, "startedAt": updated,
            "updatedAt": updated, "url": "https://example.test/run"
        })
    }
}

fn journal_state(runtime: &OrbitRuntime, journal_id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection
        .query_row(
            "SELECT state FROM task_commit_journal WHERE journal_id = ?1",
            [journal_id],
            |row| row.get(0),
        )
        .ok()
}

fn ci_red_pending_commit_skips_repair_lookup_and_preserves_journal() {
    let fixture = Fixture::new(true);
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    let task = fixture
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Repair CI".into(),
            description: "Repair.".into(),
            acceptance_criteria: vec!["CI green.".into()],
            status: Some(orbit_types::task::TaskStatus::Proposed),
            creation_decision: Some(orbit_types::task::TaskDecision::CiFailureIdentity(
                orbit_types::task::CiFailureIdentity {
                    failure_key: "key".into(),
                    compiler_cause_digest: None,
                    open_compiler_set: None,
                    signature: None,
                    tested_commit: fixture.head.clone(),
                    sources: vec![orbit_types::task::CiFailureSource {
                        run_id: "42".into(),
                        job_id: "1".into(),
                        checkout: fixture.head.clone(),
                    }],
                },
            )),
            ..Default::default()
        })
        .unwrap();
    let workspace_id = fixture.runtime.workspace_id().unwrap();
    {
        let connection =
            rusqlite::Connection::open(fixture.runtime.global_root().join("orbit.db")).unwrap();
        connection
            .execute(
                "INSERT INTO task_commit_journal \
                     (journal_id, workspace_id, task_id, state, intent_json, created_at) \
                 VALUES (?1, ?2, ?3, 'prepared', '{}', ?4)",
                rusqlite::params![
                    "attention-ci-pending-1",
                    workspace_id,
                    task.id,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }
    assert_eq!(
        journal_state(&fixture.runtime, "attention-ci-pending-1").as_deref(),
        Some("prepared")
    );
    let before = fixture.runtime.observe_task_metadata().unwrap();
    assert!(fixture.runtime.inspect_execution_claims().is_err());

    // Fresh and cached provider reads alike: the CI evidence is reported,
    // the repair-task association degrades explicitly, and neither the
    // journal nor task/claim state is settled by the query.
    for attempt in ["fresh", "cached"] {
        let calls_before = fixture.calls();
        let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
        let items = response["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{attempt}: {response}");
        assert_eq!(items[0]["refs"]["run_id"], "42", "{attempt}: {response}");
        assert!(
            items[0]["refs"].get("task_id").is_none(),
            "{attempt}: pending lookup must not link a repair task: {response}"
        );
        assert_eq!(
            items[0]["next"]["tool"], "github.run.view",
            "{attempt}: {response}"
        );
        assert!(
            response["degraded"].as_array().unwrap().iter().any(|d| {
                d["kind"] == "ci_red"
                    && d["code"] == "source_unavailable"
                    && d["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("pending commit"))
            }),
            "{attempt}: repair lookup must degrade explicitly: {response}"
        );
        assert_eq!(
            journal_state(&fixture.runtime, "attention-ci-pending-1").as_deref(),
            Some("prepared"),
            "{attempt}: attention must not abort or replay the pending commit"
        );
        assert_eq!(fixture.runtime.observe_task_metadata().unwrap(), before);
        assert!(
            fixture.runtime.inspect_execution_claims().is_err(),
            "{attempt}: attention left the pending commit for an ordinary read"
        );
        if attempt == "cached" {
            assert_eq!(
                fixture.calls(),
                calls_before,
                "cached provider read must not call the provider again"
            );
        }
    }

    // Ordinary operational history reads retain recovery semantics, and
    // once settled the repair-task link works again.
    fixture.runtime.get_task_history(&task.id).unwrap();
    assert_eq!(
        journal_state(&fixture.runtime, "attention-ci-pending-1").as_deref(),
        Some("aborted"),
        "an ordinary history read aborts the prepared journal row"
    );
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["refs"]["task_id"], task.id, "{response}");
    assert_eq!(items[0]["next"]["tool"], "orbit.task.show", "{response}");
}

fn ci_red_lists_newest_failed_run_on_head() {
    let fixture = Fixture::new(true);
    let old = (chrono::Utc::now() - chrono::Duration::hours(100)).to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &old, &fixture.head)]));
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["kind"], "ci_red");
    assert_eq!(
        items[0]["severity"], "critical",
        "both age thresholds passed: {response}"
    );
    assert_eq!(items[0]["refs"]["run_id"], "42");
    assert_eq!(items[0]["next"]["tool"], "github.run.view");
    assert!(items[0]["fingerprint"].as_str().unwrap().starts_with("fp_"));
}

fn ci_red_exclude_kind_makes_no_provider_call() {
    let fixture = Fixture::new(true);
    fixture.set_runs(json!([fixture.run(
        42,
        "failure",
        &chrono::Utc::now().to_rfc3339(),
        &fixture.head
    )]));
    let response = call(
        &fixture.runtime,
        json!({"kind": ["pilot_warning"], "exclude_kind": ["ci_red"]}),
    );
    assert!(
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "ci_red")
    );
    assert_eq!(
        fixture.calls(),
        "",
        "--exclude-kind ci_red must skip the provider read"
    );
    // Explicit ci_red does call the provider.
    let _ = call(&fixture.runtime, json!({"kind": "ci_red"}));
    assert!(
        fixture.calls().contains("run list"),
        "calls: {}",
        fixture.calls()
    );
}

fn ci_red_missing_auth_degrades_without_failing() {
    let fixture = Fixture::new(false);
    fixture.set_runs(json!([fixture.run(
        42,
        "failure",
        &chrono::Utc::now().to_rfc3339(),
        &fixture.head
    )]));
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    assert!(
        response["items"].as_array().unwrap().is_empty(),
        "{response}"
    );
    let degraded = response["degraded"].as_array().unwrap();
    assert!(
        degraded
            .iter()
            .any(|d| d["kind"] == "ci_red" && d["code"] == "source_unavailable"),
        "{response}"
    );
}

fn ci_red_ignores_a_failed_run_on_a_stale_head() {
    let fixture = Fixture::new(true);
    let now = chrono::Utc::now().to_rfc3339();
    // Failure on a different (older) head is not current.
    fixture.set_runs(json!([fixture.run(
        42,
        "failure",
        &now,
        "0".repeat(40).as_str()
    )]));
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    assert!(
        response["items"].as_array().unwrap().is_empty(),
        "stale head: {response}"
    );
}

fn ci_red_folds_into_existing_repair_task() {
    let fixture = Fixture::new(true);
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    let task = fixture
        .runtime
        .add_task(orbit_core::application::task::TaskAddParams {
            title: "Repair CI".into(),
            description: "Repair.".into(),
            acceptance_criteria: vec!["CI green.".into()],
            status: Some(orbit_types::task::TaskStatus::Proposed),
            creation_decision: Some(orbit_types::task::TaskDecision::CiFailureIdentity(
                orbit_types::task::CiFailureIdentity {
                    failure_key: "key".into(),
                    compiler_cause_digest: None,
                    open_compiler_set: None,
                    signature: None,
                    tested_commit: fixture.head.clone(),
                    sources: vec![orbit_types::task::CiFailureSource {
                        run_id: "42".into(),
                        job_id: "1".into(),
                        checkout: fixture.head.clone(),
                    }],
                },
            )),
            ..Default::default()
        })
        .unwrap();
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["refs"]["task_id"], task.id);
    assert_eq!(items[0]["next"]["tool"], "orbit.task.show");
    assert_eq!(items[0]["severity"], "high");
}

fn friction_untriaged_lists_only_open() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), "").unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    let store = orbit_core::runtime::friction::store_for(&runtime).unwrap();
    use orbit_store::contracts::FrictionAddParams;
    let open = store
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Open friction".into()),
            body: "Open friction body".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Open,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    store
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Triaged friction".into()),
            body: "Triaged body".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Triaged,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    let response = call(&runtime, json!({"kind": "friction_untriaged"}));
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["refs"]["friction_id"], open.record.id);
    assert_eq!(items[0]["severity"], "low");
    assert_eq!(items[0]["next"]["tool"], "orbit.friction.update");
    assert_eq!(items[0]["next"]["input"]["status"], "triaged");
}

fn replica_owner_only_candidate_reads_are_silent_and_make_no_provider_call() {
    let fixture = Fixture::new(true);
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    let friction_id = add_open_friction(&fixture.runtime);
    let replica = fixture
        .runtime
        .clone()
        .with_coordination_write_owner(Some("hm_owner".into()));
    let candidate = call_with(
        &replica,
        json!({"kind": ["ci_red", "friction_untriaged"], "candidate_read": true}),
        aggregator_session(),
    );
    assert_eq!(candidate["items"], json!([]), "{candidate}");
    assert_eq!(candidate["degraded"], json!([]), "{candidate}");
    assert_eq!(
        fixture.calls(),
        "",
        "replica candidate read must not call the provider: {}",
        fixture.calls()
    );
    // The owner still reports both sources.
    let owner = call(
        &fixture.runtime,
        json!({"kind": ["ci_red", "friction_untriaged"]}),
    );
    let items = owner["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{owner}");
    assert!(
        items
            .iter()
            .any(|item| item["kind"] == "ci_red" && item["refs"]["run_id"] == "42"),
        "{owner}"
    );
    assert!(
        items.iter().any(|item| item["kind"] == "friction_untriaged"
            && item["refs"]["friction_id"] == friction_id),
        "{owner}"
    );
    assert!(fixture.calls().contains("run list"), "{}", fixture.calls());
}

fn replica_direct_reads_identify_owner_only_sources_without_provider_call() {
    let fixture = Fixture::new(true);
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    add_open_friction(&fixture.runtime);
    let replica = fixture
        .runtime
        .clone()
        .with_coordination_write_owner(Some("hm_owner".into()));
    let direct = call(&replica, json!({"kind": ["ci_red", "friction_untriaged"]}));
    assert_eq!(direct["items"], json!([]), "{direct}");
    for kind in ["ci_red", "friction_untriaged"] {
        let entry = direct["degraded"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["kind"] == kind)
            .unwrap_or_else(|| panic!("missing degraded for {kind}: {direct}"));
        assert_eq!(entry["code"], "source_unavailable", "{direct}");
        assert!(
            entry["message"]
                .as_str()
                .unwrap()
                .contains("read the workspace owner"),
            "{direct}"
        );
    }
    assert_eq!(
        fixture.calls(),
        "",
        "direct replica read must not call the provider: {}",
        fixture.calls()
    );
}

fn ci_red_reports_remote_head_when_local_behind() {
    let fixture = Fixture::new(true);
    let remote_head = fixture.advance_remote_head();
    assert_ne!(remote_head, fixture.head);
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(43, "failure", &now, &remote_head)]));
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["refs"]["run_id"], "43");
    assert_eq!(items[0]["detail"]["reported_head_sha"], remote_head);
}

fn ci_red_ignores_retained_failure_on_stale_local_head() {
    let fixture = Fixture::new(true);
    fixture.advance_remote_head();
    let now = chrono::Utc::now().to_rfc3339();
    // Retained failure on the stale local head is not current.
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    assert!(
        response["items"].as_array().unwrap().is_empty(),
        "stale local head must not be reported as current: {response}"
    );
}

fn ci_red_advancing_remote_head_invalidates_cache() {
    let fixture = Fixture::new(true);
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    let first = call(&fixture.runtime, json!({"kind": "ci_red"}));
    assert_eq!(first["items"].as_array().unwrap().len(), 1, "{first}");
    let remote_head = fixture.advance_remote_head();
    fixture.set_runs(json!([fixture.run(43, "failure", &now, &remote_head)]));
    let second = call(&fixture.runtime, json!({"kind": "ci_red"}));
    let items = second["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{second}");
    assert_eq!(items[0]["refs"]["run_id"], "43", "{second}");
}

fn ci_red_unavailable_base_head_degrades() {
    let fixture = Fixture::new(true);
    fixture.delete_remote_head();
    let now = chrono::Utc::now().to_rfc3339();
    fixture.set_runs(json!([fixture.run(42, "failure", &now, &fixture.head)]));
    let response = call(&fixture.runtime, json!({"kind": "ci_red"}));
    assert!(
        response["items"].as_array().unwrap().is_empty(),
        "unavailable head must not be treated as a known current head: {response}"
    );
    let degraded = response["degraded"].as_array().unwrap();
    assert!(
        degraded
            .iter()
            .any(|d| d["kind"] == "ci_red" && d["code"] == "source_unavailable"),
        "{response}"
    );
}

crate::scenario_table!(
    attention_provider,
    replica_owner_only_candidate_reads_are_silent_and_make_no_provider_call,
    replica_direct_reads_identify_owner_only_sources_without_provider_call,
    ci_red_lists_newest_failed_run_on_head,
    ci_red_exclude_kind_makes_no_provider_call,
    ci_red_missing_auth_degrades_without_failing,
    ci_red_ignores_a_failed_run_on_a_stale_head,
    ci_red_folds_into_existing_repair_task,
    ci_red_pending_commit_skips_repair_lookup_and_preserves_journal,
    ci_red_reports_remote_head_when_local_behind,
    ci_red_ignores_retained_failure_on_stale_local_head,
    ci_red_advancing_remote_head_invalidates_cache,
    ci_red_unavailable_base_head_degrades,
    friction_untriaged_lists_only_open,
);
