use std::path::{Component, Path};

use serde_json::Value;

use crate::error::FrictionNotLocal;
use crate::{
    ArtifactOrigin, DependencyNotDelivered, OrbitError, RecoverableVcsConflict, WorkspaceClaimHeld,
};

use super::pattern::redact_all;

/// Scrub sensitive environment values and known secret patterns from JSON strings.
///
/// Object keys and non-string values retain their types, so provider results
/// remain usable as structured step output after redaction.
pub fn redact_all_json(value: Value) -> Value {
    redact_json_with(value, redact_all)
}

/// Replace `$HOME` / `$USERPROFILE` with `~` in the given string. Prevents
/// user-identifiable paths from leaking into logs. Addresses CodeQL
/// `rust/cleartext-logging`.
pub fn redact_home_dir(text: &str) -> String {
    if let Some(home) = home_dir_string() {
        redact_path_prefix(text, &home)
    } else {
        text.to_string()
    }
}

fn redact_path_prefix(text: &str, prefix: &str) -> String {
    let mut redacted = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(index) = remaining.find(prefix) {
        let (before_match, match_and_after) = remaining.split_at(index);
        let after_match = &match_and_after[prefix.len()..];

        redacted.push_str(before_match);
        if after_match.is_empty() || after_match.starts_with('/') {
            redacted.push('~');
        } else {
            redacted.push_str(prefix);
        }
        remaining = after_match;
    }

    redacted.push_str(remaining);
    redacted
}

/// Scrub an [`OrbitError`]'s string payloads with the full [`redact_all`]
/// pipeline (live env values **plus** the HTTP header / bearer / provider-key
/// patterns), not just live env values.
/// [ORB-00417] Apply at the error persistence/log boundary so an error message
/// embedding a `Bearer <token>` or `sk-*` key in a URL is never written out
/// un-redacted. Idempotent: `redact_all` placeholders never re-match the secret
/// patterns.
pub fn redact_all_error(error: OrbitError) -> OrbitError {
    redact_error_with(error, redact_all)
}

/// [`redact_all`] then [`redact_home_dir`]: the display form for text an
/// operator-facing surface may echo, free of secrets and the private `$HOME`.
pub fn redact_all_and_home(text: &str) -> String {
    redact_home_dir(&redact_all(text))
}

/// [`redact_all_error`] that also replaces the private `$HOME` with `~`,
/// keeping the error's variant.
pub fn redact_all_and_home_error(error: OrbitError) -> OrbitError {
    redact_error_with(error, redact_all_and_home)
}

/// Apply `redact` to every string payload of `error`, keeping its variant.
fn redact_error_with(error: OrbitError, redact: fn(&str) -> String) -> OrbitError {
    match error {
        OrbitError::PolicyDenied(m) => OrbitError::PolicyDenied(redact(&m)),
        OrbitError::NotFound { kind, id } => OrbitError::NotFound {
            kind,
            id: redact(&id),
        },
        OrbitError::CapabilityDenied(m) => OrbitError::CapabilityDenied(redact(&m)),
        OrbitError::UnknownSelector(m) => OrbitError::UnknownSelector(redact(&m)),
        OrbitError::AmbiguousCaller(m) => OrbitError::AmbiguousCaller(redact(&m)),
        OrbitError::UnauthorizedCaller(m) => OrbitError::UnauthorizedCaller(redact(&m)),
        OrbitError::AmbiguousDestination(m) => OrbitError::AmbiguousDestination(redact(&m)),
        OrbitError::UnreachableDestination(m) => OrbitError::UnreachableDestination(redact(&m)),
        OrbitError::StaleRoute(m) => OrbitError::StaleRoute(redact(&m)),
        OrbitError::UnhealthyCheckout(m) => OrbitError::UnhealthyCheckout(redact(&m)),
        OrbitError::ToolNotOnThisHost(m) => OrbitError::ToolNotOnThisHost(redact(&m)),
        OrbitError::PluginDisabledInWorkspace { plugin, workspace } => {
            OrbitError::PluginDisabledInWorkspace {
                plugin: redact(&plugin),
                workspace: redact(&workspace),
            }
        }
        OrbitError::PluginDisabledOnHost { plugin } => OrbitError::PluginDisabledOnHost {
            plugin: redact(&plugin),
        },
        OrbitError::CapabilityRefused(m) => OrbitError::CapabilityRefused(redact(&m)),
        OrbitError::ProtocolSkew(m) => OrbitError::ProtocolSkew(redact(&m)),
        OrbitError::HostRegistry { code, message } => OrbitError::HostRegistry {
            code,
            message: redact(&message),
        },
        OrbitError::PluginBuildConsentRequired(m) => {
            OrbitError::PluginBuildConsentRequired(redact(&m))
        }
        OrbitError::PluginBuildConsentUnavailable(m) => {
            OrbitError::PluginBuildConsentUnavailable(redact(&m))
        }
        OrbitError::PluginBuildFetchUnsupported(m) => {
            OrbitError::PluginBuildFetchUnsupported(redact(&m))
        }
        OrbitError::AdrInvalidTransition(m) => OrbitError::AdrInvalidTransition(redact(&m)),
        OrbitError::RemoteArtifactUnavailable {
            kind,
            id,
            artifact_origin,
        } => OrbitError::RemoteArtifactUnavailable {
            kind,
            id: redact(&id),
            artifact_origin: redact_artifact_origin(artifact_origin, redact),
        },
        OrbitError::ArtifactNotLocal {
            kind,
            id,
            artifact_origin,
        } => OrbitError::ArtifactNotLocal {
            kind,
            id: redact(&id),
            artifact_origin: redact_artifact_origin(artifact_origin, redact),
        },
        OrbitError::FrictionNotLocal(details) => {
            OrbitError::FrictionNotLocal(redact_friction_not_local(*details, redact))
        }
        OrbitError::InvalidInput(m) => OrbitError::InvalidInput(redact(&m)),
        OrbitError::ClaimRefused { kind, message } => OrbitError::ClaimRefused {
            kind,
            message: redact(&message),
        },
        OrbitError::SensitiveInput { field, reason } => OrbitError::SensitiveInput {
            field: redact(&field),
            reason: redact(&reason),
        },
        OrbitError::InvalidInputDiagnostic {
            message,
            did_you_mean,
        } => OrbitError::InvalidInputDiagnostic {
            message: redact(&message),
            did_you_mean: did_you_mean
                .into_iter()
                .map(|suggestion| redact(&suggestion))
                .collect(),
        },
        OrbitError::SkillValidation(m) => OrbitError::SkillValidation(redact(&m)),
        OrbitError::JobValidation(m) => OrbitError::JobValidation(redact(&m)),
        OrbitError::AgentProtocolViolation(m) => OrbitError::AgentProtocolViolation(redact(&m)),
        OrbitError::UnsupportedAgentProvider(m) => OrbitError::UnsupportedAgentProvider(redact(&m)),
        OrbitError::OwnerUnavailable(m) => OrbitError::OwnerUnavailable(redact(&m)),
        OrbitError::OwnerNegotiation(m) => OrbitError::OwnerNegotiation(redact(&m)),
        OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } => OrbitError::OutcomeUnknown {
            mcp_call_id: redact(&mcp_call_id),
            message: redact(&message),
        },
        OrbitError::RemoteTool {
            code,
            message,
            payload,
        } => OrbitError::RemoteTool {
            code: redact(&code),
            message: redact(&message),
            payload: redact_json_with(payload, redact),
        },
        OrbitError::Execution(m) => OrbitError::Execution(redact(&m)),
        OrbitError::ExecutionTimeout {
            timeout_ms,
            message,
        } => OrbitError::ExecutionTimeout {
            timeout_ms,
            message: redact(&message),
        },
        OrbitError::ProcessTimeout { timeout_ms, detail } => OrbitError::ProcessTimeout {
            timeout_ms,
            detail: redact(&detail),
        },
        OrbitError::WorkerContainmentUnavailable { reason } => {
            OrbitError::WorkerContainmentUnavailable {
                reason: redact(&reason),
            }
        }
        OrbitError::RecoverableVcsConflict(conflict) => {
            OrbitError::RecoverableVcsConflict(redact_recoverable_vcs_conflict(*conflict, redact))
        }
        OrbitError::RunCancellationIncomplete {
            pid,
            pgid,
            term_sent,
            kill_sent,
            leader_alive,
            group_alive,
        } => OrbitError::RunCancellationIncomplete {
            pid,
            pgid,
            term_sent,
            kill_sent,
            leader_alive,
            group_alive,
        },
        OrbitError::TaskBundleCorrupt {
            task_id,
            path,
            reason,
        } => OrbitError::TaskBundleCorrupt {
            task_id: redact(&task_id),
            path: redact(&path),
            reason: redact(&reason),
        },
        OrbitError::FileLockTimeout(timeout) => {
            OrbitError::FileLockTimeout(redact_file_lock_timeout(*timeout, redact))
        }
        OrbitError::Store(m) => OrbitError::Store(redact(&m)),
        OrbitError::SqliteContention(contention) => {
            OrbitError::SqliteContention(redact_sqlite_contention(*contention, redact))
        }
        OrbitError::TaskStatusTransition(m) => OrbitError::TaskStatusTransition(redact(&m)),
        OrbitError::SystemIdentityTagDropped { tag } => {
            OrbitError::SystemIdentityTagDropped { tag: redact(&tag) }
        }
        OrbitError::DependencyNotDelivered(diagnostic) => {
            OrbitError::DependencyNotDelivered(redact_dependency_not_delivered(*diagnostic, redact))
        }
        OrbitError::ShipRunInFlight { task_id, run_id } => OrbitError::ShipRunInFlight {
            task_id: redact(&task_id),
            run_id: redact(&run_id),
        },
        OrbitError::PrForgeRemoteMissing { remotes } => OrbitError::PrForgeRemoteMissing {
            remotes: redact(&remotes),
        },
        OrbitError::TaskCompletionLiveRun { task_id, run_id } => {
            OrbitError::TaskCompletionLiveRun {
                task_id: redact(&task_id),
                run_id: redact(&run_id),
            }
        }
        OrbitError::TaskRevisionConflict { task_id } => OrbitError::TaskRevisionConflict {
            task_id: redact(&task_id),
        },
        OrbitError::DesktopWriteAccepted { task_id, reason } => OrbitError::DesktopWriteAccepted {
            task_id: redact(&task_id),
            reason: redact(&reason),
        },
        OrbitError::ResumeRunInFlight {
            source_run_id,
            run_id,
        } => OrbitError::ResumeRunInFlight {
            source_run_id: redact(&source_run_id),
            run_id: redact(&run_id),
        },
        OrbitError::WorkspaceClaimHeld(claim) => {
            OrbitError::WorkspaceClaimHeld(redact_workspace_claim_held(*claim, redact))
        }
        OrbitError::TmpGcActiveRuns { run_ids } => OrbitError::TmpGcActiveRuns {
            run_ids: run_ids.iter().map(|id| redact(id)).collect(),
        },
        OrbitError::JobRunStateTransition(m) => OrbitError::JobRunStateTransition(redact(&m)),
        OrbitError::JobRunStartConflict(m) => OrbitError::JobRunStartConflict(redact(&m)),
        OrbitError::JobRunControlConflict(m) => OrbitError::JobRunControlConflict(redact(&m)),
        OrbitError::Io(m) => OrbitError::Io(redact(&m)),
        OrbitError::WorkspaceError(m) => OrbitError::WorkspaceError(redact(&m)),
        OrbitError::Migration(m) => OrbitError::Migration(redact(&m)),
        OrbitError::StorageAccessDenied { layer, message } => OrbitError::StorageAccessDenied {
            layer,
            message: redact(&message),
        },
    }
}

fn redact_file_lock_timeout(
    mut timeout: crate::fs::io::FileLockTimeout,
    redact: fn(&str) -> String,
) -> Box<crate::fs::io::FileLockTimeout> {
    timeout.lock_path = redact(&timeout.lock_path.to_string_lossy()).into();
    timeout.label = redact(&timeout.label);
    if let Some(holder) = &mut timeout.holder {
        holder.acquired_at = redact(&holder.acquired_at);
        holder.label = redact(&holder.label);
    }
    Box::new(timeout)
}

fn redact_sqlite_contention(
    mut contention: crate::SqliteContention,
    redact: fn(&str) -> String,
) -> Box<crate::SqliteContention> {
    contention.path = redact(&contention.path);
    contention.phase = redact(&contention.phase);
    contention.detail = redact(&contention.detail);
    Box::new(contention)
}

fn redact_friction_not_local(
    details: FrictionNotLocal,
    redact: fn(&str) -> String,
) -> Box<FrictionNotLocal> {
    Box::new(FrictionNotLocal {
        friction_id: redact(&details.friction_id),
        task_id: redact(&details.task_id),
        workspace_id: redact(&details.workspace_id),
        found_in: details
            .found_in
            .into_iter()
            .map(|workspace_id| redact(&workspace_id))
            .collect(),
    })
}

fn redact_workspace_claim_held(
    claim: WorkspaceClaimHeld,
    redact: fn(&str) -> String,
) -> Box<WorkspaceClaimHeld> {
    Box::new(WorkspaceClaimHeld {
        operation: redact(&claim.operation),
        holder: redact(&claim.holder),
        claim_id: redact(&claim.claim_id),
        expires_at: redact(&claim.expires_at),
    })
}

fn redact_dependency_not_delivered(
    diagnostic: DependencyNotDelivered,
    redact: fn(&str) -> String,
) -> Box<DependencyNotDelivered> {
    Box::new(DependencyNotDelivered {
        task_id: redact(&diagnostic.task_id),
        dependency_id: redact(&diagnostic.dependency_id),
        base_ref: redact(&diagnostic.base_ref),
        base_sha: redact(&diagnostic.base_sha),
        detail: redact(&diagnostic.detail),
    })
}

fn redact_recoverable_vcs_conflict(
    conflict: RecoverableVcsConflict,
    redact: fn(&str) -> String,
) -> Box<RecoverableVcsConflict> {
    Box::new(RecoverableVcsConflict {
        operation: redact(&conflict.operation),
        original_base_sha: redact(&conflict.original_base_sha),
        target_base_sha: redact(&conflict.target_base_sha),
        conflicting_paths: conflict
            .conflicting_paths
            .into_iter()
            .map(|path| redact(&path))
            .collect(),
        diagnostic: redact(&conflict.diagnostic),
    })
}

pub(super) fn redact_json_with(value: Value, redact: fn(&str) -> String) -> Value {
    match value {
        Value::String(raw) => Value::String(redact(&raw)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_json_with(item, redact))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, redact_json_with(value, redact)))
                .collect(),
        ),
        other => other,
    }
}

fn redact_artifact_origin(
    artifact_origin: ArtifactOrigin,
    redact: fn(&str) -> String,
) -> ArtifactOrigin {
    ArtifactOrigin {
        mode: artifact_origin.mode,
        worktree_root: credential_safe_location_with(&artifact_origin.worktree_root, redact),
        branch: artifact_origin
            .branch
            .map(|branch| credential_safe_location_with(&branch, redact)),
    }
}

/// Return a public-safe worktree or branch location. URI-shaped values are
/// rejected wholesale so allocation metadata can never expose a credentialed
/// transport target; ordinary filesystem paths retain their useful location
/// while the standard credential patterns are scrubbed.
pub fn credential_safe_location(raw: &str) -> String {
    credential_safe_location_with(raw, redact_all)
}

fn credential_safe_location_with(raw: &str, redact: fn(&str) -> String) -> String {
    let lower = raw.trim().to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("bearer ") || lower.contains("authorization:") {
        "[REDACTED_LOCATION]".to_string()
    } else {
        redact(raw)
    }
}

fn home_dir_string() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .filter(|home| {
            let path = Path::new(home);
            path.is_absolute()
                && path
                    .components()
                    .any(|component| matches!(component, Component::Normal(_)))
        })
}
