//! [ORB-14634] A forge outage at a claimed leaf's push holds its claim.
//!
//! The follower runs the shipped `task_claimed_pr_pipeline` from its
//! checkpoints after `validate`: the candidate is committed, synchronized,
//! reviewed and validated, so only `push` and `pr_open` execute, on the
//! claim's bound runtime. A `git` ahead of the real one on `PATH` refuses the
//! leaf's pushes the way GitHub did during the 2026-10-07 outage, more times
//! than the push's backoff budget allows, and then accepts. Generic resume
//! cannot continue a claimed leaf, so the leaf keeps its claim and retries
//! inside the push's window: the same head reaches `origin` and `pr_open`
//! opens its pull request, while the owner still holds the claim. Neither the
//! implementer nor the before-PR reviewer runs again — this host has no
//! provider CLI, so either would fail the run.

use super::claimed_review::ToOwner;
use super::*;
use orbit_engine::activity_job::{V2ActivityCatalog, load_activity_asset, load_job_asset};
use orbit_engine::{
    V2AuditWriter, execute_job_with_resume, resolve_job_catalog_refs_for_execution,
};
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::activity_job::JobV2StepBody;
use orbit_types::workflow::{JobRunState, PipelineState};

const ORIGIN_URL: &str = "https://github.com/owner/repository.git";
const PULL_REQUEST_URL: &str = "https://github.com/owner/repository/pull/7";

/// What GitHub printed for every push during the outage.
const INTERNAL_SERVER_ERROR: &str = "remote: Internal Server Error\n\
 ! [remote rejected]     candidate -> candidate (Internal Server Error)\n\
error: failed to push some refs to 'https://github.com/owner/repository.git'";

/// The shipped claimed PR leaf with every activity resolved.
fn claimed_pipeline() -> orbit_types::workflow::activity_job::JobV2 {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut catalog = V2ActivityCatalog::new();
    for entry in std::fs::read_dir(assets.join("activities")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "yaml") {
            let asset = load_activity_asset(&std::fs::read_to_string(path).unwrap()).unwrap();
            catalog.insert(asset.name, asset.spec);
        }
    }
    let mut job = load_job_asset(
        &std::fs::read_to_string(assets.join("jobs").join(format!("{LEAF_JOB}.yaml"))).unwrap(),
    )
    .unwrap()
    .spec;
    resolve_job_catalog_refs_for_execution(&mut job, &catalog).unwrap();
    job
}

fn step_index(job: &orbit_types::workflow::activity_job::JobV2, id: &str) -> usize {
    job.steps
        .iter()
        .position(|step| step.id == id)
        .unwrap_or_else(|| panic!("the claimed leaf has a `{id}` step"))
}

fn executable(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A `git` and a `gh` in `HOME/bin`, which leads this child's `PATH`: `git`
/// refuses the next pushes counted in `HOME/refusals` and hands every command
/// to the real Git, and `gh` answers as GitHub does for a branch with no pull
/// request yet. Returns the file counting pushes.
fn forge_substitutes() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let real_git = String::from_utf8(
        std::process::Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    assert!(!real_git.starts_with(&*bin.to_string_lossy()), "{real_git}");
    let state = home.display().to_string();
    std::fs::write(home.join("refusal"), INTERNAL_SERVER_ERROR).unwrap();
    std::fs::write(home.join("refusals"), "0").unwrap();
    std::fs::write(home.join("pushes"), "0").unwrap();
    executable(
        &bin.join("git"),
        &format!(
            r#"#!/bin/sh
command=
skip=
for argument in "$@"; do
  if [ -n "$skip" ]; then skip=; continue; fi
  case "$argument" in -c|-C) skip=1; continue;; esac
  command=$argument
  break
done
if [ "$command" = push ]; then
  count=$(($(cat '{state}/pushes') + 1))
  printf '%s\n' "$count" > '{state}/pushes'
  if [ "$count" -le "$(cat '{state}/refusals')" ]; then
    cat '{state}/refusal' >&2
    exit 1
  fi
fi
exec '{real_git}' "$@"
"#
        ),
    );
    executable(
        &bin.join("gh"),
        &format!(
            r#"#!/bin/sh
case "$2" in
  list) echo '[]' ;;
  create) echo '{PULL_REQUEST_URL}' ;;
  view) echo '{{"number": 7, "url": "{PULL_REQUEST_URL}", "title": "", "body": "", "headRefName": "", "files": [], "commits": []}}' ;;
  *) exit 1 ;;
esac
"#
        ),
    );
    home.join("pushes")
}

fn count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn a_forge_outage_holds_the_claim_and_the_same_head_reaches_pr_open() {
    if !isolated(
        module_path!(),
        "a_forge_outage_holds_the_claim_and_the_same_head_reaches_pr_open",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let repo = &pair.owner_repo;
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    git(repo, &["remote", "add", "origin", ORIGIN_URL]);
    publish_origin(repo);
    let base = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    let bare = repo.with_file_name("origin.git");

    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), task);

    // The follower's own clone, with the leaf's committed candidate.
    let follower = &pair.follower_repo;
    let branch = format!("orbit/{task}");
    git(follower, &["init", "-q", "-b", "main"]);
    git(follower, &["remote", "add", "origin", ORIGIN_URL]);
    git(
        follower,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.display()),
            ORIGIN_URL,
        ],
    );
    git(follower, &["fetch", "-q", "origin", "main"]);
    git(follower, &["reset", "-q", "--hard", "origin/main"]);
    git(follower, &["checkout", "-q", "-b", &branch]);
    std::fs::write(follower.join("candidate.txt"), "the reviewed candidate\n").unwrap();
    git(follower, &["add", "candidate.txt"]);
    git(follower, &["commit", "-q", "-m", "candidate"]);
    let head = git(follower, &["rev-parse", "HEAD"]).trim().to_string();

    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.as_ref().unwrap();
    let bound = OrbitRuntime::from_roots(&pair.follower.global_root(), &follower.join(".orbit"))
        .unwrap()
        .with_automation_machine_identity(Some(FOLLOWER.into()))
        .with_coordination_write_owner(Some(OWNER.into()))
        .with_drain_owner_transport(pair.wire.clone())
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: task.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: leaf.clone(),
            },
            Arc::new(ToOwner(pair.wire.owner.clone())),
        )
        .unwrap();

    // The leaf's checkpoints through `validate`, as its pipeline state holds
    // them after review and validation passed on `head`.
    let mut job = claimed_pipeline();
    let push = step_index(&job, "push");
    let pr_open = step_index(&job, "pr_open");
    job.steps.truncate(pr_open + 1);
    // Keep the shipped budget and window; only shorten the waits.
    let JobV2StepBody::Target(target) = &mut job.steps[push].body else {
        panic!("`push` targets an activity");
    };
    let retry = target
        .default_input
        .as_mut()
        .and_then(|input| input.get_mut("forge_retry"))
        .expect("the claimed push declares its forge retry");
    retry["initial_backoff_ms"] = json!(20);
    retry["backoff_cap_ms"] = json!(40);
    let budget = retry["max_attempts"].as_u64().expect("a budget") as usize;

    let implementation = json!({
        "execution_summary": "Outcome: success\nImplemented and validated the candidate."
    });
    let synced = json!({
        "head": branch, "head_sha": head, "head_sha_before": head, "rewritten": false,
        "base": "main", "base_ref": "origin/main", "base_sha": base, "remote_sha_before": null,
    });
    let checkpoints = [
        (
            "worktree",
            json!({
                "job_run_id": leaf, "workspace_path": follower,
                "base_sha": base, "base_ref": "origin/main",
            }),
        ),
        ("resume_candidate", json!({})),
        ("implement_bundle", json!({})),
        (
            "commit",
            json!({"commit_sha": head, "skipped_no_diff_expected": false}),
        ),
        ("prepare_branch", synced.clone()),
        ("sync_base", synced),
        ("review_gate_admit", json!({"applies": true})),
        ("review", json!({"verdict": "approve"})),
        (
            "review_gate_settle",
            json!({
                "applies": true, "reviewed_head_sha": head, "reviewed_base_sha": base,
                "review_fixes": null, "handoff_evidence": null,
            }),
        ),
        ("validate", json!({"decision": "passed"})),
    ];
    let input = json!({"task_ids": [task], "base_sync": "remote"});
    let mut resume = PipelineState::new(leaf.clone(), LEAF_JOB.into(), input.clone());
    for (id, output) in checkpoints {
        resume.record_step(
            step_index(&job, id) as u32,
            JobRunState::Success,
            Some(output),
            None,
        );
    }
    resume.compound_outputs.insert(
        step_index(&job, "implement_bundle") as u32,
        [("implement_one".into(), implementation)]
            .into_iter()
            .collect(),
    );

    // The forge refuses every push past the budget, then accepts.
    let pushes = forge_substitutes();
    let home = pushes.parent().unwrap().to_path_buf();
    std::fs::write(home.join("refusals"), (budget + 2).to_string()).unwrap();

    let audit = V2AuditWriter::with_disk_sinks(
        &follower.join(".orbit/tmp/pipeline-audit"),
        bound.v2_audit_store().unwrap(),
        bound.workspace_id().unwrap(),
        &leaf,
        "forge-hold-fixture",
        Some(follower),
    )
    .unwrap();
    let outcome =
        execute_job_with_resume(&job, input, &leaf, audit, &bound, Some(&resume)).unwrap();

    assert!(outcome.success, "{outcome:#?}");
    assert_eq!(outcome.forge_hold, None, "the leaf never held");
    assert_eq!(
        count(&pushes),
        budget + 3,
        "every refusal past the budget is retried"
    );
    let pushed = &outcome.pipeline["push"];
    assert_eq!(pushed["local_sha"], head.as_str(), "{pushed}");
    assert_eq!(pushed["push_attempts"], budget + 3, "{pushed}");
    assert_eq!(
        git(&bare, &["rev-parse", &format!("refs/heads/{branch}")]).trim(),
        head,
        "origin holds the reviewed head"
    );
    let opened = &outcome.pipeline["pr_open"];
    assert_eq!(opened["pr_number"], "7", "{opened}");
    assert_eq!(opened["head"], branch.as_str(), "{opened}");

    assert!(
        pair.admission(&leaf).settlement.is_none(),
        "the leaf settled nothing while it retried"
    );
    let claims = pair.owner_claims();
    assert_eq!(claims.len(), 1, "{claims:#?}");
    assert_ne!(
        claims[0]["claim"]["phase"], "revoked",
        "the owner still holds the claim: {claims:#?}"
    );
    assert_eq!(pair.owner_status(&task), "in-progress");
}
