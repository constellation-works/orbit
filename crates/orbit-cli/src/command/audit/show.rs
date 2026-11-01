use clap::Args;
use orbit_core::{AuditEvent, AuditEventFilter, OrbitRuntime};
use orbit_types::tool::{
    ApplicationAdmissionMatch, ApplicationArtifactEvidence, ApplicationAuditRefusal,
    ApplicationInvocation, ApplicationPredicateEvidence, ApplicationTextDigest,
};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::command::{CommandOut, Execute, Payload};

use super::support::audit_event_to_json;

#[derive(Args)]
pub struct AuditShowArgs {
    /// Audit event ID
    pub id: i64,
}

impl Execute for AuditShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let event = runtime.show_audit_event(self.id)?;
        let doc = audit_event_to_json(&event);

        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "ID:                {}", event.id);
        let _ = writeln!(out, "Execution ID:      {}", event.execution_id);
        let _ = writeln!(out, "Timestamp:         {}", event.timestamp.to_rfc3339());
        let _ = writeln!(out, "Command:           {}", event.command);
        let _ = writeln!(
            out,
            "Subcommand:        {}",
            event.subcommand.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Tool:              {}",
            event.tool_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Target type:       {}",
            event.target_type.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Target ID:         {}",
            event.target_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(out, "Role:              {}", event.role);
        // ORB-10890: the trailing marker is not decoration. `Role` is what
        // Orbit authenticated; this line is whatever the caller typed about
        // itself, and reading the two without the distinction is how a
        // forgeable number gets published as a measured one.
        let _ = writeln!(
            out,
            "Self-reported:     {}",
            event
                .self_reported_actor
                .as_deref()
                .map(|actor| format!("{actor} (unverified)"))
                .unwrap_or_else(|| "-".to_string())
        );
        let _ = writeln!(out, "Status:            {}", event.status);
        let _ = writeln!(out, "Exit code:         {}", event.exit_code);
        let _ = writeln!(out, "Duration (ms):     {}", event.duration_ms);
        let _ = writeln!(out, "Working dir:       {}", event.working_directory);
        let _ = writeln!(out, "PID:               {}", event.pid);
        let _ = writeln!(
            out,
            "Host:              {}",
            event.host.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Workspace ID:      {}",
            event.workspace_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Caller machine:    {}",
            event.caller_machine_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Caller host:       {}",
            event.caller_machine_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Process machine:   {}",
            event.process_machine_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Process host ID:   {}",
            event.process_machine_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Transport:         {}",
            event
                .transport
                .map(|value| value.to_string())
                .as_deref()
                .unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Capabilities:      {}",
            event
                .effective_capabilities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let _ = writeln!(
            out,
            "Origin session:    {}",
            event.origin_session_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "MCP call:          {}",
            event.mcp_call_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Lease:             {}",
            event.lease_id.as_deref().unwrap_or("-")
        );
        if let Some(ref err) = event.error_message {
            let _ = writeln!(out, "Error:             {err}");
        }
        if let Some(application) = &event.application {
            write_application_audit(&mut out, application);
            if !application.callback {
                write_application_callbacks(&mut out, runtime, &event)?;
            }
        }
        Ok(Payload::detail(doc, out).into())
    }
}

fn write_field(out: &mut String, label: &str, value: impl std::fmt::Display) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "{label:<19}{value}");
}

fn write_application_audit(out: &mut String, application: &ApplicationInvocation) {
    write_field(out, "Application:", &application.consent.application_id);
    write_field(out, "Consent:", &application.consent.consent_id);
    write_field(out, "Scope:", scope_label(application));
    write_field(out, "Match:", match_label(application.admission_match));
    write_field(
        out,
        "Callback:",
        if application.callback { "yes" } else { "no" },
    );
    if let Some(entry) = &application.entry_execution_id {
        write_field(out, "Entry execution:", entry);
    }
    if let Some(predicate) = &application.predicate {
        write_field(out, "Predicate:", predicate_text(predicate));
    }
    if !application.bindings.is_empty() {
        write_field(out, "Bindings:", bindings_text(&application.bindings));
    }
    if !application.text_digests.is_empty() {
        write_field(
            out,
            "Text digests:",
            text_digests_text(&application.text_digests),
        );
    }
    if let Some(refused) = &application.refused {
        write_field(out, "Refused:", refused_text(refused));
    }
}

/// Callback rows of this entry, oldest first. An empty page is omitted.
fn write_application_callbacks(
    out: &mut String,
    runtime: &OrbitRuntime,
    event: &AuditEvent,
) -> Result<(), orbit_common::OrbitError> {
    use std::fmt::Write as _;
    let mut callbacks = runtime.list_audit_events_filtered(&AuditEventFilter {
        application_entry_execution_id: Some(event.execution_id.clone()),
        limit: 100,
        ..AuditEventFilter::default()
    })?;
    if callbacks.is_empty() {
        return Ok(());
    }
    callbacks.reverse();
    let _ = writeln!(out, "Callbacks:");
    for callback in callbacks {
        let tool = callback.tool_name.as_deref().unwrap_or("-");
        let _ = writeln!(out, "  {} {tool} {}", callback.id, callback.status);
    }
    Ok(())
}

fn scope_label(application: &ApplicationInvocation) -> String {
    match &application.scope_name {
        Some(name) => format!("{name} ({})", application.scope_index),
        None => application.scope_index.to_string(),
    }
}

fn match_label(admission: ApplicationAdmissionMatch) -> &'static str {
    match admission {
        ApplicationAdmissionMatch::Exact => "exact",
        ApplicationAdmissionMatch::Constrained => "constrained",
    }
}

fn predicate_text(evidence: &ApplicationPredicateEvidence) -> String {
    let mut text = format!("task {} status {}", evidence.task, evidence.status_seen);
    if !evidence.tags_matched.is_empty() {
        text.push_str(" tags ");
        text.push_str(&evidence.tags_matched.join(", "));
    }
    if let Some(artifact) = &evidence.artifact {
        text.push_str(&artifact_text(artifact));
    }
    text
}

fn artifact_text(artifact: &ApplicationArtifactEvidence) -> String {
    format!(" artifact {} sha256 {}", artifact.path, artifact.sha256)
}

fn bindings_text(bindings: &BTreeMap<String, Value>) -> String {
    bindings
        .iter()
        .map(|(key, value)| format!("{key}={}", binding_value(value)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn binding_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn text_digests_text(digests: &BTreeMap<String, ApplicationTextDigest>) -> String {
    digests
        .iter()
        .map(|(field, digest)| format!("{field} sha256 {} chars {}", digest.sha256, digest.chars))
        .collect::<Vec<_>>()
        .join(", ")
}

fn refused_text(refusal: &ApplicationAuditRefusal) -> String {
    let scope = refusal.scope_name.as_deref().unwrap_or("-");
    match &refusal.field {
        Some(field) => format!(
            "scope {scope} field {field} constraint {}",
            refusal.constraint
        ),
        None => format!("scope {scope} constraint {}", refusal.constraint),
    }
}
