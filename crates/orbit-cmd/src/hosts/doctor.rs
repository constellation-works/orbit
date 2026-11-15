//! The `hosts` row of `orbit doctor`.

use std::path::Path;

use orbit_common::HostRegistryCode;
use orbit_registry::hosts::{HostEntry, load_host_registry};

use super::{HostRow, in_parallel, remote_row, replica_owners};
use crate::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

const CHECK: &str = "hosts";

/// Probe every registered host and report what would break a route.
///
/// Fails when the host file is invalid, when the legacy file exists, or when
/// a host this machine pulls from (the owner of a local replica
/// checkout) runs a pull protocol or version that pull admission refuses.
/// Warns on an unreachable host, any other version or protocol difference, a
/// reachable host that reports no version or protocol fingerprint (an unknown
/// is never a match, and fails like a difference on a host pulled from), a
/// replica owner with no entry.
pub fn doctor_hosts_row(global_root: &Path) -> WorkspaceDoctorResult {
    let registry = match load_host_registry(global_root) {
        Ok(registry) => registry,
        Err(error) => {
            let remediation = if error.host_registry_code()
                == Some(HostRegistryCode::HostFileConflict)
            {
                "Move mcp-destinations.toml aside, run `orbit host add <ssh-target>` for each \
                 `ssh` row, then delete the moved file."
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
    let targets = registry.hosts().entries();
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    for owner in &pulls_from {
        if !targets
            .iter()
            .any(|target| target.machine_id.as_str() == owner)
        {
            warnings.push(format!(
                "{owner} owns a replica checkout here but has no host entry, so pull drains \
                 cannot reach it"
            ));
        }
    }
    let rows = in_parallel(targets, |target| remote_row(target, &local.id, true));
    for (target, host) in targets.iter().zip(&rows) {
        classify(
            target,
            host,
            pulls_from.contains(target.machine_id.as_str()),
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
    let remediation =
        "Run `orbit host list` for each host's live state. Deploy matching Orbit builds where \
             versions or protocols differ, and register missing owners with \
             `orbit host add <ssh-target>`."
            .to_string();
    row(status, message, Some(&remediation))
}

fn classify(
    target: &HostEntry,
    host: &HostRow,
    pulled_from: bool,
    failures: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    let name = &target.name;
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
    // A field the host did not report is not a match: pull admission refuses a
    // missing fingerprint, so an older owner omitting its facts is skew too.
    let unknown = [
        ("binary_version", host.binary_version.is_none()),
        ("protocol_fingerprint", host.protocol_fingerprint.is_none()),
    ]
    .into_iter()
    .filter_map(|(field, missing)| missing.then_some(field))
    .collect::<Vec<_>>();
    if !host.skew && unknown.is_empty() {
        return;
    }
    let mut details = Vec::new();
    if host.skew {
        details.push(format!("differs in {}", host.skew_fields.join(" and ")));
    }
    if !unknown.is_empty() {
        details.push(format!("did not report {}", unknown.join(" and ")));
    }
    let line = format!(
        "{name} {} (version {}, protocol {})",
        details.join(" and "),
        host.binary_version.as_deref().unwrap_or("unknown"),
        host.protocol_fingerprint.as_deref().unwrap_or("unknown")
    );
    // Pull admission refuses any version or fingerprint difference or absence,
    // so on a host this machine pulls from every skew stops the drain.
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
