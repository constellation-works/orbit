use crate::command::{CommandOut, Execute, Payload};
use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

/// Reconcile the already-merged head of a recovered follower delivery.
///
/// A handed-off pull request that merged at a head other than its candidate
/// carries no validation or review for that head. Reconciliation runs the
/// owner's required validation and an independent read-only review of exactly
/// the merged head and records owner-held evidence a desktop review can
/// complete from. Operator-only.
#[derive(Args)]
pub struct TaskReconcileReviewCommand {
    #[command(flatten)]
    pub(crate) routing: super::command::TaskHostArgs,
    #[command(subcommand)]
    pub command: TaskReconcileReviewSubcommand,
}

#[derive(Subcommand)]
pub enum TaskReconcileReviewSubcommand {
    /// Show whether the task's merged delivery can be reconciled, and why not
    Inspect(ReconcileInspectArgs),
    /// Admit a reconciliation run; resubmitting the same key replays it
    Submit(ReconcileSubmitArgs),
    /// Show reconciliation outcomes, their runs and the next step
    Status(ReconcileStatusArgs),
    /// Record an audited disposition of a failure the base already had
    AcceptBaseline(ReconcileAcceptBaselineArgs),
}

impl TaskReconcileReviewSubcommand {
    pub(crate) fn task_id(&self) -> &str {
        match self {
            Self::Inspect(args) => &args.id,
            Self::Submit(args) => &args.id,
            Self::Status(args) => &args.id,
            Self::AcceptBaseline(args) => &args.id,
        }
    }
}

#[derive(Args)]
pub struct ReconcileInspectArgs {
    /// Task in review whose foreign delivery merged
    pub id: String,
}

#[derive(Args)]
pub struct ReconcileSubmitArgs {
    /// Task in review whose foreign delivery merged
    pub id: String,
    /// Operator-chosen request key; resubmitting it replays the same reconciliation
    #[arg(long)]
    pub request: String,
}

#[derive(Args)]
pub struct ReconcileStatusArgs {
    /// Task whose reconciliations to show
    pub id: String,
    /// Show only this reconciliation
    #[arg(long)]
    pub reconciliation: Option<String>,
}

#[derive(Args)]
pub struct ReconcileAcceptBaselineArgs {
    /// Task the reconciliation belongs to
    pub id: String,
    /// Reconciliation waiting for a baseline disposition
    #[arg(long)]
    pub reconciliation: String,
    /// Required command whose failure reproduced at the base
    #[arg(long)]
    pub command: String,
    /// Commit on the landing branch, landed on top of the delivery's merge or
    /// squash commit, that remediates the failure
    #[arg(long)]
    pub remediation: String,
    /// Why the baseline failure is accepted; recorded with the disposition
    #[arg(long)]
    pub reason: String,
}

impl TaskReconcileReviewSubcommand {
    /// The `orbit.task.reconcile_review` input, without a workspace selector.
    pub(crate) fn tool_input(&self) -> Value {
        let mut input = match self {
            Self::Inspect(args) => json!({"action": "inspect", "id": args.id}),
            Self::Submit(args) => {
                json!({"action": "submit", "id": args.id, "request_key": args.request})
            }
            Self::Status(args) => json!({
                "action": "status",
                "id": args.id,
                "reconciliation_id": args.reconciliation,
            }),
            Self::AcceptBaseline(args) => json!({
                "action": "accept_baseline",
                "id": args.id,
                "reconciliation_id": args.reconciliation,
                "command": args.command,
                "remediation_commit": args.remediation,
                "reason": args.reason,
            }),
        };
        if let Value::Object(object) = &mut input {
            object.retain(|_, value| !value.is_null());
        }
        input
    }
}

impl Execute for TaskReconcileReviewCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut input = self.command.tool_input();
        if let Value::Object(object) = &mut input {
            object.insert("workspace".into(), json!(runtime.paths().repo_root));
        }
        let value = runtime.run_tool("orbit.task.reconcile_review", input)?;
        let text = reconciliation_text(&value);
        Ok(Payload::detail(value, text).into())
    }
}

pub(crate) fn reconciliation_text(value: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(eligible) = value.get("eligible").and_then(Value::as_bool) {
        lines.push(format!("Eligible: {eligible}"));
        for (label, pointer) in [
            ("Refusal", "/refusal"),
            ("Next step", "/next_step"),
            ("Run", "/binding/execution/run_id"),
            ("Host", "/binding/execution/machine_id"),
            ("Claim", "/binding/execution/claim_id"),
            ("Handoff", "/binding/execution/handoff_id"),
            ("Candidate", "/binding/execution/candidate_commit"),
            ("Pull request", "/binding/pull_request/number"),
            ("URL", "/binding/pull_request/url"),
            ("Repository", "/binding/pull_request/repository"),
            ("Landing branch", "/binding/pull_request/landing_branch"),
            ("Merged head", "/binding/pull_request/merged_head/commit"),
            ("Base", "/binding/pull_request/base/commit"),
            ("Landed commit", "/binding/pull_request/landed/commit"),
            ("Commands source", "/contract/commands_source"),
            ("Accepted commands", "/contract/accepted_commands"),
            ("Required commands", "/contract/required_commands"),
            ("Review crew", "/contract/review_crew"),
            ("Review crew source", "/contract/review_crew_source"),
            ("Frozen at", "/contract/frozen_at"),
        ] {
            if let Some(field) = value.pointer(pointer).filter(|field| !field.is_null()) {
                let text = field
                    .as_str()
                    .map_or_else(|| field.to_string(), str::to_owned);
                lines.push(format!("{label}: {text}"));
            }
        }
    }
    if let Some(records) = value.get("reconciliations").and_then(Value::as_array) {
        if records.is_empty() {
            lines.push("No reconciliations.".into());
        }
        lines.extend(records.iter().map(reconciliation_line));
    } else if value.get("reconciliation_id").is_some() {
        lines.push(reconciliation_line(value));
    }
    lines.join("\n")
}

fn reconciliation_line(value: &Value) -> String {
    let field = |name| value.get(name).and_then(Value::as_str);
    let mut line = format!(
        "{}: {}",
        field("reconciliation_id").unwrap_or("-"),
        field("outcome").unwrap_or("unsettled"),
    );
    if let Some(run) = field("run_id") {
        line.push_str(&format!("; run: {run}"));
        if let Some(state) = field("run_state") {
            line.push_str(&format!(" ({state})"));
        }
    }
    if value.get("replayed").and_then(Value::as_bool) == Some(true) {
        line.push_str("; replayed");
    }
    if let Some(next) = field("next_step") {
        line.push_str(&format!("; next step: {next}"));
    }
    line
}
