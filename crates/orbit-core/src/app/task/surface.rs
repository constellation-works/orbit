//! Task commands from a command line or a server: each command's operation
//! identity, the registry verbs sent as fields, and the hand-written task
//! commands. Every flow calls the runtime method the surface called before.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::task::TaskVerb;
use orbit_registry::workspace_registry;
use orbit_tools::ToolContext;
use orbit_types::task::{TaskType, TaskWriteProvenance, TaskWriteSurface};
use serde_json::{Value, json};

use super::{
    Archive, Command, Outcome, Publication, Read, Rechecked, SettleMerged, Shown, Written,
};
use crate::app::exec::Caller;
use crate::app::exec::Operation;
use crate::application::contracts::ToolInvocationPort;
use crate::application::task::{
    TaskWriteMetadata, TaskWriteResponsePolicy, task_write_response_from_tool_answer,
    task_write_response_json, with_task_write_provenance,
};
use crate::{Core, OrbitRuntime};

/// The operation `command` performs: the names its governance rows and its
/// audit row are keyed on. The audit names are the ones the command line
/// recorded for the same command.
pub(super) fn operation(command: &Command<'_>) -> Operation {
    let unlisted = |subcommand: &str| Operation::command("task", subcommand);
    match command {
        Command::Operation(verb, input) => {
            let fields = input.fields();
            let operation = Operation::of(*verb, fields).without_tool_row();
            match verb {
                // A lock listing or reservation names no task.
                TaskVerb::LocksList | TaskVerb::LocksReserve => operation.target(None, None),
                TaskVerb::LocksRelease => operation.target(
                    Some("reservation"),
                    fields
                        .get("reservation_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                ),
                TaskVerb::Add
                | TaskVerb::Update
                | TaskVerb::ArtifactPut
                | TaskVerb::ArtifactGet
                | TaskVerb::List
                | TaskVerb::Eligible
                | TaskVerb::Show
                | TaskVerb::Lint
                | TaskVerb::ReviewReset
                | TaskVerb::Repilot
                | TaskVerb::Requeue
                | TaskVerb::ResumeDelivery
                | TaskVerb::BlockTriage
                | TaskVerb::ReconcileReview
                | TaskVerb::RetryHandoff
                | TaskVerb::RecoverClaim
                | TaskVerb::RevokeHandoff
                | TaskVerb::Reject
                | TaskVerb::Delete => operation,
            }
        }
        Command::Decision(verb, input) => Operation::of(*verb, input.fields()).without_tool_row(),
        Command::Archive(archive) => {
            unlisted("archive").target(Some("task"), Some(archive.id.clone()))
        }
        Command::Flow { .. } => unlisted("flow"),
        Command::Contention => unlisted("locks-contention"),
        Command::RecheckBlocked { .. } => unlisted("recheck-blocked"),
        Command::SettleMerged(settle) => match settle {
            SettleMerged::All => unlisted("settle-merged"),
            SettleMerged::Task { id } | SettleMerged::RecoveredClaim { id, .. } => {
                unlisted("settle-merged").target(Some("task"), Some(id.clone()))
            }
        },
        Command::Export { .. } => unlisted("export"),
        Command::Import { archive, .. } => {
            unlisted("import").target(None, Some(archive.to_str().unwrap_or_default().to_string()))
        }
        Command::Reindex { .. } => unlisted("reindex"),
        Command::Publication(publication) => match publication {
            Publication::Publish { .. } => {
                unlisted("publication-publish").target(Some("workspace"), None)
            }
            Publication::Status => unlisted("publication-status").target(Some("workspace"), None),
            Publication::Inspect(_) => {
                unlisted("publication-inspect").target(Some("publication"), None)
            }
            Publication::Restore { .. } => {
                unlisted("publication-restore").target(Some("workspace"), None)
            }
        },
        Command::Record(write) => unlisted(write.verb).target(None, write.target_id.clone()),
        Command::Read(read) => unlisted(match read {
            Read::Pages { .. } => "list",
            Read::Row { .. } | Read::Detail { .. } => "show",
            Read::Rows(_) => "list",
            Read::Locks => "locks-list",
            Read::CompletionByComplexity => "completion-by-complexity",
        }),
    }
}

/// Run `mutate` as the command line's own write.
fn as_command_line_write<T>(
    mutate: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    with_task_write_provenance(
        TaskWriteProvenance::new(Some(TaskWriteSurface::Cli), None),
        mutate,
    )
}

/// One task verb sent as fields: run it through the generic tool entry, as
/// the command line did, then gather what that surface renders beside the
/// tool's answer.
pub(super) fn operation_fields(
    core: &Core,
    verb: TaskVerb,
    mut fields: Value,
) -> Result<Outcome, OrbitError> {
    let runtime = core.runtime();
    let command_line = core.caller() == Caller::CommandLine;
    if command_line && acts_on_this_checkout(verb) {
        // The command line names no workspace: these act on its checkout.
        if let Some(object) = fields.as_object_mut() {
            object.insert("workspace".to_string(), json!(runtime.paths().repo_root));
        }
    }
    let full_record = fields.get("fields").is_none();
    let answer = ToolInvocationPort::run_tool_with_context(
        runtime,
        verb.spec().tool_name,
        fields,
        ToolContext::default(),
    )?;
    match verb {
        TaskVerb::Add | TaskVerb::Update | TaskVerb::ArtifactPut if command_line => {
            let (document, task, metadata) =
                task_write_response_from_tool_answer(runtime, &answer)?;
            Ok(Outcome::Written(Box::new(Written {
                document,
                task,
                warnings: metadata.warnings,
                context_files_unverified: metadata.context_files_unverified,
            })))
        }
        TaskVerb::Show if command_line && full_record => shown(runtime, answer),
        TaskVerb::LocksReserve => Ok(Outcome::Reservation(answer)),
        TaskVerb::Add
        | TaskVerb::Update
        | TaskVerb::ArtifactPut
        | TaskVerb::ArtifactGet
        | TaskVerb::LocksList
        | TaskVerb::LocksRelease
        | TaskVerb::List
        | TaskVerb::Eligible
        | TaskVerb::Show
        | TaskVerb::Lint
        | TaskVerb::ReviewReset
        | TaskVerb::Repilot
        | TaskVerb::Requeue
        | TaskVerb::ResumeDelivery
        | TaskVerb::BlockTriage
        | TaskVerb::ReconcileReview
        | TaskVerb::RetryHandoff
        | TaskVerb::RecoverClaim
        | TaskVerb::RevokeHandoff
        | TaskVerb::Reject
        | TaskVerb::Delete => Ok(Outcome::Answer(answer)),
    }
}

/// The verbs whose command line acts on the checkout it runs in.
fn acts_on_this_checkout(verb: TaskVerb) -> bool {
    match verb {
        TaskVerb::Add
        | TaskVerb::Repilot
        | TaskVerb::ReviewReset
        | TaskVerb::ReconcileReview
        | TaskVerb::Requeue
        | TaskVerb::RecoverClaim
        | TaskVerb::RevokeHandoff => true,
        TaskVerb::Update
        | TaskVerb::ArtifactPut
        | TaskVerb::ArtifactGet
        | TaskVerb::LocksList
        | TaskVerb::LocksReserve
        | TaskVerb::LocksRelease
        | TaskVerb::List
        | TaskVerb::Eligible
        | TaskVerb::Show
        | TaskVerb::Lint
        | TaskVerb::ResumeDelivery
        | TaskVerb::BlockTriage
        | TaskVerb::RetryHandoff
        | TaskVerb::Reject
        | TaskVerb::Delete => false,
    }
}

/// A task's full record with what the command line shows beside it: the
/// registered workspace it was read from, each relation target's status and
/// the run that owns the task.
fn shown(runtime: &OrbitRuntime, answer: Value) -> Result<Outcome, OrbitError> {
    let workspace = match runtime.opened_registered_workspace() {
        Some(opened) => opened.map(|(id, name)| (id.to_owned(), name.to_owned())),
        None => workspace_registry::load_registry_from(&workspace_registry::registry_path_for(
            &runtime.global_root(),
        ))
        .ok()
        .and_then(|registry| {
            workspace_registry::find_workspace_by_path(&registry, &runtime.paths().repo_root)
                .map(|workspace| (workspace.id.clone(), workspace.name.clone()))
        }),
    };
    let targets = answer
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|relation| relation.get("target").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let relation_statuses = if targets.is_empty() {
        BTreeMap::new()
    } else {
        runtime.task_status_index_for(&targets)?
    };
    // The observed read neither reconciles nor changes a stale run.
    let run = answer
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "in-progress" | "in_progress" | "review"))
        .and_then(|_| answer.get("job_run_id").and_then(Value::as_str))
        .filter(|run_id| !run_id.is_empty())
        .map(|run_id| match runtime.show_job_run_observed(run_id) {
            Ok(run) => Some(run),
            Err(error) => {
                tracing::warn!(
                    target: "orbit.cli.task",
                    %run_id,
                    %error,
                    "task run state could not be read"
                );
                None
            }
        });
    Ok(Outcome::Shown(Box::new(Shown {
        answer,
        workspace,
        relation_statuses,
        run,
    })))
}

pub(super) fn archive(runtime: &OrbitRuntime, archive: Archive) -> Result<Outcome, OrbitError> {
    as_command_line_write(|| runtime.archive_task_as_operator(&archive.id, archive.force))?;
    let task = runtime.get_task(&archive.id)?;
    task_write_response_json(
        runtime,
        &task,
        &TaskWriteResponsePolicy::FullRecord,
        &TaskWriteMetadata::default(),
    )
    .map(Outcome::Answer)
}

pub(super) fn flow(
    runtime: &OrbitRuntime,
    tags: &[String],
    task_type: Option<TaskType>,
) -> Result<Outcome, OrbitError> {
    runtime
        .list_tasks_by_tags(tags)?
        .into_iter()
        .filter(|task| task_type.is_none_or(|kind| task.task_type == kind))
        .map(|task| {
            let history = runtime.get_task_history(&task.id)?;
            Ok((task, history))
        })
        .collect::<Result<Vec<_>, OrbitError>>()
        .map(Outcome::Flow)
}

pub(super) fn recheck_blocked(
    runtime: &OrbitRuntime,
    confirm: bool,
) -> Result<Outcome, OrbitError> {
    let found = runtime.infra_blocked_tasks()?;
    let transient = runtime.transient_infra_blocked_tasks()?;
    let (requeued, transient_requeued) = if confirm {
        as_command_line_write(|| {
            Ok((
                runtime.requeue_cleared_infra_blocked_tasks()?,
                runtime.requeue_cleared_transient_infra_blocks()?,
            ))
        })?
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(Outcome::Rechecked(Rechecked {
        found,
        transient,
        requeued,
        transient_requeued,
    }))
}

pub(super) fn settle_merged(
    runtime: &OrbitRuntime,
    settle: SettleMerged,
) -> Result<Outcome, OrbitError> {
    as_command_line_write(|| match &settle {
        SettleMerged::RecoveredClaim {
            id,
            claim,
            pull_request,
            published_head,
        } => Ok(vec![runtime.settle_recovered_claim_merge(
            id,
            claim,
            *pull_request,
            published_head,
        )?]),
        SettleMerged::Task { id } => Ok(vec![runtime.settle_externally_merged_task(id)?]),
        SettleMerged::All => runtime.settle_externally_merged_tasks(),
    })
    .map(Outcome::Settled)
}

/// One decision verb sent as fields. The reads and the channel's own two
/// verbs run through the generic tool entry; the operator's verbs are applied
/// here as this surface's human writer, which no tool call can reach.
pub(super) fn decision_fields(
    core: &Core,
    verb: DecisionVerb,
    fields: Value,
) -> Result<Outcome, OrbitError> {
    let runtime = core.runtime();
    let required = |name: &str| {
        fields
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| OrbitError::InvalidInput(format!("`{name}` is required")))
    };
    let operator = || {
        let label = runtime.actor_label().trim();
        if label.is_empty() {
            "human".to_string()
        } else {
            label.to_string()
        }
    };
    match verb {
        DecisionVerb::List
        | DecisionVerb::Show
        | DecisionVerb::Raise
        | DecisionVerb::Submit
        | DecisionVerb::Digest
        | DecisionVerb::PluginRaise => ToolInvocationPort::run_tool_with_context(
            runtime,
            verb.spec().tool_name,
            fields,
            ToolContext::default(),
        )
        .map(Outcome::Answer),
        DecisionVerb::Answer => {
            let id = required("id")?;
            let answer = required("answer")?;
            let note = fields
                .get("note")
                .and_then(Value::as_str)
                .map(str::to_string);
            let actor = operator();
            as_command_line_write(|| {
                runtime.answer_decision_request_as_human(
                    &id,
                    &answer,
                    note,
                    actor,
                    Some("cli".to_string()),
                )
            })
            .map(|request| Outcome::DecisionRequest(Box::new(request)))
        }
        DecisionVerb::Reverse => {
            let id = required("id")?;
            let actor = operator();
            as_command_line_write(|| runtime.reverse_decision_answer(&id, &actor))
                .map(|request| Outcome::DecisionRequest(Box::new(request)))
        }
        DecisionVerb::RuleAuto => {
            let rule = required("rule")?;
            let auto = match required("auto")?.as_str() {
                "on" => true,
                "off" => false,
                other => {
                    return Err(OrbitError::InvalidInput(format!(
                        "`auto` must be on or off, not `{other}`"
                    )));
                }
            };
            runtime.set_decision_rule_auto(&rule, auto, &operator())?;
            Ok(Outcome::Answer(json!({ "rule": rule, "auto": auto })))
        }
        DecisionVerb::Consolidate => runtime
            .consolidate_decision_precedent(fields.get("since").and_then(Value::as_str))
            .map(Outcome::Answer),
        DecisionVerb::ChannelList => runtime.list_decision_channels().map(Outcome::Channels),
        DecisionVerb::ChannelRegister
        | DecisionVerb::ChannelRevoke
        | DecisionVerb::ChannelRotateKey => {
            if agent_context_declared(runtime.env()) {
                return Err(OrbitError::PolicyDenied(
                    "registering, revoking or rotating a decision channel is the operator's: it is refused from an agent envelope, so no running code can mint itself a human".into(),
                ));
            }
            let channel = required("channel")?;
            match verb {
                DecisionVerb::ChannelRevoke => runtime.revoke_decision_channel(&channel),
                DecisionVerb::ChannelRotateKey => runtime.rotate_decision_channel_key(&channel),
                _ => {
                    let human = required("human")?;
                    let plugin = required("plugin")?;
                    let allowed_senders = fields
                        .get("allowed_senders")
                        .and_then(Value::as_array)
                        .map(|senders| {
                            senders
                                .iter()
                                .filter_map(|sender| sender.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let require_dkim_verification = fields
                        .get("require_dkim_verification")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    runtime.register_decision_channel_with_dkim_requirement(
                        &channel,
                        &human,
                        &plugin,
                        allowed_senders,
                        require_dkim_verification,
                    )
                }
            }
            .map(Outcome::Channel)
        }
    }
}
