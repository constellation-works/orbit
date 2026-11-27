//! Captured CI log shapes through the deterministic task-filing boundary.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]
#[path = "ci_failure_goldens/archive.rs"]
mod archive;
#[path = "ci_failure_goldens/claimed_repair_owners.rs"]
mod claimed_repair_owners;
#[path = "support/git.rs"]
mod git_test_support;
orbit_common::isolate_test_process!();
#[cfg(unix)]
#[path = "ci_failure_goldens/sweep.rs"]
mod sweep;
#[cfg(unix)]
#[path = "ci_failure_goldens/sweep_rate_limit.rs"]
mod sweep_rate_limit;

use chrono::Duration;
use orbit_common::security::release::sha256_hex;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::bootstrap::task_migration::seed_task_id_start;
use orbit_core::runtime::OrbitRuntime;
use orbit_engine::Dispatch;
use orbit_engine::DispatchError;
use orbit_engine::RunLifecycle;
use orbit_engine::TaskAutomationUpdate;
use orbit_store::compose::workspace_coordinated_backends;
use orbit_store::contract::{
    AdmissionRunContext, BranchObservationOutcome, ClaimEvidence, ClaimInvocation, ClaimMutation,
    DEFERRED_BRANCH_OBSERVATION_KIND, DeferredBranchObservation, ExecutionClaim,
    ExecutionClaimPhase, ExecutionLocation,
};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_tools::ToolContext;
use orbit_types::task::{
    CI_FAILURE_KEY_TAG_PREFIX, CiFailureIdentity, CiFailureSource, Task, TaskArtifact, TaskComment,
    TaskComplexity, TaskDecision, TaskHistoryEntry, TaskRelation, TaskRelationType, TaskStatus,
    task_id_prefix,
};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const AGG_JOB: &str = "Check / Clippy / Test / Await the test build and nextest shards";
const SIB_JOB: &str = "Test partition 2";
const OUTER_JOB: &str = "Check / Clippy / Test / Await the outer aggregate";
const MID_JOB: &str = "Check / Clippy / Test / Await the middle aggregate";
const LEAF_JOB: &str = "Test partition 2";
const ENTRY_WIN: &str = r"crates\orbit-core\src\application\distributed\entry.rs";
const ENTRY_NIX: &str = "crates/orbit-core/src/application/distributed/entry.rs";
const SANDBOX_MSG: &str = "cannot find `git_sandbox` in `runtime`";

fn orbit_dirs() -> (TempDir, PathBuf, PathBuf) {
    let root = TempDir::new().unwrap();
    let (g, ws) = (
        root.path().join("home/.orbit"),
        root.path().join("repo/.orbit"),
    );
    std::fs::create_dir_all(&g).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    (root, g, ws)
}
fn bare_runtime() -> (TempDir, OrbitRuntime) {
    let (root, g, ws) = orbit_dirs();
    (root, OrbitRuntime::from_roots(&g, &ws).unwrap())
}
fn configured_runtime(cfg: &str) -> (TempDir, OrbitRuntime) {
    let (root, g, ws) = orbit_dirs();
    std::fs::write(ws.join("config.toml"), cfg).unwrap();
    (root, OrbitRuntime::from_roots(&g, &ws).unwrap())
}
/// One migrated `orbit.db`, captured after its connection closed.
fn migrated_store_template() -> &'static (Vec<u8>, std::fs::Permissions) {
    use std::sync::OnceLock;
    static T: OnceLock<(Vec<u8>, std::fs::Permissions)> = OnceLock::new();
    T.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("orbit.db");
        drop(orbit_store::Store::open(&db).unwrap());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(names.len() == 1 && names[0] == "orbit.db", "{names:?}");
        let perms = std::fs::metadata(&db).unwrap().permissions();
        (std::fs::read(&db).unwrap(), perms)
    })
}
fn seed_migrated_store(global: &Path) {
    let db = global.join("orbit.db");
    if !db.exists() {
        let (bytes, perms) = migrated_store_template();
        std::fs::write(&db, bytes).unwrap();
        std::fs::set_permissions(&db, perms.clone()).unwrap();
    }
}
fn seeded_golden_runtime() -> (TempDir, OrbitRuntime) {
    let (root, g, ws) = orbit_dirs();
    seed_migrated_store(&g);
    (root, OrbitRuntime::from_roots(&g, &ws).unwrap())
}
fn operator_commits(config: &str, count: usize) -> (TempDir, OrbitRuntime, Vec<String>) {
    let root = TempDir::new().unwrap();
    let (g, repo) = (root.path().join("home/.orbit"), root.path().join("repo"));
    let ws = repo.join(".orbit");
    std::fs::create_dir_all(&g).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("config.toml"), config).unwrap();
    let commits = commit_chain(&repo, count);
    (root, OrbitRuntime::from_roots(&g, &ws).unwrap(), commits)
}
fn operator_fixture(config: &str) -> (TempDir, OrbitRuntime, Vec<String>) {
    operator_commits(config, 3)
}
fn fixture_owner(log: &str) -> (TempDir, OrbitRuntime, Vec<String>, Value, String) {
    let (root, rt, c) = operator_fixture("");
    let first = file_one(&rt, log, 0, &c[0]);
    let owner = filed_id(&first, 0).to_string();
    (root, rt, c, first, owner)
}
fn sha(ch: char) -> String {
    ch.to_string().repeat(40)
}
fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().unwrap()
}
fn arr_len(v: &Value, key: &str) -> usize {
    arr(v, key).len()
}
fn filed_id(out: &Value, idx: usize) -> &str {
    out["filed"][idx]["task_id"].as_str().unwrap()
}
fn filed_key(out: &Value, idx: usize) -> &str {
    out["filed"][idx]["failure_key"].as_str().unwrap()
}
fn get_task(rt: &OrbitRuntime, id: &str) -> Task {
    rt.get_task(id).unwrap()
}
fn task_desc(rt: &OrbitRuntime, id: &str) -> String {
    get_task(rt, id).description
}
fn task_status(rt: &OrbitRuntime, id: &str) -> TaskStatus {
    get_task(rt, id).status
}
fn comments(rt: &OrbitRuntime, id: &str) -> Vec<TaskComment> {
    rt.get_task_comments(id).unwrap()
}
fn artifacts(rt: &OrbitRuntime, id: &str) -> Vec<TaskArtifact> {
    rt.get_task_artifacts(id).unwrap()
}
fn arts_len(rt: &OrbitRuntime, id: &str) -> usize {
    artifacts(rt, id).len()
}
fn cmts_len(rt: &OrbitRuntime, id: &str) -> usize {
    comments(rt, id).len()
}
fn history(rt: &OrbitRuntime, id: &str) -> Vec<TaskHistoryEntry> {
    rt.get_task_history(id).unwrap()
}
fn art_json(a: &TaskArtifact) -> Value {
    serde_json::from_slice(&a.content).unwrap()
}
fn compiler_log(code: &str, msg: &str, path: &str, line: usize, col: usize) -> String {
    format!("error[{code}]: {msg}\n   --> {path}:{line}:{col}\n")
}
fn sandbox_at(line: usize, col: usize) -> String {
    compiler_log("E0433", SANDBOX_MSG, ENTRY_NIX, line, col)
}
fn e0425_foo(line: usize) -> String {
    compiler_log(
        "E0425",
        "cannot find value `foo` in this scope",
        ENTRY_NIX,
        line,
        10,
    )
}
fn sandbox_win_pair() -> (String, String) {
    (
        compiler_log("E0433", SANDBOX_MSG, ENTRY_WIN, 277, 25),
        compiler_log("E0433", SANDBOX_MSG, ENTRY_WIN, 280, 28),
    )
}
fn set_status(rt: &OrbitRuntime, id: &str, status: TaskStatus) {
    let u = TaskAutomationUpdate {
        status: Some(status),
        ..Default::default()
    };
    orbit_core::test_support::EngineHost::new(rt)
        .apply_task_automation_update(id, u)
        .unwrap();
}
fn add_task(
    rt: &OrbitRuntime,
    title: &str,
    desc: &str,
    plan: &str,
    tags: Vec<String>,
    sys: bool,
) -> Task {
    let p = TaskAddParams {
        title: title.into(),
        description: desc.into(),
        plan: plan.into(),
        tags,
        system_created: sys,
        ..Default::default()
    };
    rt.add_task(p).unwrap()
}
fn add_titled(rt: &OrbitRuntime, title: &str) -> Task {
    add_task(rt, title, "", "", vec![], false)
}
fn add_planned(rt: &OrbitRuntime, title: &str) -> Task {
    add_task(
        rt,
        title,
        "",
        "Hold the branch while the follower runs.",
        vec![],
        false,
    )
}
fn add_sweep_task(rt: &OrbitRuntime, title: &str, desc: &str, key: &str) -> Task {
    let tags = vec![
        "ci-failure-sweep".into(),
        format!("{CI_FAILURE_KEY_TAG_PREFIX}{key}"),
    ];
    add_task(rt, title, desc, "", tags, true)
}
fn patch_task(rt: &OrbitRuntime, id: &str, f: impl FnOnce(&mut TaskUpdateParams)) {
    let mut p = TaskUpdateParams::default();
    f(&mut p);
    rt.update_task_as_human(id, p, "human:fixture".into())
        .unwrap();
}
fn set_desc(rt: &OrbitRuntime, id: &str, d: &str) {
    patch_task(rt, id, |p| p.description = Some(d.into()));
}
fn with_job(mut run: Value, job: &str) -> Value {
    run["failed_jobs"][0]["name"] = json!(job);
    run
}
fn failure(log: &str, index: usize, checkout: &str) -> Value {
    json!({ "run_id": 10 + index, "job_id": 910 + index, "log_job_id": 910 + index, "checkout_identity": {"state": "observed", "provenance": {"job_id": 910 + index, "complete": true}}, "workflow": "CI", "status": "completed", "conclusion": "failure", "event": "push", "url": format!("https://github.com/acme/orbit/actions/runs/{}", 10 + index), "created_at": "2026-09-07T07:24:42Z", "head_branch": "agent-main", "ref_kind": "integration", "event_reported_head_sha": checkout, "current_ref_head_sha": sha('1'), "actual_checkout_shas": [checkout], "checkout_evidence": [format!("HEAD is now at {checkout}")], "checkout_evidence_scope": "all", "investigated": true, "log_excerpt": log, "log_truncated": false, "failed_jobs": [{"job_id": 910 + index, "name": "build", "conclusion": "failure", "failed_steps": [{"name": "Run CI", "conclusion": "failure"}]}] })
}
fn pull_request(log: &str, index: usize, branch: &str, checkout: &str) -> Value {
    let mut run = failure(log, index, checkout);
    run["event"] = json!("pull_request");
    run["head_branch"] = json!(branch);
    run["ref_kind"] = json!("pull_request");
    run
}
fn pr_for(owner: &str, log: &str, idx: usize, ch: char) -> Value {
    pull_request(log, idx, &format!("orbit/{owner}-ddb04571"), &sha(ch))
}
fn named_failure(log: &str, idx: usize, sha_s: &str, rid: u64, job: &str, step: &str) -> Value {
    let mut run = with_job(failure(log, idx, sha_s), job);
    run["run_id"] = json!(rid);
    run["url"] = json!(format!("https://github.com/acme/orbit/actions/runs/{rid}"));
    run["failed_jobs"][0]["failed_steps"][0]["name"] = json!(step);
    run
}
fn evidence_input(runs: Vec<Value>, extra: Value) -> Value {
    let mut ev = json!({ "schema_version": 2, "collected": true, "outcome_hint": "current_failures", "capability": {"available": true, "authenticated": true}, "repository": {"name": "orbit", "full_name": "acme/orbit", "default_branch": "main"}, "heads": [{"kind": "integration", "branch": "agent-main", "current_head_sha": sha('1')}], "latest_runs": runs.clone(), "current_failures": runs, "stale_or_superseded": [], "in_flight": [], "retryable_errors": [], "collected_at": "2026-09-07T08:00:00Z" });
    if let Some(map) = extra.as_object() {
        for (k, v) in map {
            ev[k] = v.clone();
        }
    }
    json!({ "ci_evidence": ev })
}
fn file_input(rt: &OrbitRuntime, input: Value) -> Result<Value, DispatchError> {
    orbit_core::test_support::EngineHost::new(rt).run_deterministic(
        "file_ci_failure_tasks",
        &json!({}),
        &input,
        ToolContext::default(),
    )
}
fn file_result(rt: &OrbitRuntime, runs: Vec<Value>) -> Result<Value, DispatchError> {
    file_input(rt, evidence_input(runs, json!({})))
}
fn file(rt: &OrbitRuntime, runs: Vec<Value>) -> Value {
    file_result(rt, runs).expect("file CI")
}
fn file_one(rt: &OrbitRuntime, log: &str, idx: usize, c: &str) -> Value {
    file(rt, vec![failure(log, idx, c)])
}
fn file_err(rt: &OrbitRuntime, run: Value) -> DispatchError {
    file_result(rt, vec![run]).unwrap_err()
}
fn branch_file(rt: &OrbitRuntime, pr: Value) -> Value {
    file_input(rt, evidence_input(vec![], json!({"branch_failures": [pr]}))).unwrap()
}
fn retryable_error_payload(err: DispatchError) -> Value {
    let msg = err.to_string();
    let (_, p) = msg
        .split_once("ci_failure_sweep retryable: ")
        .unwrap_or_else(|| panic!("{msg}"));
    serde_json::from_str(p).unwrap()
}
fn release_claim(rt: &OrbitRuntime, owner: &str, cid: &str, mach: &str, key: &str, sum: &str) {
    let inv = ClaimInvocation::trusted_worker(owner.into(), cid.into(), mach.into(), None);
    let ev = ClaimEvidence {
        summary: Some(sum.into()),
        ..Default::default()
    };
    rt.mutate_execution_claim(Some(&inv), key, &ClaimMutation::Release(ev))
        .unwrap();
}
fn release(rt: &OrbitRuntime, claim: &ExecutionClaim, key: &str) {
    release_claim(
        rt,
        &claim.task_id,
        &claim.claim_id,
        &claim.executed_on.machine_id,
        key,
        "follower finished",
    );
}
fn open_db(rt: &OrbitRuntime) -> rusqlite::Connection {
    rusqlite::Connection::open(rt.global_root().join("orbit.db")).unwrap()
}
fn insert_coordination_row(rt: &OrbitRuntime, kind: &str, row_id: &str, payload: &str) {
    let ws = rt.workspace_id().unwrap();
    let sql = "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at) VALUES (?1, ?2, ?3, ?4, 'fixture', ?5)";
    let p = rusqlite::params![ws, kind, row_id, payload, chrono::Utc::now().to_rfc3339()];
    open_db(rt).execute(sql, p).unwrap();
}
fn deferred_rows(rt: &OrbitRuntime) -> Vec<DeferredBranchObservation> {
    let (ws, db) = (rt.workspace_id().unwrap(), open_db(rt));
    let sql = "SELECT payload_json FROM task_coordination_rows WHERE workspace_id=?1 AND kind=?2 ORDER BY row_id";
    let mut stmt = db.prepare(sql).unwrap();
    let args = rusqlite::params![ws, DEFERRED_BRANCH_OBSERVATION_KIND];
    let rows = stmt.query_map(args, |r| r.get::<_, String>(0)).unwrap();
    rows.map(|p| serde_json::from_str(&p.unwrap()).unwrap())
        .collect()
}
fn description_only_owner(rt: &OrbitRuntime, owner: &str) -> String {
    let t = get_task(rt, owner);
    let keep: Vec<_> = t
        .description
        .lines()
        .filter(|l| !l.starts_with("- Open compiler diagnostic set identity:"))
        .collect();
    let legacy = add_task(rt, &t.title, &keep.join("\n"), &t.plan, t.tags, true);
    set_status(rt, owner, TaskStatus::Done);
    let re = OrbitRuntime::from_roots(&rt.global_root(), &rt.shared_root()).unwrap();
    assert_eq!(task_desc(&re, &legacy.id), legacy.description);
    // Creation records its crew assignment [ORB-24023]; the legacy shape has no CI-failure decision.
    let no_ci =
        |e: &TaskHistoryEntry| matches!(e.decision, None | Some(TaskDecision::CrewAssignment(_)));
    assert!(history(&re, &legacy.id).iter().all(no_ci));
    legacy.id
}
fn archive_owner(rt: &OrbitRuntime, id: &str, status: TaskStatus, cover: Option<&str>) {
    let rels = cover.map(|t| {
        vec![TaskRelation {
            relation_type: TaskRelationType::CoveredBy,
            target: t.into(),
            regression: None,
        }]
    });
    patch_task(rt, id, |p| {
        p.status = Some(status);
        p.relations = rels;
    });
}
fn assert_str(v: &Value, k: &str, exp: &str) {
    assert_eq!(v[k], exp, "{v}");
}
fn assert_len(v: &Value, k: &str, n: usize) {
    assert_eq!(arr_len(v, k), n, "{v}");
}
fn assert_filed(out: &Value, n: usize, ctx: &str) {
    assert_eq!(out["filed_count"], n, "{ctx}: {out}");
}
fn assert_empty(out: &Value, key: &str, ctx: &str) {
    assert_eq!(out[key], json!([]), "{ctx}: {out}");
}
fn assert_skipped(out: &Value, id: &str, ctx: &str) {
    assert_eq!(out["skipped_existing"][0]["task_id"], id, "{ctx}: {out}");
}
fn assert_covered(out: &Value, reason: &str, owner: &str, cover: &str, ctx: &str) {
    let w = &out["withheld"][0];
    assert_eq!(w["outcome"], "covered", "{ctx}: {out}");
    assert_eq!(w["reason"], reason, "{ctx}: {out}");
    assert_eq!(w["owner"], owner, "{ctx}: {out}");
    assert_eq!(w["cover"], cover, "{ctx}: {out}");
}
fn assert_pending(out: &Value, repair: &str, landed: &str, tested: &str, ctx: &str) {
    let p = &out["pending_supersession"][0];
    assert_eq!(
        p["reason"], "repaired_by_descendant_landing",
        "{ctx}: {out}"
    );
    assert_eq!(p["task_id"], repair, "{ctx}");
    assert_eq!(p["landed_commit"], landed, "{ctx}");
    assert_eq!(p["tested_commit"], tested, "{ctx}");
}
/// Archived and rejected owners suppress until the fixed 6-hour clock boundary.
fn assert_archive_suppresses(
    label: &str,
    log: &str,
    later: &str,
    idx: usize,
    same: bool,
    audit: bool,
) {
    for status in [TaskStatus::Archived, TaskStatus::Rejected] {
        let (_root, rt, c, first, owner) = fixture_owner(log);
        archive_owner(&rt, &owner, status, None);
        let h = history(&rt, &owner);
        let at = h
            .iter()
            .rev()
            .find(|e| e.to_status == Some(status))
            .unwrap()
            .at;
        let before = rt
            .clone()
            .with_ci_failure_time(at + Duration::hours(6) - Duration::nanoseconds(1));
        let withheld = file_one(&before, later, 1, &c[idx]);
        assert_filed(&withheld, 0, &format!("{label} ({status:?})"));
        assert_eq!(withheld["pilot_candidate_count"], 0, "{label}");
        let w = &withheld["withheld"][0];
        assert_str(w, "outcome", "withheld");
        assert_str(w, "reason", "operator_archived");
        assert_str(w, "archive_actor", "human:fixture");
        assert_str(w, "owner", &owner);
        assert_eq!(
            w["failure_key"] == first["filed"][0]["failure_key"],
            same,
            "{label}"
        );
        if audit {
            assert_eq!(
                withheld["audit"]["withheld"], withheld["withheld"],
                "{label}"
            );
        }
        let after = rt.with_ci_failure_time(at + Duration::hours(6));
        let released = file_one(&after, later, 2, &c[2]);
        assert_filed(&released, 1, &format!("{label} ({status:?})"));
        assert_empty(&released, "withheld", label);
    }
}
fn compiler_observation(
    rid: &str,
    run: &Value,
    checkout: &str,
    key: &str,
) -> (String, String, String) {
    let url = run["url"].as_str().unwrap();
    let body = format!(
        "{{\"run_id\":\"{rid}\",\"run_url\":\"{url}\",\"checkout\":\"{checkout}\",\"failure_key\":\"{key}\"}}"
    );
    let path = format!(
        "ci-compiler-observations/{}.json",
        sha256_hex(body.as_bytes())
    );
    let cmt = format!(
        "CI compiler observation: run `{rid}` {url}; checkout `{checkout}`; exact failure key `ci-failure:{key}`."
    );
    (path, body, cmt)
}
fn assert_retained_branch_failure(
    rt: &OrbitRuntime,
    out: &Value,
    owner_id: &str,
    pr: &Value,
) -> String {
    assert_str(&out["attributed"][0], "task_id", owner_id);
    let path = out["attributed"][0]["artifact"]
        .as_str()
        .unwrap()
        .to_string();
    let receipt = rt.get_task_artifact(owner_id, &path).unwrap().unwrap();
    assert_eq!(art_json(&receipt)["failure"], *pr);
    path
}
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ci_failure_goldens")
}
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_CI_LOG_GOLDEN_CHILD";
    if std::env::var_os(MARKER).is_some() {
        return true;
    }
    let (home, logs) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|key| {
        cmd.env_remove(key);
    });
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"]);
    cmd.env(MARKER, "1")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let out = orbit_common::test_env::run_child_test(&mut cmd, test, logs.path());
    orbit_common::test_env::assert_child_test_passed(test, out.status, out.stdout, out.stderr);
    false
}
fn dep_log(dep: &str) -> String {
    dep_log_for(dep, 911)
}
fn dep_log_for(dep: &str, job_id: u64) -> String {
    format!(
        "orbit-dependency-evidence:{{\"dependencies\":[{{\"conclusion\":\"failure\",\"job_id\":{job_id},\"job_name\":\"{dep}\",\"status\":\"completed\"}}],\"execution_contract\":\"isolated-source-v1\",\"kind\":\"dependency_failed\",\"schema_version\":1}}\nci-await: {dep} concluded failure: https://example.invalid/job\n##[error]Process completed with exit code 1."
    )
}

#[test]
fn ci_checkout_identity_gaps_are_named_at_the_filing_boundary() {
    if !isolated("ci_checkout_identity_gaps_are_named_at_the_filing_boundary") {
        return;
    }
    let (_root, rt, c) = operator_fixture("");
    let admitted = file_one(&rt, "error: valid checkout identity", 0, &c[0]);
    assert_filed(&admitted, 1, "admitted");
    assert_eq!(admitted["deferred_count"], 0, "{admitted}");
    let cases = &[
        ("incomplete", true, true, 1),
        ("missing", true, true, 1),
        ("ambiguous", true, true, 1),
        ("observed", false, true, 1),
        ("observed", true, false, 1),
        ("observed", true, true, 0),
        ("observed", true, true, 2),
    ];
    for (offset, &(state, prov_ok, job_ok, shas)) in cases.iter().enumerate() {
        let idx = offset + 1;
        let co = &c[idx % c.len()];
        let mut run = failure(&format!("error: checkout identity case {offset}"), idx, co);
        run["checkout_identity"]["state"] = json!(state);
        run["checkout_identity"]["provenance"]["complete"] = json!(prov_ok);
        if !job_ok {
            run["checkout_identity"]["provenance"]["job_id"] = json!(99_000 + idx);
        }
        run["actual_checkout_shas"] = match shas {
            0 => json!([]),
            1 => run["actual_checkout_shas"].clone(),
            _ => json!([co, &c[(idx + 1) % c.len()]]),
        };
        let out = retryable_error_payload(file_err(&rt, run));
        let r = &out["errors"][0];
        assert_str(&out, "outcome", "retryable_error");
        assert_str(&out, "stage", "collection_or_investigation");
        assert_str(r, "stage", "registration");
        assert_str(r, "operation", "job_evidence_identity");
        assert_eq!(r["retryable"], true, "{out}");
        let msg = format!(
            "checkout identity is not completely observed for this job (state={state}, state_is_observed={}, provenance.complete={prov_ok}, provenance.job_id_matches={job_ok}, checkout_sha_count={shas}, checkout_sha_count_is_one={})",
            state == "observed",
            shas == 1
        );
        assert_eq!(r["message"], msg, "{out}");
    }
    let mut bad = failure("error: missing log evidence", 9, &c[0]);
    bad["log_excerpt"] = json!("");
    bad["log_truncated"] = json!(true);
    let p = retryable_error_payload(file_err(&rt, bad));
    assert_str(
        &p["errors"][0],
        "message",
        "job diagnostic evidence is missing or truncated",
    );
}

#[test]
fn ci_snapshot_wide_retryable_errors_defer_without_filing() {
    if !isolated("ci_snapshot_wide_retryable_errors_defer_without_filing") {
        return;
    }
    let (_root, rt, c) = operator_fixture("");
    let run = failure("error: would file without the snapshot-wide gap", 0, &c[0]);
    let extra = json!({ "retryable_errors": [{ "stage": "collection", "operation": "list_runs", "run_id": null, "job_id": null, "retryable": true, "message": "run listing did not finish" }] });
    let out = file_input(&rt, evidence_input(vec![run], extra)).expect("defers");
    assert_str(&out, "outcome", "deferred");
    assert_str(&out, "stage", "collection_or_investigation");
    assert_filed(&out, 0, "snapshot_wide");
    assert_empty(&out, "filed", "snapshot_wide");
    assert_eq!(out["audit"]["retryable_errors"], 1, "{out}");
    assert_str(&out["retryable_errors"][0], "operation", "list_runs");
    let no_sweep = rt
        .list_tasks()
        .unwrap()
        .iter()
        .all(|t| !t.tags.iter().any(|g| g == "ci-failure-sweep"));
    assert!(no_sweep);
}

#[test]
fn ci_dependency_only_aggregate_folds_into_failed_sibling() {
    if !isolated("ci_dependency_only_aggregate_folds_into_failed_sibling") {
        return;
    }
    let sib = |ch: &str, inv: bool| {
        let mut s = named_failure("error: sibling test exploded", 1, ch, 4242, SIB_JOB, "Test");
        s["investigated"] = json!(inv);
        s
    };
    let dl = dep_log(SIB_JOB);
    let agg_step = "Await the test build and nextest shards";
    {
        let (_root, rt, c) = operator_fixture("");
        let r0 = named_failure(&dl, 0, &c[0], 4242, AGG_JOB, agg_step);
        let out = file(&rt, vec![r0, sib(&c[0], true)]);
        assert_filed(&out, 1, "folds_into_sibling");
        assert_str(&out["filed"][0], "job", SIB_JOB);
        assert_eq!(out["folded_count"], 1, "{out}");
        assert_str(&out["folded"][0], "job", AGG_JOB);
        assert_eq!(
            out["folded"][0]["covered_by_task_id"],
            out["filed"][0]["task_id"]
        );
        assert!(task_desc(&rt, filed_id(&out, 0)).contains(AGG_JOB));
    }
    let (df, tail) = (
        dep_log("Test partition 9"),
        "##[error]Process completed with exit code 1.",
    );
    let own = format!(
        "{}\nerror: aggregate's own check broke\n{tail}",
        dl.lines().next().unwrap()
    );
    let b1 = format!("orbit-dependency-evidence:{{not-json}}\n{tail}");
    let b2 = format!("orbit-dependency-evidence:{{\"dependencies\":[]}}\n{tail}");
    let b3 = format!(
        "orbit-dependency-evidence:{{\"dependencies\":[{{\"conclusion\":\"success\",\"job_name\":\"Test partition 2\"}}]}}\n{tail}"
    );
    let cases = [
        ("own_error", own.as_str(), true, true, 2, 0),
        ("non_failed_dep", &df, true, true, 2, 0),
        ("malformed_json", &b1, false, true, 1, 0),
        ("empty_deps", &b2, false, true, 1, 0),
        ("success_dep", &b3, false, true, 1, 0),
    ];
    for (label, log, with_sib, sib_inv, filed_cnt, def_cnt) in cases {
        let (_root, rt, c) = operator_fixture("");
        let mut runs = vec![named_failure(log, 0, &c[0], 4242, AGG_JOB, agg_step)];
        if with_sib {
            runs.push(sib(&c[0], sib_inv));
        }
        let out = file(&rt, runs);
        assert_filed(&out, filed_cnt, label);
        assert_eq!(out["folded_count"], 0, "{label}: {out}");
        if def_cnt > 0 {
            assert_eq!(out["deferred_count"], def_cnt, "{label}: {out}");
        }
    }
    {
        // A waiter whose named sibling is uninvestigated is held with it: with
        // no leaf evidence there is nothing for a waiter task to repair.
        let (_root, rt, c) = operator_fixture("");
        let waiter = named_failure(&dl, 0, &c[0], 4242, AGG_JOB, agg_step);
        let payload =
            retryable_error_payload(file_result(&rt, vec![waiter, sib(&c[0], false)]).unwrap_err());
        let operations: Vec<_> = payload["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|error| error["operation"].as_str().unwrap().to_string())
            .collect();
        assert!(
            operations.contains(&"dependency_log_pending".to_string()),
            "{payload}"
        );
        assert!(rt.list_tasks().unwrap().is_empty(), "{payload}");
    }
    for (old, new, same) in [
        ("\"job_id\":911", "\"job_id\":999", false),
        ("\"job_id\":911,", "", false),
        ("\"job_id\":911", "\"job_id\":999", true),
    ] {
        let (_root, rt, c) = operator_fixture("");
        let log = compiler_log("E0433", "missing module", "crates/example/src/lib.rs", 8, 1);
        let j1 = if same { SIB_JOB } else { "Other job" };
        let r0 = named_failure(&dl.replace(old, new), 0, &c[0], 4242, AGG_JOB, agg_step);
        let r1 = named_failure(&log, 1, &c[0], 4242, j1, "Build");
        let r2 = named_failure(&log, 2, &c[0], 4343, SIB_JOB, "Build");
        let out = file(&rt, vec![r0, r1, r2]);
        assert_filed(&out, 2, "dep_identity");
        assert_eq!(out["folded_count"], 0, "{out}");
    }
}

#[test]
fn ci_dependency_only_chain_reports_the_covering_task() {
    if !isolated("ci_dependency_only_chain_reports_the_covering_task") {
        return;
    }
    let (_root, rt, c) = operator_fixture("");
    let n = |log: &str, idx, job, step| named_failure(log, idx, &c[0], 4242, job, step);
    let (l1, l2) = (dep_log_for(MID_JOB, 911), dep_log_for(LEAF_JOB, 912));
    let r0 = n(&l1, 0, OUTER_JOB, "Await the outer aggregate");
    let r1 = n(&l2, 1, MID_JOB, "Await the middle aggregate");
    let r2 = n("error: leaf test exploded", 2, LEAF_JOB, "Test");
    let out = file(&rt, vec![r0, r1, r2]);
    assert_filed(&out, 1, "chain");
    assert_str(&out["filed"][0], "job", LEAF_JOB);
    assert_eq!(out["folded_count"], 2, "{out}");
    let tid = filed_id(&out, 0).to_string();
    let desc = task_desc(&rt, &tid);
    for job in [OUTER_JOB, MID_JOB] {
        let f = arr(&out, "folded")
            .iter()
            .find(|e| e["job"] == job)
            .unwrap();
        assert_eq!(f["covered_by_task_id"], tid, "{out}");
        assert!(desc.contains(job), "leaf task carries {job}: {desc}");
    }
}

#[test]
fn ci_persisted_compiler_owner_keys_keep_first_substring_diagnostic() {
    if !isolated("ci_persisted_compiler_owner_keys_keep_first_substring_diagnostic") {
        return;
    }
    let (p1, p2, p3) = (
        "ci-guardrails: check-error-translation.sh 5.785 s",
        "error[vulnerability]: timing variability",
        "error[E0062]: first\n --> crates/example/src/lib.rs:3:1",
    );
    for (prefix, key) in [
        (p1, "e8854db646a4fe9d"),
        (p2, "2929103cafa315be"),
        (p3, "656050554f8e32a1"),
    ] {
        let (_root, rt, c) = operator_fixture("");
        let desc = format!(
            "run `10` (id `910`) commit actually checked out: `{}`",
            c[0]
        );
        let owner = add_sweep_task(&rt, "Persisted compiler sweep owner", &desc, key);
        let log = format!(
            "{prefix}\nerror[E0433]: cannot find module `real`\n --> crates/example/src/lib.rs:8:1\n##[error]Process completed with exit code 1."
        );
        let out = file_one(&rt, &log, 0, &c[0]);
        assert_filed(&out, 0, prefix);
        assert_skipped(&out, &owner.id, prefix);
    }
}

#[test]
fn ci_failure_identity_survives_description_rewrite() {
    if !isolated("ci_failure_identity_survives_description_rewrite") {
        return;
    }
    for archived in [false, true] {
        let (_root, rt, c, first, owner) = fixture_owner(&sandbox_at(277, 25));
        let val = serde_json::to_value(history(&rt, &owner)).unwrap();
        let hist: Vec<TaskHistoryEntry> = serde_json::from_value(val).unwrap();
        let id = CiFailureIdentity::from_history(&hist).unwrap().unwrap();
        assert_eq!(id.failure_key, first["filed"][0]["failure_key"]);
        assert!(
            id.compiler_cause_digest.is_some()
                && id.open_compiler_set.is_some()
                && id.signature.is_some()
        );
        assert_eq!(id.tested_commit, c[0]);
        let src = CiFailureSource {
            run_id: "10".into(),
            job_id: "910".into(),
            checkout: c[0].clone(),
        };
        assert_eq!(id.sources, vec![src]);
        set_desc(&rt, &owner, "Operator rewrote the brief.");
        if archived {
            archive_owner(&rt, &owner, TaskStatus::Archived, None);
        }
        let second = file_one(&rt, &sandbox_at(280, 28), 1, &c[1]);
        assert_filed(&second, 0, &format!("archived={archived}"));
        let matched = if archived {
            &second["withheld"][0]["owner"]
        } else {
            &second["skipped_existing"][0]["task_id"]
        };
        assert_eq!(matched, &owner);
    }
}

#[test]
fn ci_open_compiler_owner_survives_checkout_and_coordinate_changes() {
    if !isolated("ci_open_compiler_owner_survives_checkout_and_coordinate_changes") {
        return;
    }
    let (log1, log2) = sandbox_win_pair();
    let cases = [
        (TaskStatus::Proposed, false),
        (TaskStatus::Backlog, false),
        (TaskStatus::InProgress, false),
        (TaskStatus::Review, false),
        (TaskStatus::Blocked, false),
        (TaskStatus::Proposed, true),
    ];
    for (status, legacy) in cases {
        let (_root, rt, c, first, filed_owner) = fixture_owner(&log1);
        let leg = legacy.then(|| description_only_owner(&rt, &filed_owner));
        let owner = leg.as_deref().unwrap_or(&filed_owner);
        patch_task(&rt, owner, |p| {
            p.plan = Some("Reproduce and repair.".into())
        });
        set_status(&rt, owner, status);
        let (before, cmts0) = (get_task(&rt, owner), cmts_len(&rt, owner));
        let run2 = failure(&log2, 1, &c[1]);
        let second = file(&rt, vec![run2.clone()]);
        assert_filed(&second, 0, &format!("{status:?}, legacy={legacy}"));
        let m = &second["skipped_existing"][0];
        assert_str(m, "task_id", owner);
        assert_str(m, "match_kind", "open_compiler_diagnostics");
        assert_ne!(m["failure_key"], first["filed"][0]["failure_key"]);
        let cmts = comments(&rt, owner);
        assert_eq!(cmts.len(), cmts0 + 1);
        let (obs, mkey) = (
            &cmts.last().unwrap().message,
            m["failure_key"].as_str().unwrap(),
        );
        let url2 = run2["url"].as_str().unwrap();
        assert!(
            obs.contains(&c[1]) && obs.contains(url2) && obs.contains(mkey),
            "{obs}"
        );
        let after = get_task(&rt, owner);
        assert_eq!(
            after.tags, before.tags,
            "exact key is evidence, not rewritten"
        );
        assert_eq!(after.status, status);
        assert_eq!(after.description, before.description);
        assert_filed(&file(&rt, vec![run2]), 0, "retry");
        assert_eq!(cmts_len(&rt, owner), cmts.len());
        let leg_run = failure(&log2, 2, &c[1]);
        let (_, _, leg_msg) = compiler_observation("12", &leg_run, &c[1], mkey);
        patch_task(&rt, owner, |p| p.comment = Some(leg_msg));
        let (cmts_leg, arts0) = (comments(&rt, owner), arts_len(&rt, owner));
        assert_filed(&file(&rt, vec![leg_run.clone()]), 0, "legacy_result");
        assert_eq!(comments(&rt, owner), cmts_leg);
        let arts = artifacts(&rt, owner);
        assert_eq!(arts.len(), arts0 + 1);
        let is_r12 = |a: &&TaskArtifact| {
            a.path.starts_with("ci-compiler-observations/") && art_json(a)["run_id"] == "12"
        };
        let rcpt = arts.iter().find(is_r12).unwrap();
        let exp = json!({"run_id": "12", "run_url": leg_run["url"], "checkout": c[1], "failure_key": mkey});
        assert_eq!(art_json(rcpt), exp);
        file(&rt, vec![leg_run]);
        assert_eq!(comments(&rt, owner), cmts_leg);
        assert_eq!(artifacts(&rt, owner), arts);
        assert_eq!(rt.list_tasks().unwrap().len(), 1 + usize::from(legacy));
    }
    let (_root, rt, c) = operator_fixture("");
    let snap = file(
        &rt,
        vec![failure(&log1, 0, &c[0]), failure(&log2, 1, &c[1])],
    );
    assert_filed(&snap, 1, "same_snapshot");
    assert_eq!(
        snap["skipped_existing"][0]["task_id"],
        snap["filed"][0]["task_id"]
    );
    assert!(comments(&rt, filed_id(&snap, 0))[0].message.contains(&c[1]));
}

#[test]
fn ci_compiler_observation_receipt_without_comment_is_appended_once() {
    if !isolated("ci_compiler_observation_receipt_without_comment_is_appended_once") {
        return;
    }
    let (log1, log2) = sandbox_win_pair();
    let (_root, rt, c, _first, owner) = fixture_owner(&log1);
    let run2 = failure(&log2, 1, &c[1]);
    let second = file(&rt, vec![run2.clone()]);
    assert_filed(&second, 0, "second");
    let key = second["skipped_existing"][0]["failure_key"]
        .as_str()
        .unwrap()
        .to_string();
    let cmts0 = comments(&rt, &owner);
    file(&rt, vec![run2]);
    assert_eq!(comments(&rt, &owner), cmts0);
    let run3 = failure(&log2, 2, &c[1]);
    let (p3, c3, msg3) = compiler_observation("12", &run3, &c[1], &key);
    patch_task(&rt, &owner, |p| {
        p.upsert_artifacts = vec![TaskArtifact::from_text(&p3, &c3)]
    });
    assert!(rt.get_task_artifact(&owner, &p3).unwrap().is_some());
    let cmts1 = comments(&rt, &owner);
    assert!(!cmts1.iter().any(|x| x.message.contains("run `12`")));
    assert_filed(&file(&rt, vec![run3.clone()]), 0, "recovered");
    let cmts2 = comments(&rt, &owner);
    assert_eq!(cmts2.len(), cmts1.len() + 1);
    assert_eq!(cmts2.last().unwrap().message, msg3);
    file(&rt, vec![run3]);
    assert_eq!(comments(&rt, &owner), cmts2);
    let run4 = failure(&log2, 3, &c[1]);
    let (p4, _, _) = compiler_observation("13", &run4, &c[1], &key);
    let (_, bad, _) = compiler_observation("13", &run4, &c[0], &key);
    patch_task(&rt, &owner, |p| {
        p.upsert_artifacts = vec![TaskArtifact::from_text(&p4, &bad)]
    });
    let msg = file_err(&rt, run4).to_string();
    assert!(
        msg.contains("compiler observation receipt does not match its identity"),
        "{msg}"
    );
    assert_eq!(comments(&rt, &owner), cmts2);
}

#[test]
fn ci_landed_compiler_repair_does_not_cover_locationless_diagnostic() {
    if !isolated("ci_landed_compiler_repair_does_not_cover_locationless_diagnostic") {
        return;
    }
    let (_root, rt, c) = operator_commits("", 4);
    let log = sandbox_at(277, 25);
    let owner = filed_id(&file_one(&rt, &log, 0, &c[0]), 0).to_string();
    set_status(&rt, &owner, TaskStatus::Done);
    record_landing(&rt, &owner, &c[1], &c[2]);
    let locless = with_job(failure(log.lines().next().unwrap(), 1, &c[1]), "Coverage");
    let observed = file(&rt, vec![locless]);
    assert_filed(
        &observed,
        1,
        "display-only signature cannot cover locationless",
    );
    assert_empty(&observed, "pending_supersession", "locationless");
}

/// The compiler failure key embeds the observed checkout, so an operator's hold
/// must follow the location-free diagnostic set across agent-main advances.
#[test]
fn ci_operator_cover_on_a_compiler_owner_holds_at_a_later_checkout() {
    if !isolated("ci_operator_cover_on_a_compiler_owner_holds_at_a_later_checkout") {
        return;
    }
    let (log1, other) = (sandbox_at(277, 25), e0425_foo(300));
    let later = log1.replace(":277:25", ":280:28");
    let (_root, rt, c, first, owner) = fixture_owner(&log1);
    let cover = add_titled(&rt, "Hand fix");
    archive_owner(&rt, &owner, TaskStatus::Archived, Some(&cover.id));
    let tasks_before = rt.list_tasks().unwrap().len();
    for (idx, checkout) in [(1, &c[1]), (2, &c[2])] {
        let held = file_one(&rt, &later, idx, checkout);
        assert_filed(&held, 0, "compiler_cover");
        assert_eq!(held["pilot_candidate_count"], 0, "{held}");
        assert_covered(&held, "operator_cover_open", &owner, &cover.id, "cover");
        assert_ne!(
            held["withheld"][0]["failure_key"], first["filed"][0]["failure_key"],
            "exact key"
        );
    }
    assert_eq!(rt.list_tasks().unwrap().len(), tasks_before);
    assert_filed(&file_one(&rt, &other, 3, &c[2]), 1, "unrelated_diag");
}

#[test]
fn ci_compiler_coverage_requires_the_same_complete_diagnostic_set() {
    if !isolated("ci_compiler_coverage_requires_the_same_complete_diagnostic_set") {
        return;
    }
    let diag = |c, m, p, l| compiler_log(c, m, p, l, 25);
    let (log1, shifted, other) = (
        diag("E0433", SANDBOX_MSG, ENTRY_NIX, 277),
        diag("E0433", SANDBOX_MSG, ENTRY_NIX, 280),
        e0425_foo(300),
    );
    let (sup, sub) = (format!("{shifted}{other}"), format!("{log1}{other}"));
    let (p2, m2) = (
        "crates/orbit-core/src/runtime/mod.rs",
        "cannot find `other_sandbox` in `runtime`",
    );
    let c1 = diag("E0425", SANDBOX_MSG, ENTRY_NIX, 280);
    let c2 = diag("E0433", SANDBOX_MSG, p2, 280);
    let c3 = diag("E0433", m2, ENTRY_NIX, 280);
    let (l, s) = (log1.as_str(), shifted.as_str());
    let cases = [
        ("code", l, c1.as_str(), true, false),
        ("path", l, c2.as_str(), true, false),
        ("message", l, c3.as_str(), true, false),
        ("superset", l, sup.as_str(), true, false),
        ("subset", sub.as_str(), s, true, false),
        ("manual owner", l, s, false, false),
        ("done owner", l, s, true, true),
    ];
    for (label, original, observed, sweep_tag, mark_done) in cases {
        let (_root, rt, c, _first, owner) = fixture_owner(original);
        if !sweep_tag {
            let tags = get_task(&rt, &owner)
                .tags
                .into_iter()
                .filter(|t| t != "ci-failure-sweep")
                .collect();
            patch_task(&rt, &owner, |p| p.tags = Some(tags));
        }
        if mark_done {
            set_status(&rt, &owner, TaskStatus::Done);
        }
        let second = file_one(&rt, observed, 1, &c[1]);
        assert_filed(&second, 1, label);
        assert_ne!(filed_id(&second, 0), owner, "{label}");
        assert!(comments(&rt, &owner).is_empty(), "{label}");
    }
    let pad = "    source context\n".repeat(30);
    let (_root, rt, c, _first, owner) = fixture_owner(&format!("{log1}{pad}{other}"));
    let second = file_one(&rt, &format!("{shifted}{pad}{other}"), 1, &c[1]);
    assert_filed(&second, 0, "padded");
    assert_skipped(&second, &owner, "padded");
    description_only_owner(&rt, &owner);
    let partial = file_one(&rt, &shifted, 2, &c[2]);
    assert_filed(&partial, 1, "partial legacy excerpt");
    let (_root, rt, c, first, _owner) = fixture_owner(&sub);
    let reordered = file_one(&rt, &format!("{other}{shifted}{shifted}"), 1, &c[1]);
    assert_filed(&reordered, 0, "reordered");
    assert_skipped(&reordered, filed_id(&first, 0), "reordered");
}

#[test]
fn ci_operator_archive_and_reject_suppress_until_the_clock_boundary() {
    if !isolated("ci_operator_archive_and_reject_suppress_until_the_clock_boundary") {
        return;
    }
    let (plain, c1) = ("error: store GC invariant broken", sandbox_at(277, 25));
    let c2 = c1.replace(":277:25", ":280:28");
    assert_archive_suppresses("exact_log", plain, plain, 2, true, true);
    assert_archive_suppresses("compiler_later", &c1, &c2, 1, false, false);
}

#[test]
fn ci_operator_task_cover_holds_until_a_checkout_contains_the_landing() {
    if !isolated("ci_operator_task_cover_holds_until_a_checkout_contains_the_landing") {
        return;
    }
    for (label, follower) in [("local_delivery", false), ("follower_delivery", true)] {
        let log = "error: store GC invariant broken";
        let (_root, rt, c, _first, owner) = fixture_owner(log);
        let title = if follower {
            "Follower hand fix"
        } else {
            "Hand fix"
        };
        let cover = add_titled(&rt, title);
        archive_owner(&rt, &owner, TaskStatus::Archived, Some(&cover.id));
        if !follower {
            let held = file_one(&rt, log, 1, &c[0]);
            assert_filed(&held, 0, label);
            assert_covered(&held, "operator_cover_open", &owner, &cover.id, label);
            set_status(&rt, &cover.id, TaskStatus::Done);
            record_landing(&rt, &cover.id, &c[0], &c[1]);
        } else {
            record_owner_landing(&rt, &cover.id, &c[1]);
        }
        let old = file_one(&rt, log, if follower { 1 } else { 2 }, &c[0]);
        assert_filed(&old, 0, label);
        assert_str(&old["withheld"][0], "reason", "awaiting_cover_commit");
        if follower {
            assert_covered(&old, "awaiting_cover_commit", &owner, &cover.id, label);
            assert_eq!(old["withheld"][0]["landed_commit"], c[1], "{label}");
        }
        let repro = file_one(&rt, log, if follower { 2 } else { 3 }, &c[2]);
        assert_filed(&repro, 1, label);
        let fc = &repro["filed"][0]["failed_covers"][0];
        assert_eq!(fc["cover"], cover.id, "{label}");
        assert_eq!(fc["reason"], "cover_did_not_hold", "{label}");
        if follower {
            assert_eq!(fc["landed_commit"], c[1], "{label}");
        } else {
            let desc = task_desc(&rt, filed_id(&repro, 0));
            assert!(desc.contains(&cover.id) && desc.contains(&c[1]), "{label}");
        }
    }
}

/// A failed operator cover must not erase a later repair's ownership.
#[test]
fn ci_operator_failed_cover_preserves_a_second_landed_repair() {
    if !isolated("ci_operator_failed_cover_preserves_a_second_landed_repair") {
        return;
    }
    for (cover_matches, other_job) in [(false, false), (false, true), (true, false)] {
        let (_root, rt, mut c) = operator_fixture("");
        c.extend(commit_chain(&rt.paths().repo_root, 2));
        let log = "error: store GC invariant broken";
        let owner = filed_id(&file_one(&rt, log, 0, &c[0]), 0).to_string();
        let desc = if cover_matches {
            task_desc(&rt, &owner)
        } else {
            String::new()
        };
        let cover = add_task(&rt, "Hand fix", &desc, "", vec![], false);
        archive_owner(&rt, &owner, TaskStatus::Archived, Some(&cover.id));
        set_status(&rt, &cover.id, TaskStatus::Done);
        record_landing(&rt, &cover.id, &c[0], &c[1]);
        let repro = file_one(&rt, log, 1, &c[2]);
        assert_filed(&repro, 1, "repro");
        assert_str(&repro["filed"][0]["failed_covers"][0], "cover", &cover.id);
        let repair = filed_id(&repro, 0);
        set_status(&rt, repair, TaskStatus::Done);
        record_landing(&rt, repair, &c[2], &c[3]);
        let f = failure(log, 2, &c[2]);
        let stale_run = if other_job {
            with_job(f, "Coverage")
        } else {
            f
        };
        let stale = file(&rt, vec![stale_run.clone()]);
        assert_filed(&stale, 0, "stale");
        assert_eq!(stale["pilot_candidate_count"], 0, "{stale}");
        assert_empty(&stale, "withheld", "stale");
        if other_job || cover_matches {
            assert_empty(&stale, "skipped_existing", "stale");
            assert_pending(&stale, repair, &c[3], &c[2], "failed_cover");
            assert_eq!(stale["audit"]["pending_supersession_run_ids"], json!([12]));
        } else {
            assert_empty(&stale, "pending_supersession", "stale");
            assert_skipped(&stale, repair, "stale");
            assert_str(
                &stale["skipped_existing"][0],
                "match_kind",
                "material_coverage",
            );
        }
        assert_filed(&file(&rt, vec![stale_run]), 0, "repeat_stale");
        let fresh = file(&rt, vec![with_job(failure(log, 3, &c[4]), "Coverage")]);
        assert_filed(&fresh, 1, "fresh");
        assert_empty(&fresh, "pending_supersession", "fresh");
    }
}

#[cfg(unix)]
#[test]
fn ci_operator_pr_cover_reports_open_landed_closed_and_unavailable_states() {
    if !isolated("ci_operator_pr_cover_reports_open_landed_closed_and_unavailable_states") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let (root, rt, c) = operator_fixture("");
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (resp, gh) = (bin.join("response.json"), bin.join("gh"));
    std::fs::write(&gh, format!("#!/bin/sh\ncat '{}'\n", resp.display())).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    orbit_common::test_env::prime_executable(&mut std::process::Command::new(&gh));
    let new_path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    // SAFETY: `isolated` runs this test alone with `--test-threads=1` in a
    // dedicated child process, so no other test or thread reads `PATH` concurrently.
    unsafe {
        std::env::set_var("PATH", new_path);
    }
    let log = "error: store GC invariant broken";
    let owner = filed_id(&file_one(&rt, log, 0, &c[0]), 0).to_string();
    let cover = "github-pr:https://github.com/acme/orbit/pull/3854";
    archive_owner(&rt, &owner, TaskStatus::Rejected, Some(cover));
    let cases = [
        ("OPEN", &c[2], "operator_cover_open", 0),
        ("UNKNOWN", &c[2], "operator_cover_unavailable", 0),
        ("MERGED", &c[0], "awaiting_cover_commit", 0),
        ("MERGED", &c[2], "cover_did_not_hold", 1),
    ];
    for (state, checkout, reason, count) in cases {
        let body = json!({"state": state, "mergeCommit": {"oid": c[1]}}).to_string();
        std::fs::write(&resp, body).unwrap();
        let out = file_one(&rt, log, 1, checkout);
        assert_filed(&out, count, state);
        if count == 0 {
            assert_covered(&out, reason, &owner, cover, state);
        } else {
            assert_str(&out["filed"][0]["failed_covers"][0], "reason", reason);
        }
    }
    let other = file_one(&rt, "error: different invariant broken", 2, &c[0]);
    let pr2 = Some("github-pr:3855");
    archive_owner(&rt, filed_id(&other, 0), TaskStatus::Archived, pr2);
    std::fs::write(&resp, json!({"state": "CLOSED"}).to_string()).unwrap();
    assert_filed(
        &file_one(&rt, "error: different invariant broken", 3, &c[2]),
        1,
        "closed_pr",
    );
}

#[test]
fn ci_equal_test_signatures_share_one_task_across_job_and_step_wrappers() {
    if !isolated("ci_equal_test_signatures_share_one_task_across_job_and_step_wrappers") {
        return;
    }
    let (_root, rt, c) = operator_fixture("");
    let log = "thread 'store_gc' panicked at tests/store_gc.rs:42:3:\nassertion failed: live store is retained\ntest store_gc ... FAILED";
    let first = failure(log, 0, &c[0]);
    let mut cov = with_job(failure(log, 1, &c[0]), "Coverage");
    cov["run_id"] = first["run_id"].clone();
    cov["url"] = first["url"].clone();
    cov["failed_jobs"][0]["failed_steps"][0]["name"] = json!("Collect coverage");
    let filed = file(&rt, vec![first.clone(), cov.clone()]);
    assert_filed(&filed, 1, "shared");
    assert_eq!(filed["filed"][0]["jobs"], json!(["Coverage", "build"]));
    assert_len(&filed["filed"][0], "sources", 2);
    let desc = task_desc(&rt, filed_id(&filed, 0));
    assert!(desc.contains("Coverage") && desc.contains("build"));
    let repeat = file(&rt, vec![cov, first]);
    assert_filed(&repeat, 0, "repeat");
    assert_eq!(
        repeat["skipped_existing"][0]["failure_key"],
        filed["filed"][0]["failure_key"]
    );
    let other = "thread 'another_test' panicked at tests/other.rs:1:1:\nassertion failed: another invariant\ntest another_test ... FAILED";
    assert_filed(&file_one(&rt, other, 2, &c[0]), 1, "distinct");
    let (_lr, lrt, lc) = operator_fixture("");
    let legacy = add_sweep_task(&lrt, "Persisted sweep finding", "", "5e1a7df48e870766");
    archive_owner(&lrt, &legacy.id, TaskStatus::Archived, None);
    let hold = file_one(&lrt, log, 3, &lc[2]);
    assert_filed(&hold, 0, "legacy_hold");
    let w = &hold["withheld"][0];
    assert_str(w, "candidate_owner", &legacy.id);
    assert_str(w, "reason", "archive_identity_unavailable");
    assert!(w["owner"].is_null());
    assert_eq!(w["failure_key"], filed["filed"][0]["failure_key"]);
}

#[test]
fn ci_main_thread_panic_does_not_match_agent_main_task() {
    if !isolated("ci_main_thread_panic_does_not_match_agent_main_task") {
        return;
    }
    let (_root, rt, c) = operator_fixture("");
    let unrelated = add_titled(&rt, "Review agent-main delivery workflow");
    let out = file_one(&rt, "thread 'main' panicked at build.rs:3:5:", 0, &c[0]);
    assert_filed(&out, 1, "main_thread");
    assert_empty(&out, "skipped_existing", "main_thread");
    assert_ne!(
        out["filed"][0]["task_id"], unrelated.id,
        "generic panic must not match unrelated"
    );
}

/// Hand-built collection rows: the log is the snapshot's excerpt.
fn filed_golden(case: &Value) -> Value {
    let (_root, rt) = seeded_golden_runtime();
    let mut results = Vec::new();
    let logs = arr(case, "logs");
    let split = (case["first_batch"].as_u64().unwrap_or(1) as usize).min(logs.len());
    let mut run_index = 0;
    for (gi, group) in std::iter::once(&logs[..split])
        .chain(logs[split..].chunks(1))
        .enumerate()
    {
        let co = sha(if gi == 0 { '3' } else { '4' });
        let mut runs = Vec::new();
        for log in group {
            let text = log
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| expand_parts(log));
            runs.push(failure(&text, run_index, &co));
            run_index += 1;
        }
        results.push(filed_result(&rt, &file(&rt, runs)));
    }
    json!(results)
}

/// Each log is served by a substitute `gh` as one failed job's `--log-failed`
/// output and goes through host collection before filing.
#[cfg(unix)]
fn collected_golden(case: &Value) -> Value {
    let mut ws = sweep::Workspace::seeded();
    let mut results = Vec::new();
    for (index, parts) in arr(case, "logs").iter().enumerate() {
        let log = expand_parts(parts);
        let run_id = 7_000 + index as u64;
        ws.push_red_run(run_id, 9_000 + index as u64, &log);
        let (ev, filed) = ws.sweep();
        let out = filed.unwrap_or_else(|e| panic!("file CI failure: {e}"));
        let f = arr(&ev, "current_failures")
            .iter()
            .find(|f| f["run_id"] == json!(run_id))
            .unwrap();
        let unit = &f["diagnostic_unit"];
        let mut res = filed_result(&ws.runtime, &out);
        res["collected"] = json!({ "source_bytes": log.len(), "log_truncated": f["log_truncated"], "retryable_errors": ev["retryable_errors"], "diagnostic_kind": unit["kind"], "diagnostic_step": unit["step"], "step_attribution": unit["step_attribution"], "failure_anchor_count": unit["failure_anchor_count"] });
        results.push(res);
    }
    json!(results)
}

/// Concatenate fixture parts: strings and `{"text", "repeat"}` segments.
fn expand_parts(parts: &Value) -> String {
    let mut out = String::new();
    for p in parts.as_array().unwrap() {
        match p {
            Value::String(s) => out.push_str(s),
            seg => out.push_str(
                &seg["text"]
                    .as_str()
                    .unwrap()
                    .repeat(seg["repeat"].as_u64().unwrap() as usize),
            ),
        }
    }
    out
}

fn filed_result(rt: &OrbitRuntime, output: &Value) -> Value {
    let mut tasks = Vec::new();
    for entry in arr(output, "filed") {
        let d = task_desc(rt, entry["task_id"].as_str().unwrap());
        let sig = d
            .lines()
            .find(|l| l.starts_with("- Normalized error signature"))
            .unwrap();
        let (_, body) = d.split_once("## Failed-step log excerpt\n").unwrap();
        let sec = body.split("\n## ").next().unwrap();
        let (_, rest) = sec.split_once("```\n").unwrap();
        let (excerpt, after) = rest.split_once("\n```").unwrap();
        let key = sig.split_once('`').unwrap().1.rsplit_once('`').unwrap().0;
        let fb = sig.contains("step-name fallback");
        let note = after.trim_start().starts_with('_');
        tasks.push(json!({ "failure_key": entry["failure_key"], "signature": key, "step_fallback": fb, "excerpt": excerpt, "excerpt_has_note": note }));
    }
    let skipped: Vec<_> = arr(output, "skipped_existing")
        .iter()
        .map(|e| e["failure_key"].clone())
        .collect();
    json!({"filed_count": output["filed_count"], "tasks": tasks, "skipped_keys": skipped})
}

#[test]
#[cfg(unix)]
fn ci_failure_fixture_goldens() {
    if !sweep::isolated_with_fake_gh("ci_failure_fixture_goldens") {
        return;
    }
    let raw = std::fs::read_to_string(fixtures().join("fixtures.json")).unwrap();
    let cases: Vec<Value> = serde_json::from_str(&raw).unwrap();
    let update = std::env::var_os("ORBIT_UPDATE_LOG_GOLDENS").is_some();
    let exp_raw = || std::fs::read_to_string(fixtures().join("parsed.json")).unwrap();
    let expected: Value = if update {
        json!({})
    } else {
        serde_json::from_str(&exp_raw()).unwrap()
    };
    let mut progress = orbit_common::test_env::FixtureProgress::start("CI log goldens");
    progress.phase("cases", cases.len());
    let mut rendered = serde_json::Map::new();
    for case in cases {
        let actual = if case["collect"] == true {
            collected_golden(&case)
        } else {
            filed_golden(&case)
        };
        let name = case["name"].as_str().unwrap();
        if !update {
            assert_eq!(
                actual, expected[name],
                "CI log fixture {name}; regenerate with make goldens UPDATE=1"
            );
        }
        rendered.insert(name.to_string(), actual);
        progress.advance();
    }
    progress.finish();
    if update {
        let out = serde_json::to_string_pretty(&rendered).unwrap() + "\n";
        std::fs::write(fixtures().join("parsed.json"), out).unwrap();
    } else {
        let exp_len = expected.as_object().unwrap().len();
        assert_eq!(rendered.len(), exp_len, "all CI golden cases must run");
    }
}

#[test]
fn ci_failure_branch_routing_retains_owner_evidence_and_only_files_landing_checkouts() {
    if !isolated(
        "ci_failure_branch_routing_retains_owner_evidence_and_only_files_landing_checkouts",
    ) {
        return;
    }
    let (_root, rt) = bare_runtime();
    let owner = add_titled(&rt, "Sandbox implementation");
    let mut pr = pr_for(&owner.id, "error: sandbox directory escaped", 0, '3');
    pr["pr_number"] = json!(3140);
    let first = file(&rt, vec![pr.clone()]);
    assert_filed(&first, 0, "unmerged_pr");
    assert_eq!(first["pilot_candidate_count"], 0);
    assert_retained_branch_failure(&rt, &first, &owner.id, &pr);
    assert_eq!(task_status(&rt, &owner.id), owner.status);
    assert_eq!(
        file(&rt, vec![pr.clone()])["attributed"],
        first["attributed"]
    );
    assert_eq!(arts_len(&rt, &owner.id), 1);
    let out = branch_file(&rt, pr.clone());
    assert_eq!(out["attributed"], first["attributed"]);
    assert_filed(&out, 0, "branch_partition");
    let push = failure("error: independent landing regression", 1, &sha('4'));
    let out = file(&rt, vec![pr, push.clone()]);
    assert_filed(&out, 1, "push_still_files");
    assert_len(&out, "attributed", 1);
    let repeated = file(&rt, vec![push]);
    assert_filed(&repeated, 0, "push_repeat");
    assert_len(&repeated, "skipped_existing", 1);
    let routing = [
        (2, "pull_request", '5', 0),
        (3, "pull_request", '1', 1),
        (4, "merge_group", '1', 1),
        (5, "merge_group", '6', 0),
        (6, "push", '7', 0),
    ];
    for (idx, event, ch, exp) in routing {
        let mut f = failure(&format!("error: routing case {idx}"), idx, &sha(ch));
        f["event"] = json!(event);
        f["event_reported_head_sha"] = json!(sha('1'));
        if event == "merge_group" {
            f["head_branch"] = json!("gh-readonly-queue/agent-main/pr-3140");
            f["ref_kind"] = json!("other");
        }
        let o = file(&rt, vec![f]);
        let seen = o["filed_count"].as_u64().unwrap() + arr_len(&o, "skipped_existing") as u64;
        assert_eq!(seen, exp, "{event}/{ch}");
    }
    for (idx, ev) in [(8, "schedule"), (9, "workflow_dispatch")] {
        let mut f = failure(
            &format!("error: {ev} landing regression"),
            idx,
            &idx.to_string().repeat(40),
        );
        f["event"] = json!(ev);
        assert_filed(&file(&rt, vec![f]), 1, ev);
    }
    let mut red = failure("error: scheduled coverage regression", 10, &sha('1'));
    red["event"] = json!("schedule");
    red["workflow"] = json!("Coverage");
    let mut green = red.clone();
    green["run_id"] = json!(30);
    green["conclusion"] = json!("success");
    green["created_at"] = json!("2026-09-07T07:54:42Z");
    let ev = evidence_input(vec![red.clone()], json!({"latest_runs": [red, green]}));
    let out = file_input(&rt, ev).unwrap();
    assert_filed(&out, 0, "scheduled_superseded");
    assert_eq!(out["already_repaired"][0]["run_id"], 20, "{out}");
    let mut orphan = failure("error: unmatched branch failure", 7, &sha('8'));
    orphan["event"] = json!("pull_request");
    orphan["head_branch"] = json!("orbit/ORB-999999999-deadbeef");
    let ev = json!({ "ci_evidence": {"schema_version": 2, "collected": true, "current_failures": [orphan]} });
    let err = file_input(&rt, ev).unwrap_err();
    assert!(
        err.to_string().contains("task_branch_owner"),
        "missing owners stay retryable"
    );
}

#[test]
fn ci_failure_branch_routing_retains_evidence_for_configurable_task_prefixes() {
    if !isolated("ci_failure_branch_routing_retains_evidence_for_configurable_task_prefixes") {
        return;
    }
    for prefix in ["DE", "ORBA", "ORBX", "ABCDE"] {
        let (_root, g, ws) = orbit_dirs();
        seed_task_id_start(&g, Some(prefix), 1).unwrap();
        let rt = OrbitRuntime::from_roots(&g, &ws).unwrap();
        let owner = add_titled(&rt, "Task branch owner");
        assert_eq!(task_id_prefix(&owner.id), Some(prefix));
        let pr = pr_for(&owner.id, "error: task branch regression", 0, '3');
        let first = file(&rt, vec![pr.clone()]);
        assert_filed(&first, 0, prefix);
        assert_eq!(first["pilot_candidate_count"], 0);
        assert_empty(&first, "excluded_branch_failures", prefix);
        assert_len(&first, "attributed", 1);
        assert_retained_branch_failure(&rt, &first, &owner.id, &pr);
        assert_eq!(task_status(&rt, &owner.id), owner.status);
        let repeated = branch_file(&rt, pr);
        assert_eq!(repeated["attributed"], first["attributed"]);
        assert_empty(&repeated, "excluded_branch_failures", prefix);
        assert_eq!(arts_len(&rt, &owner.id), 1);
    }
    let (_root, rt) = configured_runtime(
        "[branch]\nname_template = 'feature/{{ext.jira}}-{{task_id}}-{{hash}}'\n",
    );
    let (sibling, owner) = (
        add_titled(&rt, "Mentioned task"),
        add_titled(&rt, "Actual delivery"),
    );
    let branch = format!("feature/X-{}-{}-0123abcd", sibling.id, owner.id);
    let pr = pull_request("error: task branch regression", 0, &branch, &sha('3'));
    let out = file(&rt, vec![pr.clone()]);
    assert_filed(&out, 0, "sibling_token");
    assert_len(&out, "attributed", 1);
    assert!(artifacts(&rt, &sibling.id).is_empty());
    assert_retained_branch_failure(&rt, &out, &owner.id, &pr);
}

fn protecting_branch_claim(rt: &OrbitRuntime, owner_id: &str) -> ExecutionClaim {
    let note = json!({ "machine_id": "follower-mac", "run_context": {"run_id": "jrun-follower-claim"}, "claim_id": format!("claim-{owner_id}"), "request_id": format!("request-{owner_id}") }).to_string();
    let u = TaskAutomationUpdate {
        expected_status: Some(TaskStatus::Proposed),
        status: Some(TaskStatus::InProgress),
        status_event: Some("pulled_by".into()),
        status_note: Some(note),
        ..Default::default()
    };
    orbit_core::test_support::EngineHost::new(rt)
        .apply_task_automation_update(owner_id, u)
        .unwrap();
    insert_protecting_claim(rt, owner_id)
}
/// Insert the follower's claim row, leaving the owner's status alone.
fn insert_protecting_claim(rt: &OrbitRuntime, id: &str) -> ExecutionClaim {
    let executed_on = ExecutionLocation {
        machine_id: "follower-mac".into(),
        machine_name: None,
    };
    let rc = AdmissionRunContext {
        run_id: "jrun-follower-claim".into(),
        job_name: "task_claimed_pr_pipeline".into(),
        machine_name: None,
    };
    let (cid, rid, res) = (
        format!("claim-{id}"),
        format!("request-{id}"),
        format!("reservation-{id}"),
    );
    let exp = "2099-01-01T00:00:00Z".into();
    let claim = ExecutionClaim {
        claim_id: cid,
        task_id: id.into(),
        request_id: rid,
        executed_on,
        run_context: rc,
        footprint: Vec::new(),
        reservation_id: res,
        reservation_expires_at: exp,
        phase: ExecutionClaimPhase::Claimed,
        repair: None,
    };
    let raw = serde_json::to_string(&claim).unwrap();
    insert_coordination_row(rt, "distributed-execution-claim-v1", &claim.claim_id, &raw);
    claim
}

/// A follower claim on the branch owner must not abort filing of landing failures [ORB-15199].
#[test]
fn ci_failure_claimed_branch_defers_attribution_until_the_claim_settles() {
    if !isolated("ci_failure_claimed_branch_defers_attribution_until_the_claim_settles") {
        return;
    }
    let (_root, rt) = bare_runtime();
    let owner = add_planned(&rt, "Follower-claimed branch owner");
    let claim = protecting_branch_claim(&rt, &owner.id);
    let crun = claim.run_context.run_id.as_str();
    let pr = pr_for(&owner.id, "error: sandbox directory escaped", 0, '3');
    let push = failure("error: independent landing regression", 1, &sha('4'));
    let assert_def = |o: &Value, filed_n: usize, skip_n: usize| {
        assert_filed(o, filed_n, "deferred");
        assert_len(o, "skipped_existing", skip_n);
        let hit = |e: &Value| {
            e["task_id"] == owner.id && e["run_id"] == pr["run_id"] && e["reason"] == "claimed"
        };
        assert!(arr(o, "deferred_attribution").iter().any(hit), "{o}");
        assert_eq!(
            o["audit"]["deferred_attribution"],
            o["deferred_attribution"]
        );
        assert!(artifacts(&rt, &owner.id).is_empty() && deferred_rows(&rt).len() == 1);
    };
    let only_pr = file(&rt, vec![pr.clone()]);
    assert_str(&only_pr, "outcome", "current_failures");
    assert_def(&only_pr, 0, 0);
    let rows = deferred_rows(&rt);
    assert!(
        !rows[0].applied
            && rows[0].claiming_run_id.as_deref() == Some(crun)
            && rows[0].task_id == owner.id
    );
    let p = TaskUpdateParams {
        comment: Some("operator note".into()),
        tags: Some(vec!["operator-note".into()]),
        ..Default::default()
    };
    let ref_err = rt
        .update_task_as_human(&owner.id, p, "human:fixture".into())
        .unwrap_err()
        .to_string();
    assert!(
        ref_err.contains("active execution claim requires a claim-scoped mutation"),
        "{ref_err}"
    );
    assert!(
        ref_err.contains(&owner.id) && ref_err.contains(crun),
        "{ref_err}"
    );
    let filed = file(&rt, vec![pr.clone(), push.clone()]);
    assert_str(&filed, "outcome", "current_failures");
    assert_ne!(filed_id(&filed, 0), owner.id);
    assert_def(&filed, 1, 0);
    assert_def(&file(&rt, vec![pr.clone(), push.clone()]), 0, 1);
    release(&rt, &claim, "release-orb-x");
    assert_eq!(task_status(&rt, &owner.id), TaskStatus::Backlog);
    let claims = rt.inspect_execution_claims().unwrap();
    let settled = claims
        .into_iter()
        .find(|i| i.claim.claim_id == claim.claim_id)
        .unwrap();
    assert_eq!(settled.claim.phase, ExecutionClaimPhase::Revoked);
    let arts = artifacts(&rt, &owner.id);
    assert!(arts.len() == 1 && arts[0].path.starts_with("ci-branch-observations/"));
    assert_eq!(art_json(&arts[0])["failure"], pr);
    let applied = deferred_rows(&rt);
    assert!(
        applied.len() == 1
            && applied[0].applied
            && applied[0].content.as_bytes() == arts[0].content
    );
    let after = file(&rt, vec![pr, push]);
    assert_len(&after, "deferred_attribution", 0);
    assert_str(&after["attributed"][0], "task_id", &owner.id);
    assert_filed(&after, 0, "after");
    assert_len(&after, "skipped_existing", 1);
    assert_eq!(arts_len(&rt, &owner.id), 1);
}

#[test]
fn ci_failure_deferred_receipts_settle_per_task_and_replay_without_growth() {
    if !isolated("ci_failure_deferred_receipts_settle_per_task_and_replay_without_growth") {
        return;
    }
    let (_root, rt) = bare_runtime();
    let (t1, t2, t3) = (
        add_planned(&rt, "First"),
        add_planned(&rt, "Second"),
        add_planned(&rt, "Unclaimed"),
    );
    let (c1, c2) = (
        protecting_branch_claim(&rt, &t1.id),
        protecting_branch_claim(&rt, &t2.id),
    );
    let runs = vec![
        pr_for(&t1.id, "error: escaped", 0, '3'),
        pr_for(&t2.id, "error: escaped", 1, '3'),
        pr_for(&t3.id, "error: escaped", 2, '3'),
    ];
    for _ in 0..2 {
        assert_len(&file(&rt, runs.clone()), "deferred_attribution", 2);
        assert_eq!(arts_len(&rt, &t3.id), 1);
        assert_eq!(
            deferred_rows(&rt).len(),
            2,
            "only deferred receipts own a row"
        );
    }
    release(&rt, &c1, "release-first");
    assert!(arts_len(&rt, &t1.id) == 1 && artifacts(&rt, &t2.id).is_empty());
    let rows1 = deferred_rows(&rt);
    assert_eq!(rows1.len(), 2);
    for r in &rows1 {
        assert_eq!(r.applied, r.task_id == t1.id, "{r:?}");
    }
    assert_len(&file(&rt, runs.clone()), "deferred_attribution", 1);
    assert!(
        arts_len(&rt, &t1.id) == 1
            && artifacts(&rt, &t2.id).is_empty()
            && deferred_rows(&rt).len() == 2
    );
    release(&rt, &c2, "release-second");
    assert!(arts_len(&rt, &t2.id) == 1 && arts_len(&rt, &t1.id) == 1);
    assert!(deferred_rows(&rt).iter().all(|r| r.applied));
    assert_len(&file(&rt, runs), "deferred_attribution", 0);
    assert_eq!(deferred_rows(&rt).len(), 2);
    for id in [&t1.id, &t2.id, &t3.id] {
        assert_eq!(arts_len(&rt, id), 1);
    }
}

/// Deterministically replay the former sweep ordering: lookup protecting claim, settle, then submit.
#[test]
fn ci_failure_branch_receipt_retains_after_settlement_precedes_insertion() {
    if !isolated("ci_failure_branch_receipt_retains_after_settlement_precedes_insertion") {
        return;
    }
    for queued in [false, true] {
        let (_root, rt) = bare_runtime();
        let owner = add_planned(&rt, "Branch owner settled before receipt insertion");
        let claim = protecting_branch_claim(&rt, &owner.id);
        let snap = rt.resolve_execution_claims().unwrap();
        let obs_claim = snap.iter().find(|i| i.claim.task_id == owner.id).unwrap();
        assert!(obs_claim.claim.phase.protects_footprint());
        let pr = pr_for(&owner.id, "error: receipt arrived after settlement", 0, '3');
        let content =
            json!({ "schema_version": 1, "kind": "task_branch_ci_failure", "failure": pr })
                .to_string();
        let path = format!(
            "ci-branch-observations/{}.json",
            sha256_hex(content.as_bytes())
        );
        assert!(rt.get_task_artifact(&owner.id, &path).unwrap().is_none());
        let crun = Some(obs_claim.claim.run_context.run_id.clone());
        let obs = DeferredBranchObservation {
            schema_version: 1,
            task_id: owner.id.clone(),
            run_id: pr["run_id"].clone(),
            job_id: pr["job_id"].clone(),
            artifact_path: path.clone(),
            content,
            claiming_run_id: crun,
            comment: None,
            applied: false,
        };
        release(&rt, &claim, "release-before-receipt");
        let reg = TaskRegistryStore::open(&task_registry_path(&rt.global_root())).unwrap();
        let db = orbit_store::Store::open(&rt.global_root().join("orbit.db")).unwrap();
        let backends = workspace_coordinated_backends(reg, rt.workspace_id().unwrap(), db).unwrap();
        let kind = DEFERRED_BRANCH_OBSERVATION_KIND;
        if queued {
            let rid = format!("{}:{}", owner.id, sha256_hex(obs.content.as_bytes()));
            insert_coordination_row(&rt, kind, &rid, &serde_json::to_string(&obs).unwrap());
        }
        let coord_rows = || backends.commit_boundary.coordination_rows(kind).unwrap();
        assert_eq!(coord_rows().len(), usize::from(queued));
        let svc = &backends.task.task;
        assert_eq!(
            svc.record_deferred_branch_observation(&obs).unwrap(),
            BranchObservationOutcome::Retained
        );
        let arts = artifacts(&rt, &owner.id);
        assert!(
            arts.len() == 1 && arts[0].path == path && arts[0].content == obs.content.as_bytes()
        );
        let rows = coord_rows();
        assert_eq!(rows.len(), usize::from(queued));
        if queued {
            let applied: DeferredBranchObservation =
                serde_json::from_str(&rows[0].payload_json).unwrap();
            assert!(applied.applied && applied.claiming_run_id.is_none());
        }
        let hist = history(&rt, &owner.id);
        assert_eq!(
            svc.record_deferred_branch_observation(&obs).unwrap(),
            BranchObservationOutcome::Retained
        );
        assert_eq!(arts_len(&rt, &owner.id), 1);
        assert_eq!(history(&rt, &owner.id), hist);
        assert_eq!(coord_rows(), rows);
        let out = file(&rt, vec![pr]);
        assert_str(&out["attributed"][0], "artifact", &path);
        assert_len(&out, "deferred_attribution", 0);
    }
}

#[test]
fn ci_failure_branch_write_failure_does_not_abort_other_observations_or_landing_filing() {
    if !isolated(
        "ci_failure_branch_write_failure_does_not_abort_other_observations_or_landing_filing",
    ) {
        return;
    }
    let (_root, rt) = bare_runtime();
    let owner = add_planned(&rt, "Branch owner whose queue write fails");
    protecting_branch_claim(&rt, &owner.id);
    let other = add_titled(&rt, "Independent branch owner");
    let sql = "CREATE TRIGGER reject_branch_observation BEFORE INSERT ON task_coordination_rows WHEN NEW.kind = 'ci-branch-observation-deferred-v1' AND json_extract(NEW.payload_json, '$.claiming_run_id') IS NOT NULL BEGIN SELECT RAISE(ABORT, 'fixture branch observation write failure'); END;";
    open_db(&rt).execute_batch(sql).unwrap();
    let pr = pr_for(&owner.id, "error: claimed branch failure", 0, '3');
    let br = format!("orbit/{}-abcdef12", other.id);
    let other_pr = pull_request("error: independent branch failure", 2, &br, &sha('3'));
    let push = failure("error: independent landing regression", 1, &sha('4'));
    let out = file(&rt, vec![pr, other_pr, push]);
    assert_filed(&out, 1, "write_failure");
    assert_len(&out, "attributed", 1);
    assert_str(&out["attributed"][0], "task_id", &other.id);
    assert_len(&out, "deferred_attribution", 0);
    let errs = arr(&out["audit"], "branch_observation_errors");
    assert_eq!(errs.len(), 1, "{out}");
    assert_str(&errs[0], "task_id", &owner.id);
    let msg = errs[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("fixture branch observation write failure"),
        "{out}"
    );
    assert!(artifacts(&rt, &owner.id).is_empty() && arts_len(&rt, &other.id) == 1);
}

/// The sweep routes each repair to a host that can reproduce it [ORB-14005].
#[test]
fn ci_failure_sweep_tags_repairs_with_the_failing_runner_os() {
    if !isolated("ci_failure_sweep_tags_repairs_with_the_failing_runner_os") {
        return;
    }
    let (root, g, ws) = orbit_dirs();
    let workflows = root.path().join("repo/.github/workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    let yml = "name: macOS CI\non: push\njobs:\n  sandbox:\n    name: Sandbox\n    runs-on: macos-14\n    steps:\n      - run: make test\n";
    std::fs::write(workflows.join("ci-macos.yml"), yml).unwrap();
    let rt = OrbitRuntime::from_roots(&g, &ws).unwrap();
    let checkout = sha('3');
    let cases = json!([ {"name": "macos", "labels": ["macos-latest"], "tag": "os:macos", "source": "job_labels"}, {"name": "ubuntu", "labels": ["ubuntu-24.04"], "tag": "os:linux", "source": "job_labels"}, {"name": "windows", "labels": ["windows-latest"], "source": "job_labels"}, {"name": "self-hosted", "labels": ["self-hosted", "gpu"], "source": "job_labels"}, {"name": "runs-on", "workflow": "macOS CI", "tag": "os:macos", "source": "workflow_runs_on"}, {"name": "unknown", "source": "unknown"} ]);
    let cases = cases.as_array().unwrap();
    let mut runs = Vec::new();
    for (idx, c) in cases.iter().enumerate() {
        let mut run = failure(
            &format!("error: {} runner regression", c["name"].as_str().unwrap()),
            idx,
            &checkout,
        );
        if !c["labels"].is_null() {
            run["failed_jobs"][0]["runner_labels"] = c["labels"].clone();
        }
        if !c["workflow"].is_null() {
            run["workflow"] = c["workflow"].clone();
            run = with_job(run, "Sandbox");
        }
        runs.push(run);
    }
    let mut input = evidence_input(runs, json!({"collected_at": "2026-10-04T08:00:00Z"}));
    input["max_tasks"] = json!(10);
    let out = file_input(&rt, input).expect("file CI failures");
    let filed = arr(&out, "filed");
    assert_eq!(filed.len(), cases.len(), "{out}");
    for (idx, c) in cases.iter().enumerate() {
        let name = &c["name"];
        let entry = filed
            .iter()
            .find(|e| e["run_ids"] == json!([10 + idx]))
            .unwrap_or_else(|| panic!("{name}"));
        let task = get_task(&rt, entry["task_id"].as_str().unwrap());
        let os_tags: Vec<_> = task.tags.iter().filter(|t| t.starts_with("os:")).collect();
        let exp: Vec<_> = c["tag"].as_str().into_iter().collect();
        assert_eq!(os_tags, exp, "{name}: {:?}", task.tags);
        assert_eq!(entry["runner_os"][0]["source"], c["source"], "{name}");
        assert!(task.description.contains("- Runner OS: "), "{name}");
    }
}

#[test]
fn ci_failure_sweep_uses_medium_pool_and_preserves_failure_key_tag() {
    if !isolated("ci_failure_sweep_uses_medium_pool_and_preserves_failure_key_tag") {
        return;
    }
    let cfg = "[workflow]\ndefault_crew = \"system\"\nmedium_complexity_crews = [\"fixture\"]\n[crews.fixture]\nprovider = \"codex\"\nmodel = \"fixture-model\"\nbackend = \"cli\"\n[crews.system]\nprovider = \"codex\"\nmodel = \"system-model\"\nbackend = \"cli\"\n";
    let (_root, rt) = configured_runtime(cfg);
    let out = file_one(&rt, "error: pool routing regression", 0, &sha('3'));
    let task = get_task(&rt, filed_id(&out, 0));
    assert_eq!(task.complexity, Some(TaskComplexity::Medium));
    assert_eq!(task.crew.as_deref(), Some("fixture"));
    assert!(
        task.tags
            .contains(&format!("ci-failure:{}", filed_key(&out, 0)))
    );
}

/// Commit `count` empty commits on `repo` and return their ids, oldest first.
fn commit_chain(repo: &Path, count: usize) -> Vec<String> {
    let git = |args: &[&str]| {
        let mut cmd = crate::git_test_support::command();
        let base = [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
        ];
        let out = cmd
            .args(base)
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("commit {i}"),
        ]);
        ids.push(git(&["rev-parse", "HEAD"]));
    }
    ids
}

/// Record the owner's verified landing of a pull follower's delivery.
fn record_owner_landing(rt: &OrbitRuntime, task_id: &str, landed: &str) {
    let note = json!({ "delivery": {"pr_number": 7392, "merged_at": "2026-11-01T01:20:00Z", "head_sha": sha('a'), "merge_commit": landed}, "pull_request": 7392, "candidate": sha('b') }).to_string();
    let entry = TaskHistoryEntry {
        at: chrono::Utc::now(),
        by: "owner-landing".into(),
        event: "landing_completed".into(),
        note: Some(note),
        from_status: None,
        to_status: Some(TaskStatus::Done),
        operation_id: None,
        decision: None,
        surface: None,
        caller: None,
    };
    let u = TaskAutomationUpdate {
        status: Some(TaskStatus::Done),
        append_history: vec![entry],
        ..Default::default()
    };
    orbit_core::test_support::EngineHost::new(rt)
        .apply_task_automation_update(task_id, u)
        .unwrap();
    let hist = history(rt, task_id);
    let ok = |e: &TaskHistoryEntry| {
        e.event == "landing_completed"
            && e.by == "owner-landing"
            && e.note.as_deref().is_some_and(|n| n.contains(landed))
    };
    assert!(hist.iter().any(ok), "{hist:?}");
    assert!(rt.observe_task_delivery(task_id, None).is_err());
}

/// Record a successful delivery run that committed and merged `task_id` at `landed`.
fn record_landing(rt: &OrbitRuntime, task_id: &str, base: &str, landed: &str) {
    let res = rt.paths().global_dir.join("resources");
    std::fs::create_dir_all(res.join("activities")).unwrap();
    std::fs::create_dir_all(res.join("jobs")).unwrap();
    for act in ["git_commit", "pr_complete"] {
        let src =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/activities/{act}.yaml"));
        std::fs::copy(src, res.join(format!("activities/{act}.yaml"))).unwrap();
    }
    let job_yaml = "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_delivery\nspec:\n  state: enabled\n  task_delivery: {}\n  steps:\n    - id: commit\n      target: activity:git_commit\n    - id: complete_pr\n      target: activity:pr_complete\n";
    std::fs::write(res.join("jobs/fixture_delivery.yaml"), job_yaml).unwrap();
    let tids = Some(json!({"task_ids": [task_id]}));
    let mut run = orbit_core::test_support::EngineHost::new(rt)
        .insert_job_run("fixture_delivery", 1, chrono::Utc::now(), tids, None)
        .unwrap();
    run.state = JobRunState::Success;
    rt.sqlite_store()
        .unwrap()
        .write(|tx| tx.job_run().put(&rt.workspace_id().unwrap(), &run, None))
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    let c_out = json!({ "phase": "commit", "decision": "performed", "committed": true, "commit_sha": landed, "base_sha": base, "job_run_id": run.run_id, "task_id": task_id });
    let m_out = json!({ "phase": "complete", "merge": {"merged": true, "pr_number": "3507", "landed_commit": landed} });
    for (idx, step_id, output) in [(0, "commit", c_out), (1, "complete_pr", m_out)] {
        state.record_step(idx, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output(step_id, output);
    }
    rt.write_run_state(&run.run_id, &state).unwrap();
}

/// A red run that completes after its repair landed is not filed again [ORB-14422] / [ORB-20664].
#[test]
fn ci_failure_predating_a_landed_repair_is_held_not_refiled() {
    if !isolated("ci_failure_predating_a_landed_repair_is_held_not_refiled") {
        return;
    }
    let p = "crates/orbit-cli/tests/workspace/init_human_crew.rs";
    let msg = "cannot find value `probe_timeout` in this scope";
    let comp = compiler_log("E0425", msg, p, 207, 9);
    let local = "error: landing stopped on its base is not repaired under the same task";
    let c1 = ("local_delivery", false, local, "Coverage", true);
    let c2 = (
        "follower_delivery",
        true,
        comp.as_str(),
        "macOS Platform",
        false,
    );
    for (label, follower, log, job, rewrite) in [c1, c2] {
        let (_root, rt, c) = operator_commits("", 4);
        let (first, late, landed, after) = (&c[0], &c[1], &c[2], &c[3]);
        let original = file_one(&rt, log, 0, first);
        assert_filed(&original, 1, label);
        let repair = filed_id(&original, 0).to_string();
        let repair_key = filed_key(&original, 0).to_string();
        if follower {
            let t = get_task(&rt, &repair);
            assert!(
                t.description.contains("- Compiler cause identity: `"),
                "{label}"
            );
            assert_eq!(t.status, TaskStatus::Proposed, "{label}");
            record_owner_landing(&rt, &repair, landed);
        } else {
            if rewrite {
                set_desc(&rt, &repair, "Operator replaced the brief.");
            }
            set_status(&rt, &repair, TaskStatus::Done);
            record_landing(&rt, &repair, late, landed);
        }
        assert_eq!(task_status(&rt, &repair), TaskStatus::Done, "{label}");
        let held = file(&rt, vec![with_job(failure(log, 1, late), job)]);
        assert_filed(&held, 0, label);
        assert_empty(&held, "skipped_existing", label);
        assert_empty(&held, "withheld", label);
        assert_pending(&held, &repair, landed, late, label);
        let ps = &held["pending_supersession"][0];
        if follower {
            assert_ne!(ps["failure_key"], repair_key.as_str(), "{label}");
        } else {
            assert_eq!(ps["run_ids"], json!([11]), "{label}");
            assert_eq!(
                held["audit"]["pending_supersession_run_ids"],
                json!([11]),
                "{label}"
            );
        }
        let reproduced = file(&rt, vec![with_job(failure(log, 2, after), job)]);
        assert_filed(&reproduced, 1, label);
        assert_empty(&reproduced, "pending_supersession", label);
    }
    let (_root, rt) = bare_runtime();
    let hold = json!({ "run_id": 37561651327u64, "workflow": "CI", "head_branch": "agent-main", "reason": "newer_descendant_run_in_flight", "pending_on": {"run_id": 37561800000u64, "status": "in_progress"} });
    let extra =
        json!({"outcome_hint": "no_current_failure", "pending_supersession": [hold.clone()]});
    let out = file_input(&rt, evidence_input(vec![], extra)).unwrap();
    assert_str(&out, "outcome", "no_current_failure");
    assert_eq!(out["pending_supersession"], json!([hold]));
    assert_eq!(
        out["audit"]["pending_supersession_run_ids"],
        json!([37561651327u64])
    );
}

/// Collection releases a red run from `pending_supersession` [ORB-14610].
#[test]
fn ci_failure_released_from_pending_supersession_is_filed_naming_the_pending_run() {
    if !isolated("ci_failure_released_from_pending_supersession_is_filed_naming_the_pending_run") {
        return;
    }
    let log = "build\tRun CI\t2026-10-07T15:10:00.0000000Z ##[group]Run cargo check --workspace\nbuild\tRun CI\t2026-10-07T15:10:00.0000000Z ##[endgroup]\nbuild\tRun CI\t2026-10-07T15:10:00.0000000Z error[E0425]: cannot find value `probe_timeout` in this scope\nbuild\tRun CI\t2026-10-07T15:10:00.0000000Z   --> crates/orbit-cmd/src/update/converge.rs:189:9\nbuild\tRun CI\t2026-10-07T15:10:00.0000000Z ##[error]Process completed with exit code 101.\n";
    let queued = json!({ "run_id": 37645190583u64, "status": "queued", "event": "push", "url": "https://github.com/acme/orbit/actions/runs/37645190583", "reported_head_sha": sha('9') });
    let previous = sha('5');
    let mut reproduced = failure(log, 1, &sha('6'));
    reproduced["reproduced_on"] = json!({ "run_id": 10, "url": "https://github.com/acme/orbit/actions/runs/10", "event_reported_head_sha": previous, "conclusion": "failure", "shared_cause": { "job": "build", "steps": ["Run CI"], "normalized_error_signature": "error[e<n>]: cannot find value `probe_timeout` in this scope" }, "pending_on": queued });
    let mut held = failure(log, 2, &sha('7'));
    held["held_past_window"] = json!({ "pending_on": queued, "pending_since": "2026-10-07T15:10:00+00:00", "window_minutes": 30 });
    let u10 = "https://github.com/acme/orbit/actions/runs/10";
    let sig = "error[e<n>]: cannot find value `probe_timeout` in this scope";
    let c1 = (reproduced, vec![u10, previous.as_str(), sig]);
    let c2 = (held, vec!["2026-10-07T15:10:00+00:00"]);
    for (released, expected) in [c1, c2] {
        let (_root, rt) = bare_runtime();
        let out = file(&rt, vec![released]);
        assert_filed(&out, 1, "released");
        assert_empty(&out, "pending_supersession", "released");
        let desc = task_desc(&rt, filed_id(&out, 0));
        let pending = sha('9');
        let u37 = "https://github.com/acme/orbit/actions/runs/37645190583";
        for named in expected.into_iter().chain([u37, pending.as_str()]) {
            assert!(desc.contains(named), "names {named}: {desc}");
        }
        assert!(desc.contains("- Compiler cause identity: `"), "{desc}");
    }
}

#[test]
fn ci_failure_sweep_strips_ansi_codes_and_extracts_context_files() {
    if !isolated("ci_failure_sweep_strips_ansi_codes_and_extracts_context_files") {
        return;
    }
    let (_root, rt) = bare_runtime();
    let cc = "build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31merror[E0425]: cannot find value `foo` in this scope\u{1b}[0m\nbuild\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[38;5;12m  --> \u{1b}[0m\u{1b}[1mcrates/orbit-core/src/runtime/mod.rs:31:16\u{1b}[0m\nbuild\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31m##[error]Process completed with exit code 101.\u{1b}[0m\n";
    let cp = "build\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31mthread 'crew_pools' panicked at \u{1b}[0mcrates/orbit-core/src/application/job/tests/crew_pools.rs:277:5:\u{1b}[0m\nbuild\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31massertion failed: `(left == right)`\u{1b}[0m\nbuild\tRun CI\t2026-10-09T12:00:00.0000000Z \u{1b}[31m##[error]Process completed with exit code 101.\u{1b}[0m\n";
    let (f1, f2) = (
        "file:crates/orbit-core/src/runtime/mod.rs",
        "file:crates/orbit-core/src/application/job/tests/crew_pools.rs",
    );
    for (idx, label, log, expected) in [(0, "compiler", cc, f1), (1, "panic", cp, f2)] {
        let ch = char::from_digit((idx + 2) as u32, 10).unwrap();
        let out = file_one(&rt, log, idx, &sha(ch));
        assert_filed(&out, 1, label);
        let task = get_task(&rt, filed_id(&out, 0));
        assert!(!task.description.contains('\u{1b}'), "{label}");
        assert!(!task.description.contains("[0m"), "{label}");
        assert_eq!(task.context_files, vec![expected.to_string()], "{label}");
    }
}

#[test]
fn ci_printed_test_names_and_locations_do_not_change_runtime_failure_identity() {
    if !isolated("ci_printed_test_names_and_locations_do_not_change_runtime_failure_identity") {
        return;
    }
    let emitted = "step.sh: line 8: scripts/ci-download-verified.sh: No such file or directory";
    let printed = format!(
        "##[group]Run verify\n  error[E0123]: printed compiler text\n   --> crates/printed/src/lib.rs:1:1\n  thread 'printed_test' panicked at crates/printed/src/lib.rs:1:1:\n  test printed_test ... FAILED\n##[endgroup]\n{emitted}\n##[error]Process completed with exit code 127.\n"
    );
    let (_root, rt, c, first, owner) = fixture_owner(&printed);
    let task = get_task(&rt, &owner);
    let nsig = "`step.sh: line <n>: scripts/ci-download-verified.sh: no such file or directory`";
    assert!(task.description.contains(nsig) && task.description.contains(emitted));
    assert!(!task.description.contains("printed_test"));
    assert!(
        task.context_files
            .iter()
            .all(|s| !s.contains("crates/printed"))
    );
    let a = printed
        .replace("printed_test", "another_printed_test")
        .replace("crates/printed", "crates/other");
    let b = a.replace(
        "  error[E0123]",
        "  key\tvalue\t##[endgroup]\n  error[E0123]",
    );
    let c_str = b.replace("step.sh: line 8:", "step.sh:\tline 8:\t");
    let later: String = c_str
        .lines()
        .map(|l| format!("2026-10-15T02:42:56Z {l}\n"))
        .collect();
    let second = file_one(&rt, &later, 1, &c[1]);
    assert_filed(&second, 0, "printed");
    assert_eq!(
        second["skipped_existing"][0]["failure_key"],
        first["filed"][0]["failure_key"]
    );
}

#[test]
fn ci_filing_at_workspace_scale_files_once_and_repeats_without_duplicates() {
    let (_root, rt) = bare_runtime();
    for i in 0..300 {
        let task = add_titled(&rt, &format!("Unrelated backlog task {i}"));
        if i % 25 == 0 {
            patch_task(&rt, &task.id, |p| {
                p.comment = Some(format!("Unrelated progress note {i}."))
            });
        }
    }
    let co = sha('b');
    let mk = |i| {
        failure(
            &format!("error: scale probe failure {i} in subsystem{i}"),
            i,
            &co,
        )
    };
    let runs: Vec<Value> = (0..3).map(mk).collect();
    let first = file(&rt, runs.clone());
    assert_eq!(first["clusters"], 3, "{first}");
    assert_filed(&first, 3, "scale_first");
    let repeated = file(&rt, runs);
    assert_filed(&repeated, 0, "scale_repeat");
    assert_len(&repeated, "skipped_existing", 3);
    let fids: Vec<_> = arr(&first, "filed")
        .iter()
        .filter_map(|e| e["task_id"].as_str())
        .collect();
    for e in arr(&repeated, "skipped_existing") {
        assert!(fids.contains(&e["task_id"].as_str().unwrap()), "{repeated}");
    }
}
