//! Operator consent and credential lifecycle for scoped applications.
use crate::command::{CommandOut, Execute, Payload};
use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;
use std::path::PathBuf;

#[derive(Args)]
#[command(
    about = "Consent to exact application calls in this workspace",
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
    /// JSON application id, expiry and exact scopes for operator review
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
        Ok(Payload::detail(
            serde_json::json!(consent),
            format!("Application consent issued: {}\n", consent.consent_id),
        )
        .into())
    }
}
