//! Operator consent and credential lifecycle for scoped applications.
use crate::command::{CommandOut, Execute, Payload};
use chrono::SecondsFormat;
use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;
use orbit_types::tool::{
    ApplicationConsent, ApplicationConstraint, ApplicationOperation, ApplicationScope,
    ApplicationTaskPredicate,
};
use std::path::PathBuf;

#[derive(Args)]
#[command(
    about = "Consent to exact and constrained application calls in this workspace",
    subcommand_required = true
)]
pub struct ApplicationCommand {
    #[command(subcommand)]
    pub command: ApplicationSubcommand,
}

#[derive(Subcommand)]
pub enum ApplicationSubcommand {
    /// Approve a consent request and provision a new private credential file
    Consent(ApplicationConsentArgs),
    /// Revoke the old consent and issue a newly approved credential
    Rotate(ApplicationRotateArgs),
    /// Revoke a consent and all of its completion callbacks
    Revoke(ApplicationRevokeArgs),
}

#[derive(Args)]
pub struct ApplicationConsentArgs {
    /// JSON application id, expiry and scopes for operator review
    #[arg(long)]
    pub input_file: PathBuf,
    /// New owner-only file receiving the credential once; must not exist
    #[arg(long)]
    pub credential_file: PathBuf,
}

#[derive(Args)]
pub struct ApplicationRotateArgs {
    pub consent_id: String,
    #[command(flatten)]
    pub consent: ApplicationConsentArgs,
}

#[derive(Args)]
pub struct ApplicationRevokeArgs {
    pub consent_id: String,
}

impl Execute for ApplicationCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let consent = match self.command {
            ApplicationSubcommand::Consent(args) => runtime.provision_application_from_file(
                &args.input_file,
                &args.credential_file,
                None,
            )?,
            ApplicationSubcommand::Rotate(args) => runtime.provision_application_from_file(
                &args.consent.input_file,
                &args.consent.credential_file,
                Some(&args.consent_id),
            )?,
            ApplicationSubcommand::Revoke(args) => {
                runtime.revoke_application(&args.consent_id, &Default::default())?;
                return Ok(Payload::detail(
                    serde_json::json!({"revoked": args.consent_id}),
                    "Application consent revoked.\n",
                )
                .into());
            }
        };
        Ok(Payload::detail(serde_json::json!(consent), consent_issued_text(&consent)).into())
    }
}

/// One plain line per constrained scope, after the issued sentence.
fn consent_issued_text(consent: &ApplicationConsent) -> String {
    let mut text = format!("Application consent issued: {}\n", consent.consent_id);
    for scope in &consent.scopes {
        if let Some(line) = constrained_scope_summary(consent, scope) {
            text.push_str(&line);
            text.push('\n');
        }
    }
    text
}

/// `None` for an exact scope. A scope is constrained when its entry or any
/// callback uses `fields`.
fn constrained_scope_summary(
    consent: &ApplicationConsent,
    scope: &ApplicationScope,
) -> Option<String> {
    let constrained = scope.operation.is_constrained()
        || scope
            .callbacks
            .iter()
            .any(ApplicationOperation::is_constrained);
    if !constrained {
        return None;
    }
    let name = scope.name.as_deref().unwrap_or("unnamed");
    let mut line = format!("{name}: {}", admit_phrase(&scope.operation));
    let callbacks: Vec<_> = scope
        .callbacks
        .iter()
        .filter(|operation| operation.is_constrained())
        .map(admit_phrase)
        .collect();
    if !callbacks.is_empty() {
        line.push_str("; callbacks ");
        line.push_str(&callbacks.join(" | "));
    }
    let expires = consent
        .expires_at
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    line.push_str(&format!("; expires {expires}"));
    if let Some(limit) = &scope.rate_limit {
        line.push_str(&format!(
            "; rate limit {} admissions per {} seconds",
            limit.admissions, limit.window_seconds
        ));
    }
    Some(line)
}

fn admit_phrase(operation: &ApplicationOperation) -> String {
    match &operation.fields {
        Some(fields) => {
            let parts: Vec<_> = fields
                .iter()
                .map(|(field, constraint)| constraint_phrase(field, constraint))
                .collect();
            format!("admit {} when {}", operation.tool, parts.join(", "))
        }
        None => format!("admit {} exactly", operation.tool),
    }
}

fn constraint_phrase(field: &str, constraint: &ApplicationConstraint) -> String {
    match constraint {
        ApplicationConstraint::Const(value) => format!("{field} is {value}"),
        ApplicationConstraint::OneOf(values) => {
            let listed = values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" or ");
            format!("{field} is one of {listed}")
        }
        ApplicationConstraint::Text(text) => {
            format!("{field} is text up to {} characters", text.max_chars)
        }
        ApplicationConstraint::Token(token) => {
            format!("{field} is a token up to {} characters", token.max_len)
        }
        ApplicationConstraint::Sha256(_) => format!("{field} is a sha256 digest"),
        ApplicationConstraint::Workspace(_) => format!("{field} is this workspace"),
        ApplicationConstraint::Task(predicate) => task_phrase(field, predicate),
        ApplicationConstraint::Transition(transition) => format!(
            "{field} moves the task from {} to {}",
            transition.from.join(" or "),
            transition.to
        ),
        ApplicationConstraint::Bound(name) => format!("{field} equals the entry's {name}"),
        ApplicationConstraint::ArtifactJson(artifact) => format!(
            "{field} is JSON at {} in artifact {}",
            artifact.pointer, artifact.path
        ),
        ApplicationConstraint::SubsetOf(values) => {
            format!("{field} is a subset of {}", values.join(", "))
        }
        ApplicationConstraint::Optional(inner) => {
            format!("optional {}", constraint_phrase(field, inner))
        }
    }
}

fn task_phrase(field: &str, predicate: &ApplicationTaskPredicate) -> String {
    let mut phrase = format!("{field} is a task");
    if !predicate.tags_all.is_empty() {
        phrase.push_str(&format!(" tagged {}", predicate.tags_all.join(", ")));
    }
    if !predicate.tags_any.is_empty() {
        let any = predicate.tags_any.join(", ");
        if predicate.tags_all.is_empty() {
            phrase.push_str(&format!(" tagged any of {any}"));
        } else {
            phrase.push_str(&format!(" and any of {any}"));
        }
    }
    if !predicate.status.is_empty() {
        phrase.push_str(&format!(" in status {}", predicate.status.join(" or ")));
    }
    if let Some(digest) = &predicate.artifact_sha256 {
        phrase.push_str(&format!(
            " whose {} digest equals {}",
            digest.path, digest.field
        ));
    }
    if predicate.pin {
        phrase.push_str(" and pins that task");
    }
    phrase
}
