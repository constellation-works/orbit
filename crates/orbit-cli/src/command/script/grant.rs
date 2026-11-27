use std::path::PathBuf;

use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};
use orbit_core::runtime::scripts::{ScriptGrantOptions, ScriptGrantState};
use orbit_types::workflow::activity_job::ScriptNetwork;
use serde_json::json;

use super::support::script_grant_json;
use crate::command::{CommandOut, Payload};

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
    /// Memory ceiling of the script's own systemd scope: bytes with an
    /// optional K, M, G or T suffix (default: this host's
    /// `machine.worker_memory_max`). Never above the host's value; a
    /// re-grant without it returns to the default.
    #[arg(long, value_name = "SIZE", value_parser = parse_script_memory_max)]
    pub memory_max: Option<u64>,
    /// Process and thread ceiling of the script's own scope (default:
    /// `machine.worker_tasks_max`). Never above the host's value.
    #[arg(long, value_name = "COUNT")]
    pub tasks_max: Option<u32>,
    /// CPU ceiling of the script's own scope, as a percentage of one core
    /// (`200` = two cores; default: `machine.worker_cpu_quota`). Never above
    /// the host's value when that is set. Enforced only where systemd
    /// delegates the cpu controller.
    #[arg(long, value_name = "PERCENT")]
    pub cpu_quota: Option<u32>,
}

impl ScriptGrantArgs {
    pub(super) fn execute(self, core: &Core) -> CommandOut {
        let result = exec(
            core,
            Command::script_grant(
                &self.name,
                ScriptGrantOptions {
                    network: self.network,
                    read_roots: self.read_roots,
                    write_roots: self.write_roots,
                    env_pass: self.env_pass,
                    follower: self.follower,
                    output_max_files: self.output_max_files,
                    output_max_file_bytes: self.output_max_file_bytes,
                    memory_max_bytes: self.memory_max,
                    tasks_max: self.tasks_max,
                    cpu_quota_percent: self.cpu_quota,
                    ..Default::default()
                },
            ),
        )?
        .value;
        let grant = result.grant;
        let status = result.status;
        let doc = json!({
            "name": grant.name,
            "state": ScriptGrantState::Granted.as_str(),
            "grant": script_grant_json(&grant),
            "previous_digest": result.previous_digest,
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
        if grant.network == ScriptNetwork::Any {
            text.push_str(
                "warning: network `any` shares this host's network with the script with no \
                 rule; `https` admits outbound TCP to port 443 only\n",
            );
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
        let machine_default =
            |value: Option<String>| value.unwrap_or_else(|| "host default".to_string());
        text.push_str(&format!(
            "granted scope ceilings (capped at this host's machine.worker_* values): memory {}, \
             tasks {}, cpu {}\n",
            machine_default(
                grant
                    .limits
                    .memory_max_bytes
                    .map(|bytes| format!("{bytes} bytes"))
            ),
            machine_default(grant.limits.tasks_max.map(|tasks| tasks.to_string())),
            machine_default(
                grant
                    .limits
                    .cpu_quota_percent
                    .map(|percent| format!("{percent}%"))
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
        if let Some(previous_digest) = result
            .previous_digest
            .as_deref()
            .filter(|previous| *previous != grant.digest)
        {
            text.push_str(&format!(
                "supersedes the earlier grant at digest {previous_digest}\n"
            ));
        }
        Ok(Payload::detail(doc, text).into())
    }
}

fn parse_script_memory_max(value: &str) -> Result<u64, String> {
    orbit_config::MemoryLimit::parse(value)
        .and_then(orbit_config::MemoryLimit::byte_count)
        .ok_or_else(|| {
            format!(
                "invalid memory size `{value}` (want a positive byte count with an optional \
                 K, M, G or T suffix)"
            )
        })
}

fn parse_script_network(value: &str) -> Result<ScriptNetwork, String> {
    ScriptNetwork::parse(value)
        .ok_or_else(|| format!("unknown network `{value}` (want `none`, `https` or `any`)"))
}
