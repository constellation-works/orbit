use orbit_cmd::WorkspaceDoctorStatus;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Payload};

use super::report;
use super::tool_telemetry;

/// `orbit doctor providers`: one row per executor definition, naming the
/// provider CLI it launches, where dispatch would find that CLI, and the
/// sandbox mode the effective definition resolves to.
pub(super) fn provider_diagnostics(runtime: &OrbitRuntime) -> CommandOut {
    use crate::output::table::{Column, Table};

    let defs = runtime.list_executor_defs()?;
    // One fresh user-scoped probe for this diagnostic. The configured executor
    // mode alone does not establish that the host can create the namespace.
    let linux_probe = defs
        .iter()
        .any(|def| {
            def.sandbox
                .is_some_and(|kind| kind.as_str() == "linux-bwrap")
        })
        .then(orbit_core::bootstrap::linux_sandbox_host::probe_bwrap_fresh);
    // [ORB-14698] The same provider-limit rows `orbit doctor` reports, under
    // the executor of the provider each concerns.
    let limit_findings = orbit_cmd::provider_limit_findings(runtime);
    let mut values = Vec::with_capacity(defs.len());
    let mut telemetry_notes: Vec<String> = Vec::new();
    let mut table = Table::new(vec![
        Column::new("EXECUTOR").fixed(),
        Column::new("TYPE").fixed(),
        Column::new("CLI").fixed(),
        Column::new("FOUND").fixed(),
        Column::new("SANDBOX").fixed(),
        Column::new("READY").fixed(),
        Column::new("BWRAP").fixed(),
        Column::new("LIMITS").fixed(),
        Column::new("TOOLS").fixed(),
        Column::new("LAUNCHER").path(),
    ])
    .empty_message("no executors defined");
    for def in &defs {
        let launcher = def
            .command
            .as_deref()
            .and_then(|program| runtime.locate_provider_launcher(program));
        // An executor without a `command` (e.g. `local-shell`) launches no
        // provider CLI, so availability does not apply rather than failing.
        let cli_available = def.command.as_ref().map(|_| launcher.is_some());
        // A declared `host_os` that excludes this host is not a missing
        // binary. An omitted declaration keeps the path probe.
        let unsupported_on_this_os = def.host_os.as_ref().is_some_and(|hosts| {
            !orbit_types::task::HostOs::current().is_some_and(|host| hosts.contains(&host))
        });
        let availability = if unsupported_on_this_os {
            Some("unsupported on this OS")
        } else {
            match cli_available {
                Some(true) => Some("installed"),
                Some(false) => Some("not installed"),
                None => None,
            }
        };
        let found = if unsupported_on_this_os {
            "unsupported"
        } else {
            match cli_available {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
            }
        };
        let sandbox = def.sandbox.map_or("unspecified", |kind| kind.as_str());
        let readiness = (sandbox == "linux-bwrap")
            .then_some(linux_probe.as_ref())
            .flatten();
        // Which trusted Bubblewrap the probe selected — the host's or the
        // one bundled with Orbit — and its version once it passed.
        let wrapper = readiness.and_then(|probe| probe.source.map(|source| (source, probe)));
        let limits = limit_findings
            .iter()
            .filter(|(provider, _)| provider.as_deref().is_none_or(|name| name == def.name))
            .map(|(_, row)| row)
            .collect::<Vec<_>>();
        // The worst of them, as `orbit doctor` ranks a row.
        let limit_status = [
            WorkspaceDoctorStatus::Error,
            WorkspaceDoctorStatus::Warning,
            WorkspaceDoctorStatus::Ok,
            WorkspaceDoctorStatus::Info,
            WorkspaceDoctorStatus::Skipped,
        ]
        .into_iter()
        .find(|status| limits.iter().any(|row| row.status == *status));
        // Whether the effective args yield tool-call traces, independent of
        // whether the binary or sandbox is available.
        let telemetry = tool_telemetry::classify(def);
        // Only a row the operator can act on gets prose under the table; the
        // rest stay in the TOOLS column and the JSON `tool_telemetry` object.
        if let (Some(cause), Some(remediation)) =
            (telemetry.cause.as_deref(), telemetry.remediation.as_deref())
        {
            telemetry_notes.push(format!(
                "{} tool telemetry {}: {cause} {remediation}",
                def.name,
                telemetry.label()
            ));
        }
        values.push(json!({
            "name": def.name,
            "executor_type": def.executor_type.to_string(),
            "command": def.command,
            "args": def.args,
            "cli_available": cli_available,
            "availability": availability,
            "launcher": launcher.as_ref().map(|path| path.display().to_string()),
            "sandbox": def.sandbox,
            "sandbox_ready": readiness.map(|probe| probe.available),
            "sandbox_readiness_detail": readiness.map(|probe| probe.detail.as_str()),
            "sandbox_wrapper": wrapper.map(|(source, _)| source.as_str()),
            "sandbox_wrapper_path": wrapper.map(|(_, probe)| probe.trusted_path.as_str()),
            "sandbox_wrapper_version": wrapper.and_then(|(_, probe)| probe.version.as_deref()),
            "allow_fallback": def.allow_fallback,
            "tool_telemetry": telemetry.to_json(),
            "provider_limits": limits
                .iter()
                .map(|row| orbit_cmd::doctor_row_json(row))
                .collect::<Vec<_>>(),
        }));
        table.add_row(vec![
            def.name.clone(),
            def.executor_type.to_string(),
            def.command.clone().unwrap_or_else(|| "-".to_string()),
            found.to_string(),
            sandbox.to_string(),
            readiness.map_or_else(
                || "-".to_string(),
                |probe| if probe.available { "yes" } else { "no" }.to_string(),
            ),
            wrapper.map_or_else(
                || "-".to_string(),
                |(source, probe)| match &probe.version {
                    Some(version) => format!("{} {version}", source.as_str()),
                    None => source.as_str().to_string(),
                },
            ),
            limit_status.map_or_else(
                || "-".to_string(),
                |status| report::status_label(status).to_string(),
            ),
            telemetry.label(),
            launcher.map_or_else(|| "-".to_string(), |path| path.display().to_string()),
        ]);
    }
    let mut blocks = vec![Block::table(table)];
    blocks.extend(telemetry_notes.into_iter().map(Block::notice));
    let records = values.clone();
    Ok(Payload::blocks(Value::Array(values), blocks)
        .with_records(records)
        .into())
}
