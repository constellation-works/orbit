//! The `hosts` row of `orbit doctor`.

use std::path::Path;

use orbit_common::HostRegistryCode;
use orbit_registry::hosts::{RegisteredHosts, legacy_destinations_path, load_host_registry};

use super::{HostRow, RemoteTarget, in_parallel, remote_row, remote_targets, replica_owners};
use crate::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

const CHECK: &str = "hosts";

/// Probe every registered host and report what would break a route.
///
/// Fails when the host file is invalid, when it and the legacy file both
/// exist, or when a host this machine pulls from (the owner of a local replica
/// checkout) runs a pull protocol or version that pull admission refuses.
/// Warns on an unreachable host, any other version or protocol difference, a
/// replica owner with no entry, and while only the legacy file exists.
pub fn doctor_hosts_row(global_root: &Path) -> WorkspaceDoctorResult {
    let registry = match load_host_registry(global_root) {
        Ok(registry) => registry,
        Err(error) => {
            let remediation = if error.host_registry_code()
                == Some(HostRegistryCode::HostFileConflict)
            {
                "Confirm `orbit host list` would show every legacy row once the legacy file is \
                 gone (re-add any that are missing with `orbit host add <ssh-target>`), then \
                 delete the legacy file."
            } else {
                "Repair the host file named in the diagnostic (or restore it from backup), then \
                 rerun `orbit doctor`."
            };
            return row(
                WorkspaceDoctorStatus::Error,
                error.to_string(),
                Some(remediation),
            );
        }
    };
    let Some(local) = registry.local() else {
        return row(
            WorkspaceDoctorStatus::Skipped,
            "this machine has no [machine] identity".to_string(),
            None,
        );
    };
    let pulls_from = replica_owners(global_root);
    let targets = remote_targets(registry.hosts());
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut migration_action = None;
    if let RegisteredHosts::Legacy(rows) = registry.hosts() {
        let command = rows
            .iter()
            .find(|row| row.machine_id != local.id)
            .map(|row| format!("orbit host add {}", row.ssh))
            .unwrap_or_else(|| "orbit host add <ssh-target>".to_string());
        migration_action = Some(format!(
            "Run `{command}` to migrate to hosts.toml and retire the legacy file. Every retained \
             host must answer; remove a decommissioned row with `orbit host remove <host>` first."
        ));
        warnings.push(format!(
            "hosts are still read from the legacy '{}'; run `{command}` to migrate them",
            legacy_destinations_path(global_root).display(),
        ));
    }
    for owner in &pulls_from {
        if !targets.iter().any(|target| target.machine_id() == owner) {
            warnings.push(format!(
                "{owner} owns a replica checkout here but has no host entry, so pull drains \
                 cannot reach it"
            ));
        }
    }
    let rows = in_parallel(&targets, |target| remote_row(target, &local.id, true));
    for (target, host) in targets.iter().zip(&rows) {
        classify(
            target,
            host,
            pulls_from.contains(target.machine_id()),
            &mut failures,
            &mut warnings,
        );
    }
    if failures.is_empty() && warnings.is_empty() {
        let message = if targets.is_empty() {
            "no remote hosts are registered".to_string()
        } else {
            format!(
                "{} registered host(s) reachable with matching version and protocol",
                targets.len()
            )
        };
        return row(WorkspaceDoctorStatus::Ok, message, None);
    }
    let status = if failures.is_empty() {
        WorkspaceDoctorStatus::Warning
    } else {
        WorkspaceDoctorStatus::Error
    };
    let message = failures
        .into_iter()
        .chain(warnings)
        .collect::<Vec<_>>()
        .join("; ");
    let remediation = migration_action.unwrap_or_else(|| {
        "Run `orbit host list` for each host's live state. Deploy matching Orbit builds where \
             versions or protocols differ, and register missing owners with \
             `orbit host add <ssh-target>`."
            .to_string()
    });
    row(status, message, Some(&remediation))
}

fn classify(
    target: &RemoteTarget<'_>,
    host: &HostRow,
    pulled_from: bool,
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let name = target.name();
    if let Some(error) = &host.error {
        let line = format!("{name}: {}", error.message);
        if error.code == HostRegistryCode::HostIdentityMismatch.as_str() {
            failures.push(line);
        } else {
            warnings.push(format!(
                "{name} is unreachable ({}): {}",
                error.code, error.message
            ));
        }
        return;
    }
    if !host.skew {
        return;
    }
    let line = format!(
        "{name} differs in {} (version {}, protocol {})",
        host.skew_fields.join(" and "),
        host.binary_version.as_deref().unwrap_or("unknown"),
        host.protocol_fingerprint.as_deref().unwrap_or("unknown")
    );
    // Pull admission refuses any version or fingerprint difference, so on a
    // host this machine pulls from every skew stops the drain.
    if pulled_from {
        failures.push(format!("{line}; pull admission from it will refuse"));
    } else {
        warnings.push(line);
    }
}

fn row(
    status: WorkspaceDoctorStatus,
    message: String,
    remediation: Option<&str>,
) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        check_name: CHECK.to_string(),
        duration_ms: 0,
        status,
        message,
        remediation: remediation.map(ToOwned::to_owned),
    }
}
