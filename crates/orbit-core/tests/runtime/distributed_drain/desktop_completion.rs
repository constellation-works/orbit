//! Desktop completion of a follower's delivery on the owner [ORB-14175].
//!
//! The follower's leaf runs and hands off pull request #42; the owner holds
//! the claim and the accepted handoff but never the leaf's run, which lives in
//! the follower's job store. Completion reads the owner-held evidence for that
//! exact host and run, takes the pull request's identity from the handoff (the
//! task carries no PR reference), and reads it from the provider by number.
//!
//! A pull request that merged at a head other than the handed-off candidate
//! completes only on an accepted review reconciliation of exactly that head.
//! The operator's public `orbit.task.reconcile_review` submission admits a
//! run of the shipped reconciliation job: it runs the owner's required
//! commands at the merged head, a read-only reviewer on the configured crew
//! inspects it, and the desktop consumer reads the settled record.

use super::*;

use orbit_core::application::review::ExpectedCandidate;
use orbit_types::desktop::{
    DesktopCriterionOutcome, DesktopReviewDecision, DesktopReviewVerdict, DesktopTaskOperation,
    DesktopTaskRequest,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{ExecutorSandboxKind, REVIEW_RECONCILIATION_JOB, ReviewReconciliation};

const PR_URL: &str = "https://github.com/owner/repository/pull/42";
const RECONCILE: &str = "orbit.task.reconcile_review";

fn session(capability: McpCapability) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: BTreeSet::from([capability]),
        ..ToolSessionContext::default()
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

fn executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A stand-in `gh` that answers only the exact view of pull request #42, from
/// `$HOME/pr.json`. A listing, or any other pull request, fails, and so does
/// every read while `$HOME/pr.fail` holds a provider error.
fn provider_answers_only_pr_42() {
    std::fs::create_dir_all(home().join("bin")).unwrap();
    let gh = home().join("bin/gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\n[ \"$1 $2 $3\" = \"pr view 42\" ] || { echo \"unexpected: gh $*\" >&2; exit 1; }\n\
         if [ -f \"$HOME/pr.fail\" ]; then cat \"$HOME/pr.fail\" >&2; exit 1; fi\n\
         exec cat \"$HOME/pr.json\"\n",
    )
    .unwrap();
    executable(&gh);
}

/// The provider's current answer for pull request #42.
fn pull_request_is(state: &str, head: &str, base: &str, merge_commit: Option<&str>) {
    std::fs::write(
        home().join("pr.json"),
        json!({
            "number": 42,
            "state": state,
            "mergedAt": if state == "MERGED" { json!("2026-10-05T06:50:00Z") } else { Value::Null },
            "headRefName": "orbit/fixture",
            "headRefOid": head,
            "baseRefName": base,
            "mergeCommit": merge_commit.map(|oid| json!({"oid": oid})),
            "url": PR_URL,
        })
        .to_string(),
    )
    .unwrap();
}

fn verdict(task: &Value, head: &str, run: &str) -> DesktopReviewVerdict {
    let criteria = task["acceptance_criteria"]
        .as_array()
        .unwrap()
        .iter()
        .map(|criterion| DesktopCriterionOutcome {
            criterion: criterion.as_str().unwrap().into(),
            met: true,
            evidence: vec![PR_URL.into()],
        })
        .collect();
    DesktopReviewVerdict {
        decision: DesktopReviewDecision::Accept,
        rationale: "The merged pull request delivers the criteria.".into(),
        criteria,
        evidence: vec!["execution_summary".into(), PR_URL.into()],
        expected_run_id: Some(run.into()),
        expected_head: Some(head.into()),
    }
}

fn review(
    id: &str,
    revision: &str,
    verdict: DesktopReviewVerdict,
    request: &str,
) -> DesktopTaskRequest {
    DesktopTaskRequest {
        request_id: request.into(),
        operation: DesktopTaskOperation::Review {
            id: id.into(),
            expected_revision: revision.into(),
            verdict,
            complete: true,
        },
    }
}

/// The ordinary transitions an operator takes from `blocked` back to review.
fn back_to_review(owner: &OrbitRuntime, task: &str, summary: Option<&str>) {
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "status": "in-progress", "model": "codex"}),
        )
        .expect("resume");
    let mut update = json!({"id": task, "status": "review", "model": "codex"});
    if let Some(summary) = summary {
        update["execution_summary"] = json!(summary);
    }
    owner
        .run_tool("orbit.task.update", update)
        .expect("back to review");
}

fn rev(repo: &Path, spec: &str) -> String {
    git(repo, &["rev-parse", spec]).trim().to_string()
}

/// How pull request #42 lands on the landing branch.
#[derive(Clone, Copy)]
enum Landing {
    /// A merge commit whose second parent is the head.
    Merge,
    /// One squash commit that keeps none of the head's history.
    Squash,
}

/// A follower delivery the owner accepted as pull request #42, whose owner
/// checkout holds the head that pull request merged at: one commit on the
/// handed-off branch, landed on the landing branch as `merge` (a merge
/// commit unless the fixture squashes it). The owner requires `commands`,
/// reviews on its `sol` crew, and has the shipped reconciliation job seeded.
struct Delivery {
    pair: Pair,
    owner: OrbitRuntime,
    repo: PathBuf,
    task: String,
    leaf: String,
    landing: String,
    claim_id: String,
    handoff_id: String,
    head: String,
    merge: String,
}

/// Owner configuration requiring `commands`, reviewing on `review_crew`, with
/// each of `crews` defined on the stand-in provider.
fn owner_config(commands: &[&str], review_crew: &str, crews: &[&str]) -> String {
    let mut config = format!(
        "[workflow]\ndefault_crew = \"{}\"\nrequired_validation_commands = {}\n\n\
         [operation]\nreview_crew = \"{review_crew}\"\n",
        crews[0],
        serde_json::to_string(commands).unwrap()
    );
    for crew in crews {
        config.push_str(&format!(
            "\n[crews.{crew}]\nprovider = \"codex\"\nmodel = \"fixture-{crew}\"\n"
        ));
    }
    config
}

impl Delivery {
    fn handed_off(commands: &[&str]) -> Self {
        Self::landed(commands, Landing::Merge, None)
    }

    /// The delivery landed as `landing`; `pre_merge` is one file a commit on
    /// the landing branch adds after the head branched and before it lands.
    fn landed(commands: &[&str], landing_kind: Landing, pre_merge: Option<(&str, &str)>) -> Self {
        provider_answers_only_pr_42();
        let config = owner_config(commands, "sol", &["sol"]);
        let pair = Pair::with_owner_config(&config, &[None]);
        *pair.wire.accept_handoffs.lock().unwrap() = true;
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        let task = pair.claimed_task(&leaf);
        pair.leaf_hands_off(&leaf);
        let settled = pair.pass(&drain);
        assert!(settled["error"].is_null(), "{settled}");
        assert_eq!(pair.owner_status(&task), "review");
        let claim = pair.wire.owner.distributed_claim_console().unwrap()["claims"][0].clone();
        let landing = claim["handoff"]["candidate"]["landing_branch"]
            .as_str()
            .unwrap()
            .to_string();

        let repo = pair.owner_repo.clone();
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        git(&repo, &["init", "-q", "-b", &landing]);
        git(&repo, &["add", ".gitignore", "src"]);
        git(&repo, &["commit", "-q", "-m", "Base"]);
        let branch = format!("orbit/{task}");
        git(&repo, &["checkout", "-q", "-b", &branch]);
        std::fs::write(repo.join("src/f0.rs"), "fn work() { fixed_by_hand() }\n").unwrap();
        git(
            &repo,
            &["commit", "-q", "-am", "Fix the handed-off change by hand"],
        );
        let head = rev(&repo, "HEAD");
        git(&repo, &["checkout", "-q", &landing]);
        if let Some((path, content)) = pre_merge {
            std::fs::write(repo.join(path), content).unwrap();
            git(&repo, &["add", path]);
            git(
                &repo,
                &[
                    "commit",
                    "-q",
                    "-m",
                    "Fix the landing branch before #42 lands",
                ],
            );
        }
        match landing_kind {
            Landing::Merge => git(
                &repo,
                &[
                    "merge",
                    "-q",
                    "--no-ff",
                    "-m",
                    "Merge pull request #42",
                    &branch,
                ],
            ),
            Landing::Squash => {
                git(&repo, &["merge", "-q", "--squash", &branch]);
                git(
                    &repo,
                    &["commit", "-q", "-m", "Pull request #42 (squashed)"],
                )
            }
        };
        let merge = rev(&repo, "HEAD");

        orbit_core::bootstrap::init::init_workspace_at_root(
            &pair.wire.owner.global_root(),
            orbit_core::bootstrap::init::InitOptions {
                global_only: true,
                refresh_defaults: true,
                ..Default::default()
            },
        )
        .expect("seed the shipped reconciliation job and activities");
        let owner = calm_host(
            OrbitRuntime::from_roots(&pair.wire.owner.global_root(), &repo.join(".orbit"))
                .unwrap()
                .with_automation_machine_identity(Some(OWNER.into())),
        );
        // The real dispatcher runs the reviewer on the configured crew; only
        // the provider's answer is fixed: the report in `review.json`, naming
        // the reconciliation its prompt carries.
        let provider = pair._root.path().join("codex");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nrid=$({{ printf '%s\\n' \"$*\"; cat; }} | grep -o 'rrc-[0-9a-f]\\{{20\\}}' | head -n 1)\n\
                 sed \"s/@RID@/$rid/\" '{}'\n",
                pair._root.path().join("review.json").display()
            ),
        )
        .unwrap();
        executable(&provider);
        follower_cli(&owner, "codex", "sh");
        let mut executor = owner.get_executor_def("codex").unwrap().unwrap();
        executor.command = Some(provider.to_string_lossy().to_string());
        executor.sandbox = Some(ExecutorSandboxKind::Off);
        owner.upsert_executor_def(&executor).unwrap();
        // Each submitted run executes in-process through `Self::execute`.
        // The substitute child test stands in for the detached worker until
        // then, so the supervisor
        // never sees a worker that exited before its run started.
        let started = pair._root.path().join("started-{run_id}");
        crate::worker_fixture::install(&started, "created");

        Self {
            claim_id: claim["claim_id"].as_str().unwrap().to_string(),
            handoff_id: claim["handoff"]["handoff_id"].as_str().unwrap().to_string(),
            pair,
            owner,
            repo,
            task,
            leaf,
            landing,
            head,
            merge,
        }
    }

    /// The operator's recovery: the owner's landing stopped, the PR was
    /// merged by hand, the handoff revoked and the claim recovered, and the
    /// task returned to review.
    fn recover(&self) {
        let expected = ExpectedCandidate {
            candidate_commit: "a".repeat(40),
            base_commit: "c".repeat(40),
        };
        self.owner
            .revoke_handoff_as_operator(
                &self.handoff_id,
                &expected,
                "operator",
                "merged by hand",
                "revoke",
            )
            .expect("revoke");
        self.owner
            .recover_claim_as_operator(
                &self.claim_id,
                "handed_off",
                TaskStatus::Blocked,
                "operator",
                "landing stopped on a conflict; merged by hand",
                "recover",
            )
            .expect("recover");
        back_to_review(&self.owner, &self.task, None);
    }

    /// Retain the observed protocol-7 shape for a stopped legacy handoff.
    /// The accepted handoff is schema 1 with before-PR review disabled and
    /// the captured validation commands; its admission has no review
    /// snapshot. Rewriting only the isolated fixture's old caller schema
    /// exercises the current public read path without re-admitting the claim.
    fn retain_protocol7_handoff(&self, command: &str) {
        let workspace_id = self.owner.workspace_id().unwrap();
        let database = self.owner.global_root().join("orbit.db");
        let connection = rusqlite::Connection::open(database).unwrap();
        let accepted_raw: String = connection
            .query_row(
                "SELECT payload_json FROM task_coordination_rows WHERE workspace_id=?1 AND kind=?2 AND row_id=?3",
                rusqlite::params![workspace_id, "distributed-handoff-v1", self.claim_id],
                |row| row.get(0),
            )
            .unwrap();
        let accepted: Value = serde_json::from_str(&accepted_raw).unwrap();
        assert_eq!(accepted["handoff"]["schema_version"], 1);
        assert_eq!(accepted["handoff"]["review"]["policy"], "none");
        assert_eq!(accepted["handoff"]["review"]["disposition"], "not_required");
        assert_eq!(accepted["required_commands"], json!([command]));

        let rows = {
            let mut statement = connection
                .prepare(
                    "SELECT row_id, payload_json FROM task_coordination_rows WHERE workspace_id=?1 AND kind=?2",
                )
                .unwrap();
            statement
                .query_map(
                    rusqlite::params![workspace_id, "distributed-admission-receipt-v1"],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let (row_id, payload) = rows
            .into_iter()
            .find(|(_, payload)| {
                serde_json::from_str::<Value>(payload)
                    .is_ok_and(|stored| stored["receipt"]["claim"]["claim_id"] == self.claim_id)
            })
            .expect("the retained admission receipt for this claim");
        let mut stored: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(stored["state"], "full");
        assert_eq!(
            stored["receipt"]["request"]["caller_schema"],
            orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA
        );
        assert_eq!(stored["receipt"]["request"]["ship"]["before_pr"], false);
        assert!(stored["receipt"]["request"]["ship"].get("review").is_none());
        stored["receipt"]["request"]["caller_schema"] = json!(7);
        stored["receipt"]["request"]["caller_before_pr"] = json!(false);
        let payload = serde_json::to_string(&stored).unwrap();
        connection
            .execute(
                "UPDATE task_coordination_rows SET payload_json=?1 WHERE workspace_id=?2 AND kind=?3 AND row_id=?4",
                rusqlite::params![payload, workspace_id, "distributed-admission-receipt-v1", row_id],
            )
            .unwrap();
    }

    /// The provider reports PR #42 merged at the delivery's head.
    fn merged(&self) {
        pull_request_is("MERGED", &self.head, &self.landing, Some(&self.merge));
    }

    /// The reviewer's next answer: a report with `verdict` and `findings`.
    fn reviewer_reports(&self, verdict: &str, findings: Value) {
        let envelope = json!({
            "schemaVersion": 1,
            "status": "success",
            "result": {"report": {
                "schema_version": 1,
                "attempt_id": "@RID@",
                "verdict": verdict,
                "summary": "The merged head was reviewed against the task.",
                "findings": findings,
            }},
            "error": null,
        });
        std::fs::write(
            self.pair._root.path().join("review.json"),
            format!("{envelope}\n"),
        )
        .unwrap();
    }

    /// Call the reconciliation tool as an operator session.
    fn reconcile(&self, input: Value) -> Result<Value, OrbitError> {
        self.reconcile_on(&self.owner, input)
    }

    fn submit(&self, key: &str) -> Value {
        self.reconcile(json!({"action": "submit", "request_key": key}))
            .expect("operator submission")
    }

    /// The owner after an operator edits its configuration: a fresh runtime
    /// over the same roots reads the new file.
    fn reconfigured(&self, config: &str) -> OrbitRuntime {
        std::fs::write(self.repo.join(".orbit/config.toml"), config).unwrap();
        calm_host(
            OrbitRuntime::from_roots(
                &self.pair.wire.owner.global_root(),
                &self.repo.join(".orbit"),
            )
            .unwrap()
            .with_automation_machine_identity(Some(OWNER.into())),
        )
    }

    /// Call the reconciliation tool as an operator session of `owner`.
    fn reconcile_on(&self, owner: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
        let mut input = input;
        input["id"] = json!(self.task);
        owner.run_tool_with_context_and_role(
            RECONCILE,
            input,
            Role::Admin,
            ToolContext {
                session_context: session(McpCapability::Operator),
                ..ToolContext::default()
            },
        )
    }

    /// Execute a submitted run the way its detached worker would, then let
    /// its substitute exit.
    fn execute(&self, run_id: &str) {
        self.execute_on(&self.owner, run_id);
    }

    fn execute_on(&self, owner: &OrbitRuntime, run_id: &str) {
        let _ = owner.execute_pipeline_run_worker(run_id);
        std::fs::write(self.pair._root.path().join(format!("started-{run_id}")), "").unwrap();
    }

    /// Execute an admitted run, then read its reconciliation.
    fn run(&self, submitted: &Value) -> Value {
        self.execute(submitted["run_id"].as_str().expect("an admitted run"));
        self.status(submitted["reconciliation_id"].as_str().unwrap())
    }

    fn status(&self, reconciliation: &str) -> Value {
        let status = self
            .reconcile(json!({"action": "status", "reconciliation_id": reconciliation}))
            .expect("status");
        status["reconciliations"][0].clone()
    }

    fn snapshot(&self, capability: McpCapability) -> orbit_types::desktop::DesktopTaskSnapshot {
        self.owner
            .desktop_task_snapshot(&self.task, &session(capability))
            .unwrap()
    }

    fn completion_refusal(&self) -> String {
        let snapshot = self.snapshot(McpCapability::Operator);
        assert!(!snapshot.actions.complete.enabled, "{:?}", snapshot.actions);
        snapshot.actions.complete.reason.unwrap_or_default()
    }

    fn completes(&self) -> bool {
        self.snapshot(McpCapability::Operator)
            .actions
            .complete
            .enabled
    }

    /// The operator's baseline disposition of `command` on `reconciliation`.
    fn dispose(
        &self,
        reconciliation: &str,
        command: &str,
        remediation: &str,
    ) -> Result<Value, OrbitError> {
        self.reconcile(json!({
            "action": "accept_baseline",
            "reconciliation_id": reconciliation,
            "command": command,
            "remediation_commit": remediation,
            "reason": "the landing branch already failed this check",
        }))
    }

    /// Land one commit on the landing branch writing `content` to `path`.
    fn land(&self, path: &str, content: &str, message: &str) -> String {
        std::fs::write(self.repo.join(path), content).unwrap();
        git(&self.repo, &["add", path]);
        git(&self.repo, &["commit", "-q", "-m", message]);
        rev(&self.repo, "HEAD")
    }

    fn stored(&self, reconciliation: &str) -> ReviewReconciliation {
        self.owner
            .review_store()
            .unwrap()
            .review_reconciliation(&self.owner.workspace_id().unwrap(), reconciliation)
            .unwrap()
            .expect("retained reconciliation")
    }

    /// Rewrite the stored record into the shape a schema-3 owner persisted:
    /// no landed commit in the binding or the disposition evidence, with the
    /// binding digest that owner computed. The store's own update path
    /// refuses this, so the fixture writes the row directly.
    fn retain_as_legacy(&self, reconciliation: &str) -> ReviewReconciliation {
        let mut record = self.stored(reconciliation);
        record.schema_version = 3;
        record.binding.pull_request.landed = None;
        for disposition in &mut record.dispositions {
            disposition.landed_commit = None;
        }
        for check in &mut record.remediation_checks {
            check.landed_commit = None;
        }
        record.binding_digest = sha256_hex(&serde_json::to_vec(&record.binding).unwrap());
        let refused = self
            .owner
            .review_store()
            .unwrap()
            .review_reconciliation_update(&self.owner.workspace_id().unwrap(), &record)
            .expect_err("the store never rewrites a binding or schema");
        assert!(refused.to_string().contains("immutable"), "{refused}");
        let database = self.owner.global_root().join("orbit.db");
        let changed = rusqlite::Connection::open(database)
            .unwrap()
            .execute(
                "UPDATE review_reconciliations SET record_json=?1 WHERE workspace_id=?2 AND reconciliation_id=?3",
                rusqlite::params![
                    serde_json::to_string(&record).unwrap(),
                    self.owner.workspace_id().unwrap(),
                    reconciliation
                ],
            )
            .unwrap();
        assert_eq!(changed, 1, "the review store lives in the global database");
        self.stored(reconciliation)
    }
}

/// The recovered Mac delivery that motivated this: the follower handed off
/// PR #42, the owner's landing stopped, an operator fixed and merged the PR
/// by hand, revoked the handoff and recovered the claim. The changed merged
/// head completes only after the operator's reconciliation validated and
/// reviewed exactly that head; the original run's identity never changes.
#[test]
fn a_recovered_follower_delivery_completes_through_a_reconciled_merged_head() {
    if !isolated(
        module_path!(),
        "a_recovered_follower_delivery_completes_through_a_reconciled_merged_head",
    ) {
        return;
    }
    let delivery = Delivery::handed_off(&["test -f src/f0.rs"]);
    let owner = &delivery.owner;
    let task = &delivery.task;
    let leaf = &delivery.leaf;
    let candidate = "a".repeat(40);
    pull_request_is("MERGED", &candidate, &delivery.landing, None);

    // While the handoff holds its claim, the snapshot names the supported way
    // forward, and no reconciliation can bind the live claim.
    let held = delivery.completion_refusal();
    assert!(
        held.contains("revoke the handoff and recover the claim"),
        "{held}"
    );
    delivery.merged();
    let live = delivery
        .reconcile(json!({"action": "submit", "request_key": "live"}))
        .expect_err("a live claim cannot be reconciled");
    assert!(live.to_string().contains("recover the claim"), "{live}");

    delivery.recover();
    let shown = delivery.pair.owner_task(task);
    assert_eq!(shown["job_run_id"], leaf.as_str(), "{shown:#}");
    assert_eq!(
        shown["job_run_machine"]["machine_id"], FOLLOWER,
        "{shown:#}"
    );
    assert_eq!(shown["external_refs"], json!([]), "{shown:#}");

    // An open pull request neither completes nor reconciles, and the head is
    // read by number: the stand-in provider refuses any listing.
    pull_request_is("OPEN", &candidate, &delivery.landing, None);
    let open = delivery.snapshot(McpCapability::Operator);
    assert_eq!(open.reviewed_head.as_deref(), Some(candidate.as_str()));
    assert!(open.actions.review.enabled, "{:?}", open.actions.review);
    assert!(delivery.completion_refusal().contains("has not merged"));
    let unmerged = delivery
        .reconcile(json!({"action": "submit", "request_key": "open"}))
        .expect_err("an open pull request cannot be reconciled");
    assert!(
        unmerged.to_string().contains("has not merged"),
        "{unmerged}"
    );

    // A provider failure names its cause.
    std::fs::write(home().join("pr.fail"), "HTTP 502: upstream unavailable").unwrap();
    let unreadable = delivery.completion_refusal();
    assert!(
        unreadable.contains("could not be read from the provider")
            && unreadable.contains("HTTP 502"),
        "{unreadable}"
    );
    std::fs::remove_file(home().join("pr.fail")).unwrap();

    // A head changed after the handoff inherits none of its candidate's
    // evidence, in the snapshot and in the write alike, and the refusal
    // names the operator's reconciliation.
    delivery.merged();
    pull_request_is("MERGED", &delivery.head, &delivery.landing, None);
    let missing_merge = delivery
        .reconcile(json!({"action": "inspect"}))
        .expect("inspect incomplete provider evidence");
    assert_eq!(missing_merge["eligible"], false, "{missing_merge:#}");
    assert!(
        missing_merge["refusal"]
            .as_str()
            .unwrap_or_default()
            .contains("without its merge commit"),
        "{missing_merge:#}"
    );
    delivery.merged();
    let changed = delivery.snapshot(McpCapability::Operator);
    assert_eq!(
        changed.reviewed_head.as_deref(),
        Some(delivery.head.as_str())
    );
    let refusal = delivery.completion_refusal();
    assert!(
        refusal.contains("do not carry to a changed head")
            && refusal.contains(&format!("orbit task reconcile-review submit {task}")),
        "{refusal}"
    );
    let operator = session(McpCapability::Operator);
    let refused = owner
        .desktop_task_write(
            review(
                task,
                &changed.revision,
                verdict(&shown, &delivery.head, leaf),
                "stale-head",
            ),
            None,
            None,
            &operator,
        )
        .expect_err("a changed head cannot complete on the candidate's evidence");
    assert!(
        refused
            .to_string()
            .contains("do not carry to a changed head"),
        "{refused}"
    );

    // Agents can neither produce the evidence nor complete.
    let agent = session(McpCapability::Agent);
    let denied = owner
        .run_tool_with_context_and_role(
            RECONCILE,
            json!({"action": "submit", "id": task, "request_key": "agent"}),
            Role::Admin,
            ToolContext {
                session_context: agent.clone(),
                ..ToolContext::default()
            },
        )
        .expect_err("an agent cannot reconcile its own delivery");
    assert!(!denied.to_string().is_empty());
    assert!(
        !delivery
            .snapshot(McpCapability::Agent)
            .actions
            .complete
            .enabled
    );

    // The operator inspects, then submits: the run validates and reviews the
    // exact merged head and settles the record.
    let inspected = delivery
        .reconcile(json!({"action": "inspect"}))
        .expect("inspect");
    assert_eq!(inspected["eligible"], true, "{inspected:#}");
    assert_eq!(
        inspected["binding"]["pull_request"]["merged_head"]["commit"],
        delivery.head.as_str()
    );
    assert_eq!(inspected["required_commands"], json!(["test -f src/f0.rs"]));
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery.submit("final-head");
    assert_eq!(submitted["replayed"], false, "{submitted:#}");
    let reconciliation = submitted["reconciliation_id"].as_str().unwrap().to_string();
    let replay = delivery.submit("final-head");
    assert_eq!(replay["replayed"], true, "{replay:#}");
    assert_eq!(replay["run_id"], submitted["run_id"]);

    let settled = delivery.run(&submitted);
    assert_eq!(settled["outcome"], "accepted", "{settled:#}");
    let record = &settled["record"];
    let execution = &record["binding"]["execution"];
    assert_eq!(execution["run_id"], leaf.as_str());
    assert_eq!(execution["machine_id"], FOLLOWER);
    assert_eq!(execution["candidate_commit"], candidate.as_str());
    let validation = &record["validation"];
    assert_eq!(validation["complete"], true, "{record:#}");
    assert_eq!(validation["run_id"], submitted["run_id"]);
    assert_eq!(
        validation["commands"][0]["head"]["commit"],
        delivery.head.as_str()
    );
    assert_eq!(record["review"]["verdict"], "accept", "{record:#}");
    assert_eq!(record["review"]["crew"], "sol");
    assert!(record["follow_up_task_id"].is_null());
    let after = delivery.submit("final-head");
    assert_eq!(after["replayed"], true);
    assert_eq!(after["record"]["attempts"].as_array().unwrap().len(), 1);

    // The accepted reconciliation lets the operator, and only the operator,
    // complete.
    let reconciled = delivery.snapshot(McpCapability::Operator);
    assert!(
        reconciled.actions.complete.enabled,
        "{:?}",
        reconciled.actions.complete
    );
    assert!(
        owner
            .desktop_task_write(
                review(
                    task,
                    &reconciled.revision,
                    verdict(&shown, &delivery.head, leaf),
                    "agent"
                ),
                None,
                None,
                &agent,
            )
            .is_err(),
        "an agent session cannot complete"
    );
    let request = review(
        task,
        &reconciled.revision,
        verdict(&shown, &delivery.head, leaf),
        "complete",
    );
    let done = owner
        .desktop_task_write(request.clone(), None, None, &operator)
        .expect("evidence-bound completion");
    assert!(!done.replayed);
    assert_eq!(done.snapshot.task.status, TaskStatus::Done);
    let replay = owner
        .desktop_task_write(request, None, None, &operator)
        .expect("replay");
    assert!(replay.replayed);

    // Provenance and the retained records stay truthful.
    let completed = delivery.pair.owner_task(task);
    assert_eq!(completed["job_run_id"], leaf.as_str());
    assert_eq!(completed["job_run_machine"]["machine_id"], FOLLOWER);
    let audit = comments_of(&completed);
    assert!(
        audit.contains(&format!("execution_machine={FOLLOWER}")),
        "{audit}"
    );
    assert!(audit.contains(&format!("pull_request={PR_URL}")), "{audit}");
    assert!(audit.contains(&reconciliation), "{audit}");
    assert_eq!(
        completed["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["event"] == "desktop_mutation")
            .count(),
        1,
        "{completed:#}"
    );
    let retained = owner.distributed_claim_console().unwrap();
    assert_eq!(retained["claims"][0]["phase"], "revoked");
    assert_eq!(
        retained["claims"][0]["handoff"]["handoff_id"].as_str(),
        Some(delivery.handoff_id.as_str())
    );
    // The merged pull request is never rewritten.
    assert_eq!(rev(&delivery.repo, &delivery.landing), delivery.merge);
}

/// A reviewer's open finding refuses the merged head and becomes one
/// follow-up task; the merged pull request stays as it landed.
#[test]
fn a_rejected_merged_head_files_a_follow_up_and_cannot_complete() {
    if !isolated(
        module_path!(),
        "a_rejected_merged_head_files_a_follow_up_and_cannot_complete",
    ) {
        return;
    }
    let delivery = Delivery::handed_off(&["test -f src/f0.rs"]);
    delivery.recover();
    delivery.merged();
    delivery.reviewer_reports(
        "reject",
        json!([{
            "id": "F1",
            "severity": "high",
            "summary": "The hand fix drops the handed-off change's error handling.",
            "paths": ["src/f0.rs"],
            "disposition": "open",
        }]),
    );
    let submitted = delivery.submit("reject");
    let settled = delivery.run(&submitted);
    assert_eq!(settled["outcome"], "refused", "{settled:#}");
    let follow_up = settled["record"]["follow_up_task_id"]
        .as_str()
        .expect("a follow-up task")
        .to_string();
    let filed = delivery.pair.owner_task(&follow_up);
    assert!(
        filed["description"]
            .as_str()
            .unwrap_or_default()
            .contains("drops the handed-off change's error handling"),
        "{filed:#}"
    );
    let refusal = delivery.completion_refusal();
    assert!(refusal.contains("was refused"), "{refusal}");

    // A refused reconciliation settles; resubmitting its key replays it.
    let replay = delivery.submit("reject");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["record"]["follow_up_task_id"], follow_up.as_str());
    assert_eq!(rev(&delivery.repo, &delivery.landing), delivery.merge);
    assert_eq!(delivery.pair.owner_status(&delivery.task), "review");
}

/// A required command that also fails at the base waits for an operator's
/// evidence-bound disposition naming a landed remediation; validation stays
/// incomplete even after the disposition lets the task complete.
#[test]
fn a_baseline_failure_completes_only_on_an_operator_disposition() {
    if !isolated(
        module_path!(),
        "a_baseline_failure_completes_only_on_an_operator_disposition",
    ) {
        return;
    }
    std::fs::create_dir_all(home().join("bin")).unwrap();
    let command_path = home().join("bin/verify.sh");
    std::fs::write(&command_path, "#!/bin/sh\ntest -f NOTICE\n").unwrap();
    executable(&command_path);
    let command = command_path.to_string_lossy().into_owned();
    let delivery = Delivery::handed_off(&[&command]);
    delivery.retain_protocol7_handoff(&command);
    delivery.recover();
    delivery.merged();
    let inspected = delivery
        .reconcile(json!({"action": "inspect"}))
        .expect("the retained protocol-7 handoff is readable through public inspect");
    assert_eq!(inspected["eligible"], true, "{inspected:#}");
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery.submit("baseline");
    let settled = delivery.run(&submitted);
    assert_eq!(settled["outcome"], "awaiting_disposition", "{settled:#}");
    let reconciliation = settled["reconciliation_id"].as_str().unwrap().to_string();
    let recorded = &settled["record"]["validation"];
    assert_eq!(recorded["complete"], false);
    assert_eq!(recorded["commands"][0]["head"]["passed"], false);
    assert_eq!(recorded["commands"][0]["baseline"]["passed"], false);
    assert_eq!(
        recorded["commands"][0]["command"], "~/bin/verify.sh",
        "public reconciliation reports redact the private HOME path"
    );
    let head_log = delivery
        .owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({
                "id": delivery.task,
                "path": recorded["commands"][0]["head"]["log"]["path"],
            }),
        )
        .expect("read the redacted reconciliation validation report");
    let head_log = head_log["content"]
        .as_str()
        .expect("text validation report");
    assert!(
        !head_log.contains(home().to_string_lossy().as_ref()),
        "{head_log}"
    );
    assert!(head_log.contains("~/bin/verify.sh"), "{head_log}");
    let refusal = delivery.completion_refusal();
    assert!(
        refusal.contains("orbit task reconcile-review accept-baseline"),
        "{refusal}"
    );

    let reason_secret = format!("ghp_{}", "A".repeat(36));
    let dispose = |remediation: &str| {
        delivery.reconcile(json!({
            "action": "accept_baseline",
            "reconciliation_id": reconciliation,
            "command": command.as_str(),
            "remediation_commit": remediation,
            "reason": format!("NOTICE was missing on the landing branch; token={reason_secret}"),
        }))
    };
    // The merged head itself remediates nothing.
    let head = dispose(&delivery.head).expect_err("the head is not a remediation");
    assert!(!head.to_string().is_empty());

    std::fs::write(delivery.repo.join("unrelated.txt"), "unrelated\n").unwrap();
    git(&delivery.repo, &["add", "unrelated.txt"]);
    git(
        &delivery.repo,
        &["commit", "-q", "-m", "Unrelated landing change"],
    );
    let unrelated = rev(&delivery.repo, "HEAD");
    let failed_remediation = dispose(&unrelated)
        .expect_err("a landed commit that does not fix the check cannot justify disposition");
    assert!(
        failed_remediation
            .to_string()
            .contains("did not pass at remediation"),
        "{failed_remediation}"
    );
    let failed_check = delivery.status(&reconciliation);
    assert_eq!(failed_check["outcome"], "awaiting_disposition");
    assert_eq!(failed_check["record"]["validation"]["complete"], false);
    assert_eq!(
        failed_check["record"]["remediation_checks"][0]["run"]["passed"],
        false
    );
    assert!(
        !delivery
            .snapshot(McpCapability::Operator)
            .actions
            .complete
            .enabled,
        "{:?}",
        delivery.snapshot(McpCapability::Operator).actions
    );

    std::fs::write(delivery.repo.join("NOTICE"), "notice\n").unwrap();
    git(&delivery.repo, &["add", "NOTICE"]);
    git(&delivery.repo, &["commit", "-q", "-m", "Add NOTICE"]);
    let remediation = rev(&delivery.repo, "HEAD");
    let fake_secret = format!("ghp_{}", "B".repeat(36));
    let invalid_selector = format!("{command} --token={fake_secret}");
    let refusal = delivery
        .reconcile(json!({
            "action": "accept_baseline",
            "reconciliation_id": reconciliation,
            "command": invalid_selector,
            "remediation_commit": remediation,
            "reason": "unknown selector",
        }))
        .expect_err("an unrecognized command selector cannot widen the disposition");
    let refusal = refusal.to_string();
    assert!(refusal.contains("is not a baseline failure"), "{refusal}");
    assert!(
        !refusal.contains(home().to_string_lossy().as_ref()),
        "{refusal}"
    );
    assert!(!refusal.contains(&fake_secret), "{refusal}");
    let still_waiting = delivery.status(&reconciliation);
    assert_eq!(still_waiting["outcome"], "awaiting_disposition");
    assert_eq!(still_waiting["record"]["validation"]["complete"], false);

    let disposed = dispose(&remediation).expect("disposition");
    assert_eq!(
        disposed["outcome"], "accepted_with_disposition",
        "{disposed:#}"
    );
    assert_eq!(
        inspected["contract"]["accepted_commands"],
        json!(["~/bin/verify.sh"]),
        "public reconciliation reports redact the private HOME path"
    );
    assert_eq!(disposed["record"]["validation"]["complete"], false);
    let disposition = &disposed["record"]["dispositions"][0];
    assert_eq!(disposition["command"], "~/bin/verify.sh");
    assert!(!disposition["reason"].as_str().unwrap().contains("ghp_"));
    assert!(
        disposition["reason"]
            .as_str()
            .unwrap()
            .contains("[REDACTED_SECRET]")
    );
    assert_eq!(disposition["remediation_commit"], remediation.as_str());
    assert_eq!(disposition["head_commit"], delivery.head.as_str());
    assert!(disposition["remediation_check"]["sha256"].is_string());
    assert_eq!(
        disposed["record"]["remediation_checks"][1]["run"]["passed"],
        true
    );
    let again = dispose(&remediation).expect("an identical disposition replays");
    assert_eq!(again["record"]["dispositions"].as_array().unwrap().len(), 1);

    let stored = delivery
        .owner
        .review_store()
        .unwrap()
        .review_reconciliation(&delivery.owner.workspace_id().unwrap(), &reconciliation)
        .unwrap()
        .expect("retained reconciliation");
    assert_eq!(stored.contract.required_commands, vec![command.clone()]);
    assert_eq!(stored.dispositions[0].command, command);
    let task_comments = comments_of(&delivery.pair.owner_task(&delivery.task));
    assert!(
        !task_comments.contains(home().to_string_lossy().as_ref()),
        "{task_comments}"
    );
    assert!(!task_comments.contains(&reason_secret), "{task_comments}");
    assert!(
        task_comments.contains("[REDACTED_SECRET]"),
        "{task_comments}"
    );

    let audit_events = delivery
        .owner
        .list_audit_events(None, Some(RECONCILE.into()), None, None, 100)
        .unwrap();
    assert!(
        audit_events
            .iter()
            .any(|event| event.command == "artifact_redaction"),
        "the redacted reconciliation response should emit its audit record"
    );
    for event in &audit_events {
        let audit_text = [
            event.arguments_json.as_deref(),
            event.error_message.as_deref(),
            event.stdout_truncated.as_deref(),
            event.stderr_truncated.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        assert!(
            !audit_text.contains(home().to_string_lossy().as_ref()),
            "reconciliation audit content echoed the private HOME path: {event:#?}"
        );
        assert!(
            !audit_text.contains("ghp_"),
            "reconciliation audit content echoed credential-like text: {event:#?}"
        );
    }

    let snapshot = delivery.snapshot(McpCapability::Operator);
    assert!(snapshot.actions.complete.enabled, "{:?}", snapshot.actions);

    // The ordinary desktop write completes on the disposition, and a lost
    // response replays rather than completing twice.
    let owner = &delivery.owner;
    let task = &delivery.task;
    let operator = session(McpCapability::Operator);
    let shown = delivery.pair.owner_task(task);
    let request = review(
        task,
        &snapshot.revision,
        verdict(&shown, &delivery.head, &delivery.leaf),
        "complete-disposed",
    );
    let done = owner
        .desktop_task_write(request.clone(), None, None, &operator)
        .expect("completion on an evidence-bound disposition");
    assert!(!done.replayed);
    assert_eq!(done.snapshot.task.status, TaskStatus::Done);
    let replay = owner
        .desktop_task_write(request, None, None, &operator)
        .expect("lost-response replay");
    assert!(replay.replayed);
    assert_eq!(replay.snapshot.task.status, TaskStatus::Done);

    // The original run's identity is unchanged, and the failure stays a
    // failure: the disposition is recorded beside it, never instead of it.
    let completed = delivery.pair.owner_task(task);
    assert_eq!(completed["job_run_id"], delivery.leaf.as_str());
    assert_eq!(completed["job_run_machine"]["machine_id"], FOLLOWER);
    assert_eq!(
        completed["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["event"] == "desktop_mutation")
            .count(),
        1,
        "{completed:#}"
    );
    let retained = delivery.status(&reconciliation);
    assert_eq!(retained["outcome"], "accepted_with_disposition");
    let validation = &retained["record"]["validation"];
    assert_eq!(validation["complete"], false, "{retained:#}");
    assert_eq!(validation["commands"][0]["head"]["passed"], false);
    assert_eq!(validation["commands"][0]["baseline"]["passed"], false);
    assert_eq!(
        retained["record"]["dispositions"][0]["failure_log_sha256"],
        validation["commands"][0]["head"]["log"]["sha256"]
    );

    // No ordinary review certificate was invented for the merged head.
    let merged = &retained["record"]["binding"]["pull_request"];
    let store = owner.review_store().unwrap();
    assert!(
        store
            .review_certificates_for_tree(
                merged["repository"].as_str().unwrap(),
                merged["merged_head"]["tree"].as_str().unwrap(),
                10,
            )
            .unwrap()
            .is_empty(),
        "a disposition must not mint a passed review certificate"
    );
    assert!(
        store
            .review_certificate(&owner.workspace_id().unwrap(), &reconciliation)
            .unwrap()
            .is_none()
    );
}

/// A baseline remediation must contain the delivery that landed. A fix that
/// landed on the landing branch before the pull request merged passes the
/// same required command on its own, while the merged code still fails it
/// for a reason the delivery introduced; the disposition refuses it before
/// running anything, and accepts a remediation landed on top of the merge.
#[test]
fn a_baseline_remediation_must_contain_the_landed_delivery() {
    if !isolated(
        module_path!(),
        "a_baseline_remediation_must_contain_the_landed_delivery",
    ) {
        return;
    }
    // Test A: the landing branch lacks NOTICE. Test B: the delivery's hand
    // fix is a regression. One required command runs both.
    std::fs::create_dir_all(home().join("bin")).unwrap();
    let command_path = home().join("bin/suite.sh");
    std::fs::write(
        &command_path,
        "#!/bin/sh\ntest -f NOTICE || exit 1\n! grep -q fixed_by_hand src/f0.rs\n",
    )
    .unwrap();
    executable(&command_path);
    let command = command_path.to_string_lossy().into_owned();
    let delivery = Delivery::landed(&[&command], Landing::Merge, Some(("NOTICE", "notice\n")));
    delivery.recover();
    delivery.merged();
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery.submit("masked");
    let reconciliation = submitted["reconciliation_id"].as_str().unwrap().to_string();
    let settled = delivery.run(&submitted);
    assert_eq!(settled["outcome"], "awaiting_disposition", "{settled:#}");
    let record = &settled["record"];
    assert_eq!(record["schema_version"], 4);
    assert_eq!(
        record["binding"]["pull_request"]["landed"]["commit"],
        delivery.merge.as_str(),
        "the provider's merge commit is bound into the reconciliation"
    );
    let original = record["validation"].clone();
    let original_stored = delivery.stored(&reconciliation).validation;
    assert_eq!(original["complete"], false);
    assert_eq!(original["commands"][0]["head"]["passed"], false);
    assert_eq!(original["commands"][0]["baseline"]["passed"], false);

    // The landing-branch fix landed before the merge: it is on the landing
    // branch, not in the head, and the required command passes there alone.
    let pre_merge = rev(&delivery.repo, &format!("{}^1", delivery.merge));
    let alone = delivery.pair._root.path().join("pre-merge");
    git(
        &delivery.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            alone.to_str().unwrap(),
            &pre_merge,
        ],
    );
    let passes_alone = std::process::Command::new(&command_path)
        .current_dir(&alone)
        .status()
        .unwrap();
    git(
        &delivery.repo,
        &["worktree", "remove", "--force", alone.to_str().unwrap()],
    );
    assert!(
        passes_alone.success(),
        "the pre-merge fix passes on its own"
    );

    let refused = delivery
        .dispose(&reconciliation, &command, &pre_merge)
        .expect_err("a remediation without the landed delivery cannot dispose");
    assert!(
        matches!(refused, OrbitError::InvalidInput(_)),
        "{refused:?}"
    );
    let refused = refused.to_string();
    assert!(
        refused.contains(&format!("does not contain commit {}", delivery.merge))
            && refused.contains("land the fix on"),
        "{refused}"
    );
    let untouched = delivery.stored(&reconciliation);
    assert!(
        untouched.remediation_checks.is_empty(),
        "the command never ran at the pre-merge fix"
    );
    assert!(untouched.dispositions.is_empty());
    assert_eq!(untouched.validation, original_stored);
    assert!(!delivery.completes());

    // A remediation landed on top of the merge fixes test B as well.
    let remediation = delivery.land(
        "src/f0.rs",
        "fn work() { repaired() }\n",
        "Repair the hand fix",
    );

    // An infrastructure failure while preparing the remediation run keeps its
    // error class, and its message carries neither the private HOME nor a
    // credential.
    let hooks = home().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let token = format!("ghp_{}", "C".repeat(36));
    std::fs::write(
        hooks.join("post-checkout"),
        format!(
            "#!/bin/sh\necho \"cannot populate {}/cache with token {token}\" >&2\nexit 7\n",
            home().display()
        ),
    )
    .unwrap();
    executable(&hooks.join("post-checkout"));
    git(
        &delivery.repo,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let failed = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect_err("the remediation checkout cannot be prepared");
    git(&delivery.repo, &["config", "--unset", "core.hooksPath"]);
    assert!(
        matches!(failed, OrbitError::Execution(_)),
        "an infrastructure failure is not an input refusal: {failed:?}"
    );
    let failed = failed.to_string();
    assert!(failed.contains("cannot populate ~/cache"), "{failed}");
    assert!(
        !failed.contains(home().to_string_lossy().as_ref()),
        "{failed}"
    );
    assert!(!failed.contains(&token), "{failed}");
    let still_waiting = delivery.stored(&reconciliation);
    assert!(still_waiting.dispositions.is_empty());
    assert!(still_waiting.remediation_checks.is_empty());

    let disposed = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect("a remediation containing the landed delivery disposes");
    assert_eq!(
        disposed["outcome"], "accepted_with_disposition",
        "{disposed:#}"
    );
    let record = &disposed["record"];
    assert_eq!(record["validation"], original, "original failures stay");
    let disposition = &record["dispositions"][0];
    assert_eq!(disposition["remediation_commit"], remediation.as_str());
    assert_eq!(disposition["landed_commit"], delivery.merge.as_str());
    assert_eq!(
        disposition["failure_log_sha256"],
        original["commands"][0]["head"]["log"]["sha256"]
    );
    assert_eq!(
        record["remediation_checks"][0]["landed_commit"],
        delivery.merge.as_str()
    );
    assert!(delivery.completes());
    let again = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect("an identical disposition replays");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["record"]["dispositions"].as_array().unwrap().len(), 1);

    // A record persisted before the landed commit was bound keeps its
    // evidence, never grants completion and is never disposed again; a new
    // request key reconciles the same head with the landed commit bound.
    let legacy = delivery.retain_as_legacy(&reconciliation);
    assert_eq!(legacy.dispositions.len(), 1);
    let refusal = delivery.completion_refusal();
    assert!(
        refusal.contains(&reconciliation) && refusal.contains("submit a new request key"),
        "{refusal}"
    );
    let legacy_refusal = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect_err("a legacy record cannot be disposed");
    assert!(
        matches!(legacy_refusal, OrbitError::InvalidInput(_)),
        "{legacy_refusal:?}"
    );
    assert!(
        legacy_refusal
            .to_string()
            .contains("predates binding the pull request's landed commit"),
        "{legacy_refusal}"
    );
    assert_eq!(delivery.stored(&reconciliation), legacy);

    let fresh = delivery.submit("masked-landed");
    let fresh_id = fresh["reconciliation_id"].as_str().unwrap().to_string();
    assert_ne!(fresh_id, reconciliation);
    let fresh_settled = delivery.run(&fresh);
    assert_eq!(
        fresh_settled["outcome"], "awaiting_disposition",
        "{fresh_settled:#}"
    );
    let fresh_disposed = delivery
        .dispose(&fresh_id, &command, &remediation)
        .expect("the fresh reconciliation disposes");
    assert_eq!(fresh_disposed["outcome"], "accepted_with_disposition");
    assert_eq!(fresh_disposed["record"]["validation"]["complete"], false);
    assert!(delivery.completes());
    assert_eq!(delivery.stored(&reconciliation), legacy);
    let execution = &fresh_disposed["record"]["binding"]["execution"];
    assert_eq!(execution["run_id"], delivery.leaf.as_str());
    assert_eq!(execution["machine_id"], FOLLOWER);
    assert_eq!(execution["claim_id"], delivery.claim_id.as_str());
    assert_eq!(execution["handoff_id"], delivery.handoff_id.as_str());
}

/// A squash landing keeps none of the head's history: the remediation must
/// contain the squash commit the provider reports, never the head. Provider
/// facts that change before or while the remediation runs refuse the
/// disposition, and completion binds the same landed commit.
#[test]
fn a_squash_landing_disposes_on_a_remediation_containing_the_squash_commit() {
    if !isolated(
        module_path!(),
        "a_squash_landing_disposes_on_a_remediation_containing_the_squash_commit",
    ) {
        return;
    }
    // The command can swap the provider's answer while it runs.
    std::fs::create_dir_all(home().join("bin")).unwrap();
    let command_path = home().join("bin/notice.sh");
    let swap = home().join("swap-provider");
    let swapped = home().join("pr.swapped.json");
    std::fs::write(
        &command_path,
        format!(
            "#!/bin/sh\nif [ -f '{swap}' ]; then cp '{swapped}' '{answer}'; fi\ntest -f NOTICE\n",
            swap = swap.display(),
            swapped = swapped.display(),
            answer = home().join("pr.json").display(),
        ),
    )
    .unwrap();
    executable(&command_path);
    let command = command_path.to_string_lossy().into_owned();
    let delivery = Delivery::landed(&[&command], Landing::Squash, None);
    let squash = delivery.merge.clone();
    assert_eq!(
        git(
            &delivery.repo,
            &["rev-list", "--parents", "-n", "1", &squash]
        )
        .split_whitespace()
        .count(),
        2,
        "a squash commit has one parent"
    );
    delivery.recover();
    delivery.merged();
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery.submit("squash");
    let reconciliation = submitted["reconciliation_id"].as_str().unwrap().to_string();
    let settled = delivery.run(&submitted);
    assert_eq!(settled["outcome"], "awaiting_disposition", "{settled:#}");
    assert_eq!(
        settled["record"]["binding"]["pull_request"]["landed"]["commit"],
        squash.as_str()
    );
    let original = settled["record"]["validation"].clone();

    let remediation = delivery.land("NOTICE", "notice\n", "Add NOTICE");
    let head_in_remediation = std::process::Command::new("git")
        .arg("-C")
        .arg(&delivery.repo)
        .args(["merge-base", "--is-ancestor", &delivery.head, &remediation])
        .status()
        .unwrap();
    assert!(
        !head_in_remediation.success(),
        "the squash remediation does not contain the pull request's head"
    );

    // The provider reports another landed commit before the run: refused
    // before the command runs.
    pull_request_is(
        "MERGED",
        &delivery.head,
        &delivery.landing,
        Some(&remediation),
    );
    let moved = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect_err("a changed landed commit refuses before running");
    assert!(moved.to_string().contains("delivery changed"), "{moved}");
    assert!(
        delivery
            .stored(&reconciliation)
            .remediation_checks
            .is_empty()
    );
    assert!(!delivery.completes());

    // The provider's answer changes while the command runs: the check is kept
    // as evidence, but no disposition is recorded.
    std::fs::rename(home().join("pr.json"), &swapped).unwrap();
    delivery.merged();
    std::fs::write(&swap, "").unwrap();
    let during = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect_err("a landed commit that changed during the run refuses");
    assert!(during.to_string().contains("delivery changed"), "{during}");
    std::fs::remove_file(&swap).unwrap();
    let checked = delivery.stored(&reconciliation);
    assert_eq!(checked.remediation_checks.len(), 1);
    assert!(checked.remediation_checks[0].run.passed);
    assert_eq!(
        checked.remediation_checks[0].landed_commit.as_deref(),
        Some(squash.as_str())
    );
    assert!(checked.dispositions.is_empty());

    // With the provider's answer restored, the passing check is reused.
    delivery.merged();
    let disposed = delivery
        .dispose(&reconciliation, &command, &remediation)
        .expect("a remediation containing the squash commit disposes");
    assert_eq!(
        disposed["outcome"], "accepted_with_disposition",
        "{disposed:#}"
    );
    let record = &disposed["record"];
    assert_eq!(record["validation"], original);
    assert_eq!(record["validation"]["complete"], false);
    assert_eq!(record["remediation_checks"].as_array().unwrap().len(), 1);
    assert_eq!(record["dispositions"][0]["landed_commit"], squash.as_str());
    assert!(delivery.completes());

    // Completion binds the landed commit as the provider reports it now.
    pull_request_is(
        "MERGED",
        &delivery.head,
        &delivery.landing,
        Some(&remediation),
    );
    assert!(!delivery.completes());
    delivery.merged();
    assert!(delivery.completes());
}

/// A command that fails as code at the merged head but cannot be judged at
/// the base is not an attributable head-only regression or a baseline
/// disposition candidate.
#[test]
fn a_baseline_environment_failure_is_not_misreported_as_head_only() {
    if !isolated(
        module_path!(),
        "a_baseline_environment_failure_is_not_misreported_as_head_only",
    ) {
        return;
    }
    let command =
        "grep -q fixed_by_hand src/f0.rs && exit 1 || orbit-baseline-environment-probe-14175";
    let delivery = Delivery::handed_off(&[command]);
    delivery.recover();
    delivery.merged();
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery.submit("base-environment");
    let settled = delivery.run(&submitted);

    assert_eq!(settled["outcome"], "refused", "{settled:#}");
    assert!(
        settled["next_step"]
            .as_str()
            .unwrap_or_default()
            .contains("make the required check runnable at both revisions"),
        "{settled:#}"
    );
    let command_result = &settled["record"]["validation"]["commands"][0];
    assert_eq!(command_result["head"]["passed"], false);
    assert_eq!(command_result["head"]["failure_kind"], "candidate");
    assert_eq!(command_result["baseline"]["passed"], false);
    assert_eq!(command_result["baseline"]["failure_kind"], "environment");
    assert_eq!(settled["record"]["follow_up_task_id"], Value::Null);
    assert!(settled["record"]["dispositions"].is_null());
    assert!(
        !delivery
            .snapshot(McpCapability::Operator)
            .actions
            .complete
            .enabled
    );
}

/// The validation commands and review crew are frozen when the operator
/// submits. A configuration edited before the run starts, or before a
/// resubmission, neither drops a requirement nor changes the reviewer; a
/// frozen crew that no longer resolves refuses with the new-request path.
#[test]
fn a_reconciliation_runs_under_the_contract_frozen_at_submission() {
    if !isolated(
        module_path!(),
        "a_reconciliation_runs_under_the_contract_frozen_at_submission",
    ) {
        return;
    }
    let required = "test -f src/f0.rs";
    let delivery = Delivery::handed_off(&[required]);
    delivery.recover();
    delivery.merged();
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery.submit("frozen");
    let contract = &submitted["record"]["contract"];
    assert_eq!(
        contract["required_commands"],
        json!([required]),
        "{contract:#}"
    );
    assert_eq!(contract["accepted_commands"], json!([required]));
    assert_eq!(contract["commands_source"], "accepted_handoff");
    assert_eq!(contract["review_crew"], "sol");

    // After submission the owner requires a command this head fails and
    // reviews on another crew. The admitted run still validates and reviews
    // under the frozen contract.
    let edited = delivery.reconfigured(&owner_config(&["false"], "luna", &["sol", "luna"]));
    let replay = delivery
        .reconcile_on(
            &edited,
            json!({"action": "submit", "request_key": "frozen"}),
        )
        .expect("replay under an edited configuration");
    assert_eq!(replay["replayed"], true, "{replay:#}");
    assert_eq!(replay["record"]["contract"], *contract);
    delivery.execute_on(&edited, submitted["run_id"].as_str().unwrap());
    let settled = delivery
        .reconcile_on(
            &edited,
            json!({"action": "status", "reconciliation_id": submitted["reconciliation_id"]}),
        )
        .unwrap()["reconciliations"][0]
        .clone();
    assert_eq!(settled["outcome"], "accepted", "{settled:#}");
    let record = &settled["record"];
    let commands: Vec<&str> = record["validation"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|command| command["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands, [required], "{record:#}");
    assert_eq!(record["review"]["crew"], "sol", "{record:#}");
    assert_eq!(record["contract"], *contract);

    // Only a new request key adopts the edited review crew; the delivery's
    // own captured obligations stay whatever the configuration now says.
    let inspected = delivery
        .reconcile_on(&edited, json!({"action": "inspect"}))
        .unwrap();
    assert_eq!(
        inspected["contract"]["required_commands"],
        json!([required])
    );
    assert_eq!(inspected["contract"]["commands_source"], "accepted_handoff");
    assert_eq!(inspected["contract"]["review_crew"], "luna");

    // A frozen crew the owner no longer defines fails the attempt instead of
    // substituting the current crew. Resubmitting the key says how to go on,
    // and once the crew is restored a new attempt runs under the same
    // contract.
    let crew_gone = delivery.submit("crew-gone");
    assert_eq!(crew_gone["record"]["contract"]["review_crew"], "sol");
    let without_sol = delivery.reconfigured(&owner_config(&[required], "luna", &["luna"]));
    delivery.execute_on(&without_sol, crew_gone["run_id"].as_str().unwrap());
    let reconciliation = crew_gone["reconciliation_id"].as_str().unwrap();
    let failed = delivery.status(reconciliation);
    assert!(failed["outcome"].is_null(), "{failed:#}");
    assert_eq!(failed["run_state"], "failed", "{failed:#}");
    assert!(failed["record"]["review"].is_null(), "{failed:#}");
    let blocked = delivery
        .reconcile_on(
            &without_sol,
            json!({"action": "submit", "request_key": "crew-gone"}),
        )
        .expect_err("the frozen crew is unavailable");
    assert!(
        blocked.to_string().contains("frozen when it was submitted")
            && blocked.to_string().contains("new request key"),
        "{blocked}"
    );
    let restored = delivery.reconfigured(&owner_config(&["false"], "luna", &["sol", "luna"]));
    let retried = delivery
        .reconcile_on(
            &restored,
            json!({"action": "submit", "request_key": "crew-gone"}),
        )
        .expect("a new attempt once the crew is back");
    assert_eq!(retried["record"]["attempts"].as_array().unwrap().len(), 2);
    delivery.execute_on(&restored, retried["run_id"].as_str().unwrap());
    let settled = delivery
        .reconcile_on(
            &restored,
            json!({"action": "status", "reconciliation_id": reconciliation}),
        )
        .unwrap()["reconciliations"][0]
        .clone();
    assert_eq!(settled["outcome"], "accepted", "{settled:#}");
    assert_eq!(settled["record"]["review"]["crew"], "sol");
    assert_eq!(
        settled["record"]["validation"]["commands"][0]["command"],
        required
    );
}

/// An accepted handoff always carries the owner's captured command list. An
/// explicitly empty one is a no-check acceptance, not a missing snapshot: the
/// reconciliation adopts the owner's configuration at submission as its own
/// contract, labelled so, and refuses when there is none to adopt.
#[test]
fn an_explicit_no_check_acceptance_adopts_a_labelled_owner_contract() {
    if !isolated(
        module_path!(),
        "an_explicit_no_check_acceptance_adopts_a_labelled_owner_contract",
    ) {
        return;
    }
    let delivery = Delivery::handed_off(&[]);
    delivery.recover();
    delivery.merged();
    let unvalidatable = delivery.reconcile(json!({"action": "inspect"})).unwrap();
    assert_eq!(unvalidatable["eligible"], false, "{unvalidatable:#}");
    assert!(
        unvalidatable["refusal"]
            .as_str()
            .unwrap_or_default()
            .contains("required no validation command"),
        "{unvalidatable:#}"
    );
    let refused = delivery
        .reconcile(json!({"action": "submit", "request_key": "nothing"}))
        .expect_err("nothing to validate with");
    assert!(
        refused.to_string().contains("required_validation_commands"),
        "{refused}"
    );

    let required = "test -f src/f0.rs";
    let configured = delivery.reconfigured(&owner_config(&[required], "sol", &["sol"]));
    delivery.reviewer_reports("accept", json!([]));
    let submitted = delivery
        .reconcile_on(
            &configured,
            json!({"action": "submit", "request_key": "adopted"}),
        )
        .expect("operator submission");
    let contract = &submitted["record"]["contract"];
    assert_eq!(contract["accepted_commands"], json!([]), "{contract:#}");
    assert_eq!(contract["required_commands"], json!([required]));
    assert_eq!(
        contract["commands_source"],
        "owner_configuration_at_submission"
    );
    delivery.execute_on(&configured, submitted["run_id"].as_str().unwrap());
    let settled = delivery
        .reconcile_on(
            &configured,
            json!({"action": "status", "reconciliation_id": submitted["reconciliation_id"]}),
        )
        .unwrap()["reconciliations"][0]
        .clone();
    assert_eq!(settled["outcome"], "accepted", "{settled:#}");
    assert_eq!(settled["record"]["validation"]["complete"], true);
}

/// Evidence binds one observation: a head that changes after submission
/// refuses the run, and no run outside the governed submission can produce
/// a record.
#[test]
fn a_reconciliation_refuses_a_changed_head_and_forged_runs() {
    if !isolated(
        module_path!(),
        "a_reconciliation_refuses_a_changed_head_and_forged_runs",
    ) {
        return;
    }
    let delivery = Delivery::handed_off(&["test -f src/f0.rs"]);
    delivery.recover();
    delivery.merged();
    delivery.reviewer_reports("accept", json!([]));
    let owner = &delivery.owner;

    // Ordinary submission cannot carry the reserved admission.
    let forged = owner
        .submit_pipeline_run(
            REVIEW_RECONCILIATION_JOB,
            json!({
                "task_id": delivery.task,
                "reconciliation_id": "rrc-forged",
                "review_reconciliation_admission": {
                    "reconciliation_id": "rrc-forged",
                    "attempt": 1,
                    "authorized_by": "operator",
                    "authorizer_provenance": "interactive",
                    "authorized_at": "2026-10-05T06:50:00Z",
                },
            }),
            None,
            Some("operator"),
        )
        .expect_err("the admission key is reserved");
    assert!(
        forged
            .to_string()
            .contains("review_reconciliation_admission"),
        "{forged}"
    );

    let submitted = delivery.submit("moving-head");
    let reconciliation = submitted["reconciliation_id"].as_str().unwrap().to_string();

    // The pull request's head moves before the admitted run starts.
    git(
        &delivery.repo,
        &["checkout", "-q", &format!("orbit/{}", delivery.task)],
    );
    std::fs::write(delivery.repo.join("src/f0.rs"), "fn work() { again() }\n").unwrap();
    git(&delivery.repo, &["commit", "-q", "-am", "Another fix"]);
    let moved = rev(&delivery.repo, "HEAD");
    git(&delivery.repo, &["checkout", "-q", &delivery.landing]);
    pull_request_is("MERGED", &moved, &delivery.landing, Some(&delivery.merge));
    let settled = delivery.run(&submitted);
    assert_eq!(settled["outcome"], "refused", "{settled:#}");
    assert!(settled["record"]["validation"].is_null(), "{settled:#}");

    // A run submitted without the admission cannot act on the record.
    let unadmitted = owner
        .submit_pipeline_run(
            REVIEW_RECONCILIATION_JOB,
            json!({"task_id": delivery.task, "reconciliation_id": reconciliation}),
            None,
            Some("operator"),
        )
        .expect("an ordinary run is submitted");
    delivery.execute(&unadmitted.run_id);
    let untouched = delivery.status(&reconciliation);
    assert_eq!(untouched["record"], settled["record"], "{untouched:#}");
    let refusal = delivery.completion_refusal();
    assert!(
        refusal.contains("orbit task reconcile-review submit"),
        "{refusal}"
    );
}

/// A claim recovered while its leaf was still running proves nothing about
/// that run, and a run the owner happens to hold under the same id is a
/// different run: completion refuses instead of substituting it.
#[test]
fn a_same_named_owner_run_never_stands_in_for_a_follower_run() {
    if !isolated(
        module_path!(),
        "a_same_named_owner_run_never_stands_in_for_a_follower_run",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let owner = &pair.wire.owner;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let operator = session(McpCapability::Operator);

    // A live claim fences the task, and its reason says how to recover it.
    let live = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert!(
        live.actions
            .complete
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("recover the claim"),
        "{:?}",
        live.actions.complete
    );

    let claim = owner.distributed_claim_console().unwrap()["claims"][0].clone();
    owner
        .recover_claim_as_operator(
            claim["claim_id"].as_str().unwrap(),
            "running",
            TaskStatus::Blocked,
            "operator",
            "the follower went away",
            "recover",
        )
        .expect("recover");

    // The owner's own job store gains a successful run with the leaf's id.
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let local = jobs
        .insert_job_run(LEAF_JOB, 1, Utc::now(), None, None)
        .unwrap()
        .run_id;
    let store = owner.sqlite_store().unwrap();
    let workspace_id = owner.workspace_id().unwrap();
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE job_runs SET run_id = ?1 WHERE workspace_id = ?2 AND run_id = ?3",
                    [leaf.as_str(), workspace_id.as_str(), local.as_str()],
                )
                .unwrap();
            Ok(())
        })
        .unwrap();
    jobs.mark_job_run_running(&leaf, Utc::now(), std::process::id())
        .unwrap();
    jobs.finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    assert_eq!(
        jobs.get_job_run(&leaf).unwrap().unwrap().state,
        JobRunState::Success
    );

    back_to_review(owner, &task, Some("Recovered."));
    let snapshot = owner.desktop_task_snapshot(&task, &operator).unwrap();
    assert!(!snapshot.actions.complete.enabled);
    let reason = snapshot.actions.complete.reason.clone().unwrap_or_default();
    assert!(
        reason.contains("recovered before the run handed off"),
        "{reason}"
    );
    let shown = pair.owner_task(&task);
    let mut no_pr = verdict(&shown, "", &leaf);
    no_pr.expected_head = None;
    no_pr.evidence = vec!["execution_summary".into()];
    for criterion in &mut no_pr.criteria {
        criterion.evidence = vec!["execution_summary".into()];
    }
    let refused = owner
        .desktop_task_write(
            review(&task, &snapshot.revision, no_pr, "substitute"),
            None,
            None,
            &operator,
        )
        .expect_err("the owner's same-named run is not the follower's");
    assert!(
        refused
            .to_string()
            .contains("recovered before the run handed off"),
        "{refused}"
    );
    assert_eq!(pair.owner_status(&task), "review");
}
