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
    /// Landed commit on the landing branch that remediates the failure
    #[arg(long)]
    pub remediation: String,
    /// Why the baseline failure is accepted; recorded with the disposition
    #[arg(long)]
    pub reason: String,
}

impl Execute for TaskReconcileReviewCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let (input, message) = match self.command {
            TaskReconcileReviewSubcommand::Inspect(args) => (
                json!({"action": "inspect", "id": args.id}),
                "Reconciliation eligibility for the task's merged delivery.",
            ),
            TaskReconcileReviewSubcommand::Submit(args) => (
                json!({"action": "submit", "id": args.id, "request_key": args.request}),
                "Reconciliation admitted; follow it with `orbit task reconcile-review status`.",
            ),
            TaskReconcileReviewSubcommand::Status(args) => (
                json!({
                    "action": "status",
                    "id": args.id,
                    "reconciliation_id": args.reconciliation,
                }),
                "Reconciliations of the task, newest first.",
            ),
            TaskReconcileReviewSubcommand::AcceptBaseline(args) => (
                json!({
                    "action": "accept_baseline",
                    "id": args.id,
                    "reconciliation_id": args.reconciliation,
                    "command": args.command,
                    "remediation_commit": args.remediation,
                    "reason": args.reason,
                }),
                "Baseline disposition recorded; validation of the merged head stays incomplete.",
            ),
        };
        let mut input = input;
        if let Value::Object(object) = &mut input {
            object.retain(|_, value| !value.is_null());
            object.insert("workspace".into(), json!(runtime.paths().repo_root));
        }
        let value = runtime.run_tool("orbit.task.reconcile_review", input)?;
        Ok(Payload::detail(value, message).into())
    }
}
