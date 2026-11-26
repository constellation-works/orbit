use super::{Command, Operation};
use crate::OrbitError;
use crate::runtime::audit::run::RunScriptExecution;
use crate::runtime::scripts::{ScriptGrantOptions, ScriptStatus};
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
