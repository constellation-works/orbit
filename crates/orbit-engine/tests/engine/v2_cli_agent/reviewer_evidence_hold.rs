//! [ORB-25667] A before-landing reviewer that declares failure (`status:
//! "failed"`, exit 0) because a required check was denied on its host, but
//! persisted a valid incomplete report naming that check as external
//! evidence, reaches the gate's settlement instead of failing the run. The
//! shipped `landing_review*` steps run as wired, with the reviewer a real
//! provider dispatch of a substitute CLI and admission and settlement
//! scripted. Settlement's evidence hold holds the run; the reviewer's
//! uncommitted repair stays in the worktree for it. Any report the host does
//! not find evidence-only keeps the reviewer step failed, and settlement never
//! runs.

use std::sync::Mutex;

use orbit_common::OrbitError;
use orbit_engine::{JobOutcome, ReviewReportCorrectionRequest};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{ReviewEvidenceHold, ReviewEvidenceKind, ReviewEvidenceRequirement};
use serde_json::json;

use super::*;
use crate::git_fixture;

const DENIED_CHECK: &str = r#"{"schemaVersion":1,"status":"failed","result":{"summary":"Reviewed the PR; repaired one doc line."},"error":{"code":"review_incomplete_denied_check","message":"Required website inline-code layout check was denied on this host; verdict is incomplete and an authorized validator must run it before acceptance."}}"#;
const RUN: &str = "denied-check-run";
const WEBSITE_CHECK: &str = "make website-inline-code-check";

/// What the host finds in the failed reviewer's report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Report {
    /// A valid incomplete report whose only gap is the named check.
    EvidenceOnly,
    /// Anything else: missing, mismatched, a reject, an open finding, a gap
    /// beyond named evidence.
    NotEvidenceOnly,
}

struct DeniedCheckHost {
    cli: ScriptHost,
    report: Report,
    /// The reviewed worktree the scripted `worktree` step assigns.
    workspace: String,
    /// The registered primary checkout the worktree belongs to.
    primary: PathBuf,
    calls: Mutex<Vec<(String, Value)>>,
    judged: Mutex<Vec<ReviewReportCorrectionRequest>>,
}

impl DeniedCheckHost {
    fn new(cli: ScriptHost, report: Report, primary: &Path, workspace: &Path) -> Self {
        Self {
            cli,
            report,
            workspace: workspace.display().to_string(),
            primary: primary.to_path_buf(),
            calls: Mutex::default(),
            judged: Mutex::default(),
        }
    }

    fn actions(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .map(|(action, _)| action.clone())
            .collect()
    }
}

fn held() -> ReviewEvidenceHold {
    ReviewEvidenceHold {
        schema_version: 1,
        attempt_id: "rvw-landing".to_string(),
        lineage_key: "lineage-1".to_string(),
        run_id: RUN.to_string(),
        candidate: SourceRevision {
            commit: "0123456789abcdef0123456789abcdef01234567".to_string(),
            tree: "89abcdef0123456789abcdef0123456789abcdef".to_string(),
        },
        task_meaning_digest: "meaning".to_string(),
        requirements: vec![ReviewEvidenceRequirement {
            kind: ReviewEvidenceKind::NativeOs,
            name: "Website inline-code layout".to_string(),
            command: WEBSITE_CHECK.to_string(),
            artifact: "evidence/website-inline-code.json".to_string(),
            os: None,
            hosted_check: None,
        }],
        task_spec_digest: None,
        published_ref: None,
        candidate_carry: None,
    }
}

impl orbit_engine::test_support::NullRuntimeHost for DeniedCheckHost {
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        RuntimeHost::agent_subprocess_environment(&self.cli, required_env_vars)
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.calls
            .lock()
            .expect("call log")
            .push((action.to_string(), input.clone()));
        match action {
            "worktree" => Ok(json!({"job_run_id": RUN, "workspace_path": self.workspace})),
            "commit" => Ok(json!({"skipped_no_diff_expected": false})),
            "review_gate_admit" => Ok(json!({
                "applies": true,
                "decision": "admitted",
                "first_task_id": "T-1",
                "attempt_id": "rvw-landing",
                "lineage_key": "lineage-1",
                "manifest_artifact": "review-manifest.json",
                "report_artifact": "review-report.json",
                "reviewer": {"crew": ""},
                "timing": "before-landing",
            })),
            // Settlement decides the hold from the report, as the gate does
            // for any evidence-only verdict.
            "review_gate_settle" => Err(DispatchError::ReviewEvidenceHold(Box::new(held()))),
            other => Err(DispatchError::DeterministicActionRefused {
                action: other.to_string(),
                message: format!("`{other}` is not part of this fixture"),
            }),
        }
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        RuntimeHost::resolve_cli_executor(&self.cli, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        let mut context = RuntimeHost::tool_context_for_activity(
            &self.cli,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        );
        context.workspace_root = Some(self.primary.clone());
        context
    }

    fn record_reviewer_invocation(
        &self,
        _request: &orbit_engine::ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        Ok(None)
    }

    fn review_report_evidence_hold(
        &self,
        request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        self.judged.lock().expect("judged").push(request.clone());
        Ok(match self.report {
            Report::EvidenceOnly => Some(format!("`{WEBSITE_CHECK}`")),
            Report::NotEvidenceOnly => None,
        })
    }
}

fn scripted(action: &str) -> ActivityV2 {
    ActivityV2 {
        description: String::new(),
        input_schema_json: Value::Null,
        output_schema_json: Value::Null,
        fs_profile: None,
        spec: ActivityV2Spec::Deterministic(DeterministicSpec {
            action: action.to_string(),
            config: Value::Null,
        }),
    }
}

/// The shipped before-landing gate, after scripted `worktree` and `commit`
/// steps. The reviewer's retry backoff is shortened; nothing else changes.
fn shipped_landing_gate() -> orbit_types::workflow::JobV2 {
    let shipped = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../orbit-core/assets/jobs/task_pr_pipeline.yaml"),
    )
    .expect("read the shipped PR pipeline");
    let shipped = load_job_asset(&shipped)
        .expect("the shipped PR pipeline loads")
        .spec;
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2, "kind": "Job", "metadata": {"name": "denied_check_fixture"},
            "spec": {"state": "enabled", "kind": "workflow", "steps": [
                {"id": "worktree", "spec": {"type": "deterministic", "action": "worktree", "config": {}}},
                {"id": "commit", "spec": {"type": "deterministic", "action": "commit", "config": {}}},
            ]}
        })
        .to_string(),
    )
    .expect("fixture steps load")
    .spec;
    job.default_input = shipped.default_input.clone();
    for id in [
        "landing_review_gate_admit",
        "landing_review",
        "landing_review_gate_settle",
    ] {
        let mut step = shipped
            .steps
            .iter()
            .find(|step| step.id == id)
            .cloned()
            .unwrap_or_else(|| panic!("the shipped PR pipeline has `{id}`"));
        if let Some(retry) = step.retry.as_mut() {
            retry.initial_backoff_ms = 1;
            retry.backoff_cap_ms = 1;
        }
        job.steps.push(step);
    }
    let mut catalog = V2ActivityCatalog::new();
    for name in [
        "review_gate_admit",
        "review_gate_settle",
        "step_failure_recovery",
    ] {
        catalog.insert(name, scripted(name));
    }
    catalog.insert(
        "agent_review_repair",
        ActivityV2 {
            description: "substitute reviewer".to_string(),
            input_schema_json: Value::Null,
            output_schema_json: Value::Null,
            fs_profile: None,
            spec: ActivityV2Spec::AgentLoop(cli_agent_loop_spec(None)),
        },
    );
    resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("the shipped gate steps resolve");
    job
}

/// Run the gate with a reviewer that repairs one file, declares the denied
/// check and exits 0. Returns the outcome, the host, the reviewer's count of
/// invocations and its repaired file.
fn run_denied_check(report: Report) -> (JobOutcome, DeniedCheckHost, u32, String) {
    let scratch = tempfile::tempdir().unwrap();
    let count_path = scratch.path().join("count");
    let primary = scratch.path().join("primary");
    fs::create_dir_all(&primary).unwrap();
    git_fixture::init(&primary);
    fs::write(primary.join("docs.md"), "doc line\n").unwrap();
    git_fixture::run(&primary, &["add", "docs.md"]);
    git_fixture::run(&primary, &["commit", "-q", "-m", "candidate"]);
    let workspace = scratch.path().join("worktree");
    git_fixture::run(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "candidate",
            &workspace.display().to_string(),
        ],
    );
    let workspace = workspace.canonicalize().unwrap();
    let primary = primary.canonicalize().unwrap();
    let repair_path = workspace.join("docs.md");
    let fake = fake_cli(
        "claude",
        &format!(
            "#!/bin/sh\ncat > /dev/null\nn=$(cat '{count}' 2>/dev/null || echo 0)\nn=$((n+1))\necho \"$n\" > '{count}'\necho 'repaired doc line' > '{repair}'\ncat <<'PAYLOAD'\n{DENIED_CHECK}\nPAYLOAD\nexit 0\n",
            count = count_path.display(),
            repair = repair_path.display(),
        ),
    )
    .unwrap();
    let host = DeniedCheckHost::new(
        ScriptHost::new(fake.cli_path()),
        report,
        &primary,
        &workspace,
    );
    let (writer, _store) = build_writer(scratch.path(), RUN).unwrap();
    let outcome = execute_job_with_resume(
        &shipped_landing_gate(),
        json!({"task_ids": ["T-1"], "base_branch": "main", "base_sync": "remote"}),
        RUN,
        writer,
        &host,
        None,
    )
    .expect("the job runs to an outcome");
    let count = fs::read_to_string(&count_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let repaired = fs::read_to_string(&repair_path).unwrap();
    (outcome, host, count, repaired)
}

#[test]
fn a_reviewer_denied_a_required_check_holds_for_its_named_evidence() {
    git_fixture::isolated(
        module_path!(),
        "a_reviewer_denied_a_required_check_holds_for_its_named_evidence",
        denied_check_holds,
    );
}

fn denied_check_holds() {
    let (outcome, host, invocations, repaired) = run_denied_check(Report::EvidenceOnly);

    assert!(!outcome.success, "a held run does not succeed: {outcome:?}");
    assert_eq!(
        outcome.evidence_hold,
        Some(held()),
        "the run holds on settlement's evidence hold, not a failure: {outcome:?}"
    );
    assert_eq!(
        host.actions(),
        [
            "worktree",
            "commit",
            "review_gate_admit",
            "review_gate_settle"
        ],
        "settlement runs for the same admitted attempt; no recovery runs"
    );
    assert_eq!(invocations, 1, "the reviewer is not retried");
    assert_eq!(
        repaired, "repaired doc line\n",
        "the reviewer's uncommitted repair stays for settlement"
    );
    let judged = host.judged.lock().unwrap().clone();
    assert_eq!(judged.len(), 1);
    assert_eq!(judged[0].attempt_id, "rvw-landing");
    assert_eq!(judged[0].lineage_key, "lineage-1");
    assert_eq!(judged[0].task_ids, ["T-1"]);
    let reviewer = &outcome.pipeline["landing_review"]["reviewer_declared_failure"];
    assert_eq!(reviewer["code"], "review_incomplete_denied_check");
    assert_eq!(reviewer["named_evidence"], format!("`{WEBSITE_CHECK}`"));
    let settle = &host.calls.lock().unwrap()[3].1;
    assert_eq!(settle["admission"]["attempt_id"], "rvw-landing");
}

#[test]
fn a_failed_reviewer_without_an_evidence_only_report_still_fails_the_step() {
    git_fixture::isolated(
        module_path!(),
        "a_failed_reviewer_without_an_evidence_only_report_still_fails_the_step",
        denied_check_without_evidence_fails,
    );
}

fn denied_check_without_evidence_fails() {
    {
        let report = Report::NotEvidenceOnly;
        let (outcome, host, invocations, _) = run_denied_check(report);

        assert!(!outcome.success, "{report:?}: {outcome:?}");
        assert!(outcome.evidence_hold.is_none(), "{report:?}: {outcome:?}");
        assert!(
            !host
                .actions()
                .iter()
                .any(|action| action == "review_gate_settle"),
            "{report:?}: settlement never sees a failed reviewer: {:?}",
            host.actions()
        );
        let message = outcome.message.clone().unwrap_or_default();
        assert!(
            message.contains("review_incomplete_denied_check"),
            "{report:?}: the declared failure stays the step's outcome: {message}"
        );
        assert!(invocations >= 1, "{report:?}");
    }
}
