use super::{Command, Operation, Outcome, RowDue};
use crate::OrbitError;
use crate::app::DashboardOrigin;
use crate::runtime::audit::run::RunScriptExecution;
use crate::runtime::scripts::{ScriptGrantOptions, ScriptStatus};
use orbit_common::governance::authorization::DASHBOARD_SCRIPT_GRANT;
use orbit_types::workflow::activity_job::ScriptGrant;

/// A new grant with the previous digest and current asset for the consent receipt.
pub struct GrantedScript {
    pub previous_digest: Option<String>,
    pub grant: ScriptGrant,
    pub status: Option<ScriptStatus>,
}

impl<'a> Command<'a, Vec<ScriptStatus>> {
    /// List workspace scripts.
    pub fn scripts() -> Self {
        Self::new(
            Operation::command("script", "list").target(Some("script"), None),
            move |core| core.runtime().script_statuses(),
        )
    }
}

impl<'a> Command<'a, ScriptStatus> {
    /// Inspect a script and its grant.
    pub fn script(name: &'a str) -> Self {
        Self::new(
            Operation::command("script", "show").target(Some("script"), Some(name.into())),
            move |core| {
                core.runtime().script_status(name)?.ok_or_else(|| OrbitError::InvalidInput(format!("no script named '{name}' and no grant under that name (see `orbit script list`)")))
            },
        )
    }
}

impl<'a> Command<'a, GrantedScript> {
    /// Grant a script and read its receipt.
    pub fn script_grant(name: &'a str, options: ScriptGrantOptions) -> Self {
        Self::new(
            Operation::command("script", "grant").target(Some("script"), Some(name.into())),
            move |core| {
                let rt = core.runtime();
                let previous_digest = rt
                    .script_status(name)
                    .ok()
                    .flatten()
                    .and_then(|status| status.grant.map(|grant| grant.digest));
                let grant = rt.grant_script(name, &options)?;
                let status = rt.script_status(name)?;
                Ok(GrantedScript {
                    previous_digest,
                    grant,
                    status,
                })
            },
        )
    }
}

impl<'a> Command<'a, ()> {
    /// Revoke a script grant.
    pub fn script_revoke(name: &'a str) -> Self {
        Self::new(
            Operation::command("script", "revoke").target(Some("script"), Some(name.into())),
            move |core| {
                if core.runtime().revoke_script(name)? {
                    Ok(())
                } else {
                    Err(OrbitError::InvalidInput(format!(
                        "no grant recorded for script '{name}' on this host"
                    )))
                }
            },
        )
    }
}

impl<'a> Command<'a, Vec<RunScriptExecution>> {
    /// Read recent script executions.
    pub fn script_history(name: &'a str, limit: usize) -> Self {
        Self::new(
            Operation::command("script", "history").target(Some("script"), Some(name.into())),
            move |core| core.runtime().script_execution_history(name, limit),
        )
    }
}

/// What the dashboard's script grant did for an admitted operator.
pub enum DashboardScriptGrant {
    /// The request named no reviewed digest; nothing was granted.
    DigestMissing,
    /// The grant, bound to the reviewed digest.
    Granted(Box<ScriptGrant>),
    /// The grant was refused.
    Refused(OrbitError),
}

impl<'a> Command<'a, DashboardScriptGrant> {
    /// Grant `name` at the digest the operator reviewed, answering its open
    /// request when one exists. `digest` is the digest as the request sent
    /// it, which the audit row records; `reviewed` is the digest to grant.
    pub fn dashboard_script_grant(
        origin: DashboardOrigin,
        name: &'a str,
        digest: Option<String>,
        reviewed: Option<String>,
        grantor: String,
    ) -> Self {
        let operation = Operation::dashboard_read("script", "dashboard-grant")
            .requires(&DASHBOARD_SCRIPT_GRANT)
            .dashboard_row(
                &DASHBOARD_SCRIPT_GRANT,
                name,
                serde_json::json!({ "digest": digest }),
                origin,
            );
        Self {
            operation,
            flow: Box::new(move |core| {
                let (value, row) = match reviewed {
                    None => (DashboardScriptGrant::DigestMissing, RowDue::Nothing),
                    Some(reviewed) => {
                        match core
                            .runtime()
                            .grant_script_from_dashboard(name, &reviewed, grantor)
                        {
                            Ok(grant) => (
                                DashboardScriptGrant::Granted(Box::new(grant)),
                                RowDue::Declared,
                            ),
                            Err(error) => (
                                DashboardScriptGrant::Refused(error),
                                RowDue::Failure("script grant refused"),
                            ),
                        }
                    }
                };
                Ok(Outcome {
                    value,
                    exit_code: 0,
                    row,
                })
            }),
        }
    }
}
