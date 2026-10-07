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
    /// Commit on the landing branch, landed on top of the delivery's merge or
    /// squash commit, that remediates the failure
    #[arg(long)]
    pub remediation: String,
    /// Why the baseline failure is accepted; recorded with the disposition
    #[arg(long)]
    pub reason: String,
}

impl Execute for TaskReconcileReviewCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let input = match self.command {
            TaskReconcileReviewSubcommand::Inspect(args) => {
                json!({"action": "inspect", "id": args.id})
            }
            TaskReconcileReviewSubcommand::Submit(args) => {
                json!({"action": "submit", "id": args.id, "request_key": args.request})
            }
            TaskReconcileReviewSubcommand::Status(args) => json!({
                "action": "status",
                "id": args.id,
                "reconciliation_id": args.reconciliation,
            }),
            TaskReconcileReviewSubcommand::AcceptBaseline(args) => json!({
                "action": "accept_baseline",
                "id": args.id,
                "reconciliation_id": args.reconciliation,
                "command": args.command,
                "remediation_commit": args.remediation,
                "reason": args.reason,
            }),
        };
        let mut input = input;
        if let Value::Object(object) = &mut input {
            object.retain(|_, value| !value.is_null());
            object.insert("workspace".into(), json!(runtime.paths().repo_root));
        }
        let value = runtime.run_tool("orbit.task.reconcile_review", input)?;
        let text = reconciliation_text(&value);
        Ok(Payload::detail(value, text).into())
    }
}

fn reconciliation_text(value: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(eligible) = value.get("eligible").and_then(Value::as_bool) {
        lines.push(format!("Eligible: {eligible}"));
        for field in ["refusal", "binding", "contract", "next_step"] {
            if let Some(detail) = value.get(field).filter(|detail| !detail.is_null()) {
                detail_lines(field, detail, &mut lines);
            }
        }
    }
    if let Some(records) = value.get("reconciliations").and_then(Value::as_array) {
        if records.is_empty() {
            lines.push("No reconciliations recorded.".into());
        }
        lines.extend(records.iter().map(reconciliation_line));
    } else if value.get("reconciliation_id").is_some() {
        lines.push(reconciliation_line(value));
    }
    lines.join("\n")
}

fn reconciliation_line(record: &Value) -> String {
    let mut fields = Vec::new();
    for field in [
        "reconciliation_id",
        "outcome",
        "run_id",
        "run_state",
        "next_step",
    ] {
        let text = match record.get(field) {
            Some(Value::Null) | None if field == "outcome" => "pending".into(),
            Some(Value::Null) | None => continue,
            Some(value) => display_value(value),
        };
        fields.push(format!("{field}: {text}"));
    }
    fields.join("; ")
}

fn detail_lines(prefix: &str, value: &Value, lines: &mut Vec<String>) {
    if let Value::Object(fields) = value {
        for (field, detail) in fields {
            detail_lines(&format!("{prefix}.{field}"), detail, lines);
        }
    } else {
        lines.push(format!("{prefix}: {}", display_value(value)));
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t"),
        other => other.to_string(),
    }
}
