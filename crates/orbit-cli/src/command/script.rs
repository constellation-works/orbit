//! `orbit script` — grant and inspect the workspace scripts a job step
//! may name with `target: script:<name>`.
//!
//! The consent surface: manifests under `.orbit/resources/scripts/`, an
//! operator grant bound to the manifest and entry digests, the staleness
//! reporting that shows when either file changed after granting, and the
//! history of what ran. Grant and revoke are governed operator-only
//! operations; an agent session can list, show and read history, never
//! grant the code it can edit.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use orbit_core::runtime::scripts::{ScriptGrantOptions, ScriptGrantState, ScriptStatus};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::workflow::activity_job::{ScriptGrant, ScriptNetwork};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(
    about = "Grant and inspect workspace scripts a job step may run",
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "A script is a manifest at `.orbit/resources/scripts/<name>.yaml` whose\n\
                  `entry` names a program file in the workspace. A job step names it\n\
                  as `target: script:<name>`. Grants are host-local consent bound to\n\
                  the manifest and entry digests: editing either file makes the grant\n\
                  stale until an operator grants again. A granted script step runs on\n\
                  Linux under a sandbox with no network; other hosts refuse it."
)]
pub struct ScriptCommand {
    #[command(subcommand)]
    pub command: ScriptSubcommand,
}

#[derive(Subcommand)]
pub enum ScriptSubcommand {
    /// List workspace scripts with their entry, digest and grant state
    List(ScriptListArgs),
    /// Show one script's manifest facts, grant and staleness
    Show(ScriptShowArgs),
    /// Grant one script at its current manifest and entry digests
    Grant(ScriptGrantArgs),
    /// Revoke one script's grant on this host
    Revoke(ScriptRevokeArgs),
    /// List recent attempts of one script, refusals included
    History(ScriptHistoryArgs),
}

impl Execute for ScriptCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            ScriptSubcommand::List(args) => args.execute(runtime),
            ScriptSubcommand::Show(args) => args.execute(runtime),
            ScriptSubcommand::Grant(args) => args.execute(runtime),
            ScriptSubcommand::Revoke(args) => args.execute(runtime),
            ScriptSubcommand::History(args) => args.execute(runtime),
        }
    }
}

#[derive(Args)]
pub struct ScriptListArgs {}

impl ScriptListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let statuses = runtime.script_statuses()?;
        let records: Vec<Value> = statuses.iter().map(script_status_json).collect();
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("ENTRY"),
            Column::new("GRANT").fixed(),
            Column::new("DIGEST"),
        ])
        .empty_message(
            "no scripts in this workspace (manifests live in .orbit/resources/scripts/)",
        );
        for status in &statuses {
            table.add_row(vec![
                status.name.clone(),
                status
                    .asset
                    .as_ref()
                    .map(|asset| asset.spec.entry.clone())
                    .unwrap_or_else(|| "-".to_string()),
                status.state.to_string(),
                status
                    .asset
                    .as_ref()
                    .map(|asset| short_digest(&asset.digest))
                    .or_else(|| {
                        status
                            .grant
                            .as_ref()
                            .map(|grant| short_digest(&grant.digest))
                    })
                    .unwrap_or_else(|| "-".to_string()),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}

#[derive(Args)]
pub struct ScriptShowArgs {
    /// Script name.
    pub name: String,
}

impl ScriptShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Some(status) = runtime.script_status(&self.name)? else {
            return Err(OrbitError::InvalidInput(format!(
                "no script named '{}' and no grant under that name (see `orbit script list`)",
                self.name
            )));
        };
        let doc = script_status_json(&status);
        let mut text = format!("script: {}\n", status.name);
        text.push_str(&format!("grant state: {}\n", status.state));
        if let Some(asset) = &status.asset {
            text.push_str(&format!("manifest: {}\n", asset.manifest_path.display()));
            text.push_str(&format!("entry: {}\n", asset.spec.entry));
            text.push_str(&format!(
                "entry (resolved): {}\n",
                asset.entry_path.display()
            ));
            if !asset.interpreter.is_empty() {
                text.push_str(&format!("interpreter: {}\n", asset.interpreter.join(" ")));
            }
            if !asset.spec.args.is_empty() {
                text.push_str(&format!("args: {}\n", asset.spec.args.join(" ")));
            }
            text.push_str(&format!("current digest: {}\n", asset.digest));
            text.push_str(&format!(
                "current manifest digest: {}\n",
                asset.manifest_digest
            ));
            text.push_str(&format!("current entry digest: {}\n", asset.entry_digest));
        }
        if let Some(grant) = &status.grant {
            text.push_str(&format!("grant: {}\n", grant.grant_id));
            text.push_str(&format!("granted digest: {}\n", grant.digest));
            text.push_str(&format!(
                "granted manifest digest: {}\n",
                grant.manifest_digest
            ));
            text.push_str(&format!("granted entry digest: {}\n", grant.entry_digest));
            text.push_str(&format!("network: {}\n", grant.network));
            text.push_str(&format!(
                "grantor: {} at {} ({})\n",
                grant.grantor, grant.granted_at, grant.provenance
            ));
        }
        text.push_str(&format!("detail: {}\n", status.detail));
        Ok(Payload::detail(doc, text).into())
    }
}

#[derive(Args)]
pub struct ScriptGrantArgs {
    /// Script name.
    pub name: String,
    /// Network the grant allows (default: what the manifest requests).
    #[arg(long, value_parser = parse_script_network)]
    pub network: Option<ScriptNetwork>,
    /// Extra absolute read root to grant (repeatable).
    #[arg(long = "read-root")]
    pub read_roots: Vec<PathBuf>,
    /// Extra absolute write root to grant (repeatable).
    #[arg(long = "write-root")]
    pub write_roots: Vec<PathBuf>,
    /// Extra environment variable name to pass (repeatable).
    #[arg(long = "env-pass")]
    pub env_pass: Vec<String>,
    /// Grant for a pull follower host instead of the workspace owner.
    #[arg(long)]
    pub follower: bool,
    /// Most output files one step may keep, up to 64 (default 8). Each
    /// grant states it again; a re-grant without it returns to the default.
    #[arg(long, value_name = "COUNT")]
    pub output_max_files: Option<u32>,
    /// Largest output file one step may keep, in bytes, up to 67108864
    /// (64 MiB; default 1048576). A step's whole output set is also
    /// bounded by files times bytes and never exceeds 256 MiB.
    #[arg(long, value_name = "BYTES")]
    pub output_max_file_bytes: Option<u64>,
}

fn parse_script_network(value: &str) -> Result<ScriptNetwork, String> {
    ScriptNetwork::parse(value)
        .ok_or_else(|| format!("unknown network `{value}` (want `none`, `https` or `any`)"))
}

impl ScriptGrantArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let previous = runtime.script_status(&self.name).ok().flatten();
        let grant = runtime.grant_script(
            &self.name,
            &ScriptGrantOptions {
                network: self.network,
                read_roots: self.read_roots,
                write_roots: self.write_roots,
                env_pass: self.env_pass,
                follower: self.follower,
                output_max_files: self.output_max_files,
                output_max_file_bytes: self.output_max_file_bytes,
            },
        )?;
        let status = runtime.script_status(&self.name)?;
        let doc = json!({
            "name": grant.name,
            "state": ScriptGrantState::Granted.as_str(),
            "grant": script_grant_json(&grant),
            "previous_digest": previous
                .as_ref()
                .and_then(|status| status.grant.as_ref())
                .map(|grant| grant.digest.clone()),
        });
        let asset = status.as_ref().and_then(|status| status.asset.as_ref());
        let mut text = format!("granted script: {}\n", grant.name);
        text.push_str(&format!("grant: {}\n", grant.grant_id));
        text.push_str(&format!("digest: {}\n", grant.digest));
        text.push_str(&format!("manifest digest: {}\n", grant.manifest_digest));
        text.push_str(&format!("entry digest: {}\n", grant.entry_digest));
        if let Some(asset) = asset {
            text.push_str(&format!("manifest: {}\n", asset.manifest_path.display()));
            text.push_str(&format!("entry: {}\n", asset.spec.entry));
            text.push_str(&format!(
                "requested network: {} (granted {})\n",
                asset.spec.requests.network, grant.network
            ));
        }
        if let Some(asset) = asset
            && asset.spec.files.outputs.declared()
        {
            let declared = &asset.spec.files.outputs;
            let requested = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
            text.push_str(&format!(
                "requested output files: {} of at most {} bytes\n",
                requested(declared.max_files.map(|files| files.to_string())),
                requested(declared.max_file_bytes.map(|bytes| bytes.to_string())),
            ));
        }
        text.push_str(&format!(
            "granted output files: {} of at most {} bytes (set total at most {} bytes)\n",
            grant.limits.output_max_files,
            grant.limits.output_max_file_bytes,
            orbit_types::workflow::activity_job::script_output_total_cap(
                grant.limits.output_max_files,
                grant.limits.output_max_file_bytes,
            ),
        ));
        if !grant.interpreter_paths.is_empty() {
            let rendered: Vec<String> = grant
                .interpreter_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            text.push_str(&format!("interpreter: {}\n", rendered.join(" ")));
        }
        if let Some(previous_digest) = previous
            .as_ref()
            .and_then(|status| status.grant.as_ref())
            .map(|grant| grant.digest.as_str())
            .filter(|previous| *previous != grant.digest)
        {
            text.push_str(&format!(
                "supersedes the earlier grant at digest {previous_digest}\n"
            ));
        }
        Ok(Payload::detail(doc, text).into())
    }
}

#[derive(Args)]
pub struct ScriptRevokeArgs {
    /// Script name.
    pub name: String,
}

impl ScriptRevokeArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if !runtime.revoke_script(&self.name)? {
            return Err(OrbitError::InvalidInput(format!(
                "no grant recorded for script '{}' on this host",
                self.name
            )));
        }
        let doc = json!({"name": self.name, "revoked": true});
        Ok(Payload::detail(doc, format!("revoked script grant: {}\n", self.name)).into())
    }
}

#[derive(Args)]
pub struct ScriptHistoryArgs {
    /// Script name.
    pub name: String,

    /// Most attempts to list, newest first.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

impl ScriptHistoryArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let executions = runtime.script_execution_history(&self.name, self.limit)?;
        let records: Vec<Value> = executions
            .iter()
            .map(|execution| execution.to_json())
            .collect();
        let mut table = Table::new(vec![
            Column::new("WHEN").fixed(),
            Column::new("RUN").fixed(),
            Column::new("ATTEMPT"),
        ])
        .empty_message("no recorded attempts of this script in this workspace");
        for execution in &executions {
            table.add_row(vec![
                execution
                    .timestamp
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
                execution.run_id.clone(),
                execution.describe(),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}

fn short_digest(digest: &str) -> String {
    digest.chars().take(12).collect()
}

fn script_status_json(status: &ScriptStatus) -> Value {
    json!({
        "name": status.name,
        "state": status.state.as_str(),
        "detail": status.detail,
        "manifest": status.asset.as_ref().map(|asset| asset.manifest_path.display().to_string()),
        "entry": status.asset.as_ref().map(|asset| asset.spec.entry.clone()),
        "entry_path": status.asset.as_ref().map(|asset| asset.entry_path.display().to_string()),
        "interpreter": status.asset.as_ref().map(|asset| asset.interpreter.clone()).unwrap_or_default(),
        "args": status.asset.as_ref().map(|asset| asset.spec.args.clone()).unwrap_or_default(),
        "current_digest": status.asset.as_ref().map(|asset| asset.digest.clone()),
        "current_manifest_digest": status.asset.as_ref().map(|asset| asset.manifest_digest.clone()),
        "current_entry_digest": status.asset.as_ref().map(|asset| asset.entry_digest.clone()),
        "grant": status.grant.as_ref().map(script_grant_json),
    })
}

fn script_grant_json(grant: &ScriptGrant) -> Value {
    serde_json::to_value(grant).unwrap_or(Value::Null)
}
