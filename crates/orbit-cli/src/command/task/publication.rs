use clap::{Args, Subcommand, ValueEnum};
use orbit_core::Core;
use orbit_core::app::task::{self, Command, Outcome, Publication, PublicationSource};
use orbit_core::app::task::{
    AttachmentPolicy, AttachmentPolicyKind, PublicationFreshness, PublicationInspection,
    PublicationPublishOutcome, PublicationPublishStatus, PublicationRecoveryCompleteness,
    PublicationRenderAuthority, ScannerFailureBehavior,
};
use orbit_types::workspace::{
    DEFAULT_PUBLICATION_BRANCH, WorkspacePublicationBinding, redact_git_remote,
};
use serde_json::{Value, json};

use crate::command::{CommandOut, ExecuteCore, Payload, require_confirmation};

const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_DENY_PATTERNS: [&str; 6] = [
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.key",
    "**/id_rsa",
    "**/credentials.json",
];

#[derive(Args)]
#[command(about = "Operate the explicit task-publication and recovery workflow")]
pub struct TaskPublicationCommand {
    #[command(subcommand)]
    pub command: TaskPublicationSubcommand,
}

#[derive(Subcommand)]
pub enum TaskPublicationSubcommand {
    /// Explicitly publish the selected owned workspace's validated tasks
    Publish(TaskPublicationPublishArgs),
    /// Compare the owner-local last-success record with the validated branch tip
    Status(TaskPublicationStatusArgs),
    /// Read and validate a labelled snapshot without adopting live task state
    Inspect(TaskPublicationInspectArgs),
    /// Deliberately restore a same-authority snapshot into the selected workspace
    Restore(TaskPublicationRestoreArgs),
}

impl ExecuteCore for TaskPublicationCommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self.command {
            TaskPublicationSubcommand::Publish(args) => args.execute(core),
            TaskPublicationSubcommand::Status(args) => args.execute(core),
            TaskPublicationSubcommand::Inspect(args) => args.execute(core),
            TaskPublicationSubcommand::Restore(args) => args.execute(core),
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliAttachmentPolicy {
    Fail,
    Include,
    Omit,
}

impl From<CliAttachmentPolicy> for AttachmentPolicyKind {
    fn from(value: CliAttachmentPolicy) -> Self {
        match value {
            CliAttachmentPolicy::Fail => Self::Fail,
            CliAttachmentPolicy::Include => Self::Include,
            CliAttachmentPolicy::Omit => Self::Omit,
        }
    }
}

#[derive(Args)]
pub struct TaskPublicationPublishArgs {
    /// Attachment exposure policy. `fail` is the safe default.
    #[arg(long = "attachments", value_enum, default_value_t = CliAttachmentPolicy::Fail)]
    attachments: CliAttachmentPolicy,
    /// Maximum bytes admitted for one included attachment.
    #[arg(long, default_value_t = DEFAULT_MAX_FILE_BYTES)]
    max_file_bytes: u64,
    /// Maximum total bytes admitted for included attachments.
    #[arg(long, default_value_t = DEFAULT_MAX_TOTAL_BYTES)]
    max_total_bytes: u64,
    /// Additional attachment path glob to reject (repeatable).
    #[arg(long = "deny-pattern")]
    deny_patterns: Vec<String>,
    /// Deliberately permit `include` when no sensitivity scanner is configured.
    #[arg(long, requires = "attachments")]
    allow_unscanned_attachments: bool,
}

impl ExecuteCore for TaskPublicationPublishArgs {
    fn execute(self, core: &Core) -> CommandOut {
        if self.allow_unscanned_attachments
            && !matches!(self.attachments, CliAttachmentPolicy::Include)
        {
            return Err(orbit_common::OrbitError::InvalidInput(
                "--allow-unscanned-attachments is valid only with --attachments include"
                    .to_string(),
            ));
        }
        let mut deny_patterns = DEFAULT_DENY_PATTERNS
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect::<Vec<_>>();
        deny_patterns.extend(self.deny_patterns);
        let policy = AttachmentPolicy {
            kind: self.attachments.into(),
            max_file_bytes: self.max_file_bytes,
            max_total_bytes: self.max_total_bytes,
            deny_patterns,
            scanner_failure_behavior: if self.allow_unscanned_attachments {
                ScannerFailureBehavior::AllowUnchecked
            } else {
                ScannerFailureBehavior::Reject
            },
        };
        let (binding, outcome) =
            *task::exec(core, Command::Publication(Publication::Publish { policy }))
                .and_then(Outcome::into_published)?;

        Ok(Payload::detail(
            publish_json(&binding, &outcome),
            format_publish(&binding, &outcome),
        )
        .into())
    }
}

#[derive(Args)]
pub struct TaskPublicationStatusArgs {}

impl ExecuteCore for TaskPublicationStatusArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let (binding, inspection) = *task::exec(core, Command::Publication(Publication::Status))
            .and_then(Outcome::into_publication_status)?;
        let (Some(local_commit), Some(inspection)) =
            (binding.last_success_commit.as_deref(), inspection)
        else {
            return Ok(Payload::detail(
                json!({
                    "workspace_id": binding.workspace_id,
                    "publication_id": binding.publication_id,
                    "state": "never-published",
                    "last_success_generation": Value::Null,
                    "last_success_commit": Value::Null,
                    "publication_remote": redact_git_remote(&binding.publication_remote),
                    "privacy": "operator-managed",
                }),
                format!(
                    "publication '{}' for workspace '{}' has no recorded successful snapshot\nprivacy: operator-managed",
                    binding.publication_id, binding.workspace_id
                ),
            )
            .into());
        };

        let state = if inspection.label.commit_id == local_commit {
            "current"
        } else {
            "authority-conflict"
        };
        Ok(Payload::detail(
            json!({
                "workspace_id": binding.workspace_id,
                "publication_id": binding.publication_id,
                "state": state,
                "last_success_generation": binding.last_success_generation,
                "last_success_commit": local_commit,
                "remote_generation": inspection.label.generation,
                "remote_commit": inspection.label.commit_id,
                "incomplete_attachments": inspection.label.incomplete_attachments,
                "publication_remote": redact_git_remote(&binding.publication_remote),
                "privacy": "operator-managed",
            }),
            format!(
                "state:           {state}\nworkspace:       {}\npublication:     {}\nlocal_commit:    {}\nremote_commit:   {}\nremote_generation: {}\nincomplete:      {}\nprivacy:         operator-managed",
                binding.workspace_id,
                binding.publication_id,
                local_commit,
                inspection.label.commit_id,
                inspection.label.generation,
                inspection.label.incomplete_attachments,
            ),
        )
        .into())
    }
}

#[derive(Args, Clone)]
struct PublicationConsumerArgs {
    /// Expected logical workspace id carried by the publication.
    #[arg(long)]
    workspace_id: String,
    /// Expected portable source-repository remote identity.
    #[arg(long = "source-remote", value_name = "URL")]
    source_repository_fingerprint: String,
    /// Expected opaque publication lineage identifier.
    #[arg(long)]
    publication_id: String,
    /// Expected authority machine id.
    #[arg(long)]
    authority_machine_id: String,
    /// Dedicated publication repository URL.
    #[arg(long, value_name = "URL")]
    remote: String,
    /// Ordinary publication branch (short name or refs/heads/*).
    #[arg(long, default_value = DEFAULT_PUBLICATION_BRANCH)]
    branch: String,
    /// Inspect or restore this exact commit instead of the current branch tip.
    #[arg(long)]
    commit: Option<String>,
}

impl PublicationConsumerArgs {
    fn source(self) -> PublicationSource {
        PublicationSource {
            workspace_id: self.workspace_id,
            source_repository_fingerprint: self.source_repository_fingerprint,
            publication_id: self.publication_id,
            authority_machine_id: self.authority_machine_id,
            remote: self.remote,
            branch: self.branch,
            commit: self.commit,
        }
    }
}

#[derive(Args)]
pub struct TaskPublicationInspectArgs {
    #[command(flatten)]
    publication: PublicationConsumerArgs,
}

impl ExecuteCore for TaskPublicationInspectArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let inspection = task::exec(
            core,
            Command::Publication(Publication::Inspect(self.publication.source())),
        )
        .and_then(Outcome::into_inspected)?;
        Ok(Payload::detail(inspection_json(&inspection), format_inspection(&inspection)).into())
    }
}

#[derive(Args)]
pub struct TaskPublicationRestoreArgs {
    #[command(flatten)]
    publication: PublicationConsumerArgs,
    /// Permit an idempotent retry only when every colliding bundle is byte-identical.
    #[arg(long)]
    allow_identical_retry: bool,
    /// Confirm deliberate mutation of the destination canonical task store.
    #[arg(long)]
    pub confirm: bool,
}

impl ExecuteCore for TaskPublicationRestoreArgs {
    fn execute(self, core: &Core) -> CommandOut {
        require_confirmation(
            self.confirm,
            "restoring a task publication into the canonical destination store",
        )?;
        let outcome = task::exec(
            core,
            Command::Publication(Publication::Restore {
                source: self.publication.source(),
                allow_identical_retry: self.allow_identical_retry,
            }),
        )
        .and_then(Outcome::into_restored)?;
        let completeness = match outcome.completeness {
            PublicationRecoveryCompleteness::Complete => "complete",
            PublicationRecoveryCompleteness::IncompleteAttachments => "incomplete-attachments",
        };
        let omitted = outcome
            .omitted_attachments
            .iter()
            .map(|entry| {
                json!({
                    "task_id": entry.task_id,
                    "path": entry.path,
                    "size_bytes": entry.size_bytes,
                    "sha256": entry.sha256,
                })
            })
            .collect::<Vec<_>>();
        Ok(Payload::detail(
            json!({
                "workspace_id": outcome.workspace_id,
                "publication_id": outcome.publication_id,
                "generation": outcome.generation,
                "restored_task_ids": outcome.restored_task_ids,
                "already_present_task_ids": outcome.already_present_task_ids,
                "completeness": completeness,
                "omitted_attachments": omitted,
            }),
            format!(
                "restored publication '{}' generation {} into workspace '{}'\nrestored: {}\nalready present: {}\ncompleteness: {}\nomitted attachments: {}",
                outcome.publication_id,
                outcome.generation,
                outcome.workspace_id,
                outcome.restored_task_ids.len(),
                outcome.already_present_task_ids.len(),
                completeness,
                outcome.omitted_attachments.len(),
            ),
        )
        .into())
    }
}

fn publication_status_label(status: PublicationPublishStatus) -> &'static str {
    match status {
        PublicationPublishStatus::Initialized => "initialized",
        PublicationPublishStatus::Advanced => "advanced",
        PublicationPublishStatus::Unchanged => "unchanged",
        PublicationPublishStatus::Reconciled => "reconciled",
    }
}

fn publish_json(
    binding: &WorkspacePublicationBinding,
    outcome: &PublicationPublishOutcome,
) -> Value {
    json!({
        "status": publication_status_label(outcome.status),
        "workspace_id": binding.workspace_id,
        "publication_id": binding.publication_id,
        "publication_remote": redact_git_remote(&binding.publication_remote),
        "branch": outcome.branch,
        "commit_id": outcome.commit_id,
        "generation": outcome.generation,
        "previous_publication": outcome.previous_publication,
        "observed_tip": outcome.observed_tip,
        "included_attachment_bytes": outcome.included_attachment_bytes,
        "omitted_attachment_bytes": outcome.omitted_attachment_bytes,
        "privacy": "operator-managed",
    })
}

fn format_publish(
    binding: &WorkspacePublicationBinding,
    outcome: &PublicationPublishOutcome,
) -> String {
    format!(
        "status:      {}\nworkspace:   {}\npublication: {}\nbranch:      {}\ncommit:      {}\ngeneration:  {}\nincluded:    {} bytes\nomitted:     {} bytes\nprivacy:     operator-managed",
        publication_status_label(outcome.status),
        binding.workspace_id,
        binding.publication_id,
        outcome.branch,
        outcome.commit_id,
        outcome.generation,
        outcome.included_attachment_bytes,
        outcome.omitted_attachment_bytes,
    )
}

fn inspection_json(inspection: &PublicationInspection) -> Value {
    let freshness = match inspection.label.freshness {
        PublicationFreshness::Current => "current",
        PublicationFreshness::Stale => "stale",
    };
    let render_authority = match inspection.label.render_authority {
        PublicationRenderAuthority::Snapshot => "snapshot",
    };
    let tasks = inspection
        .tasks
        .iter()
        .map(|task| {
            json!({
                "task": task.task,
                "description": task.description,
                "acceptance": task.acceptance,
                "plan": task.plan,
                "execution_summary": task.execution_summary,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "label": {
            "published_at": inspection.label.published_at,
            "generation": inspection.label.generation,
            "workspace_id": inspection.label.workspace_id,
            "source_repository_fingerprint": redact_git_remote(&inspection.label.source_repository_fingerprint),
            "authority_machine_id": inspection.label.authority_machine_id,
            "publication_id": inspection.label.publication_id,
            "commit_id": inspection.label.commit_id,
            "incomplete_attachments": inspection.label.incomplete_attachments,
            "freshness": freshness,
            "render_authority": render_authority,
        },
        "git_parent": inspection.git_parent,
        "attachment_policy": attachment_policy_label(inspection.envelope.attachment_policy),
        "omitted_attachments": inspection.envelope.omitted_attachments,
        "tasks": tasks,
    })
}

fn attachment_policy_label(policy: AttachmentPolicyKind) -> &'static str {
    match policy {
        AttachmentPolicyKind::Fail => "fail",
        AttachmentPolicyKind::Include => "include",
        AttachmentPolicyKind::Omit => "omit",
    }
}

fn format_inspection(inspection: &PublicationInspection) -> String {
    let freshness = match inspection.label.freshness {
        PublicationFreshness::Current => "current",
        PublicationFreshness::Stale => "stale",
    };
    let mut output = format!(
        "publication snapshot (not live state)\nworkspace:   {}\npublication: {}\ngeneration:  {}\ncommit:      {}\nfreshness:   {}\npublished:   {}\nauthority:   {}\nincomplete:  {}\ntasks:       {}",
        inspection.label.workspace_id,
        inspection.label.publication_id,
        inspection.label.generation,
        inspection.label.commit_id,
        freshness,
        inspection.label.published_at,
        inspection.label.authority_machine_id,
        inspection.label.incomplete_attachments,
        inspection.tasks.len(),
    );
    for task in &inspection.tasks {
        output.push_str(&format!("\n  {}  {}", task.task.id, task.task.title));
    }
    output
}
