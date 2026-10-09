//! The complete `orbit doctor` report: the workspace checks, then the host,
//! provider and client checks around them.
//!
//! The CLI and the dashboard's Doctor panel both run [`doctor_report_probes`],
//! so the two surfaces report the same checks with the same fields. Every
//! probe here is read-only; repairs stay separate `DoctorCommands` methods
//! that only the CLI's `--fix-*` flags call.

use orbit_config::{ConfigRoots, ResolvedConfig, canonical_crew_pool};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use super::git_protection::git_protection_row;
use super::permissions::state_directory_permissions_row;
use super::{DoctorProbe, WORKSPACE_PROBES, WorkspaceDoctorResult, WorkspaceDoctorStatus};

/// The checks after the workspace ones, in report order.
const REPORT_PROBES: &[DoctorProbe] = &[
    DoctorProbe::many("build-budget", |runtime, _| build_budget_rows(runtime)),
    DoctorProbe::one("state-directory-permissions", |runtime, _| {
        state_directory_permissions_row(runtime)
    }),
    DoctorProbe::one("git-protection", |runtime, _| git_protection_row(runtime)),
    DoctorProbe::many("provider", |runtime, _| routed_provider_rows(runtime)),
    DoctorProbe::many("provider-auth", |runtime, _| provider_auth_rows(runtime)),
    DoctorProbe::many("provider-limits", |runtime, _| provider_limit_rows(runtime)),
    DoctorProbe::one("mcp-registration", |runtime, _| {
        mcp_registration_row(runtime, orbit_common::fs::path::home_dir().ok().as_deref())
    }),
    // Machine-global checks: they read the operator's home, not the workspace.
    DoctorProbe::many("mcp-callers", |_, _| caller_authorization_rows()),
    DoctorProbe::one("clock-unit", |runtime, _| {
        clock_unit_row(&runtime.global_root())
    }),
    DoctorProbe::one("hosts", |runtime, _| {
        crate::hosts::doctor_hosts_row(&runtime.global_root())
    }),
];

fn build_budget_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    match runtime.build_budget_capacity_warnings() {
        Ok(warnings) if warnings.is_empty() => vec![WorkspaceDoctorResult {
            duration_ms: 0, check_name: "build-budget".into(), status: WorkspaceDoctorStatus::Ok,
            message: "No running drain exceeds host build slots.".into(), remediation: None,
        }],
        Ok(warnings) => warnings.into_iter().map(|warning| WorkspaceDoctorResult {
            duration_ms: 0, check_name: "build-budget".into(), status: WorkspaceDoctorStatus::Warning,
            message: format!("{}: {}", warning["run_id"].as_str().unwrap_or("drain"), warning["message"].as_str().unwrap_or("capacity mismatch")),
            remediation: Some(format!("Set ORBIT_BUILD_SLOTS or edit {}; or lower orbit run concurrency.", warning["settings_file"].as_str().unwrap_or("~/.orbit/cache/build-budget/slots"))),
        }).collect(),
        Err(error) => vec![WorkspaceDoctorResult {
            duration_ms: 0, check_name: "build-budget".into(), status: WorkspaceDoctorStatus::Warning,
            message: format!("Cannot inspect build-budget capacity: {error}"),
            remediation: Some("Check ORBIT_BUILD_SLOTS and ~/.orbit/cache/build-budget/slots, then rerun orbit doctor.".into()),
        }],
    }
}

/// Every read-only check `orbit doctor` reports, in report order.
pub fn doctor_report_probes() -> impl Iterator<Item = &'static DoctorProbe> {
    WORKSPACE_PROBES.iter().chain(REPORT_PROBES)
}

/// Run every check in [`doctor_report_probes`] in order. `deep` scans every
/// database page with SQLite quick_check.
pub fn run_doctor_report(runtime: &OrbitRuntime, deep: bool) -> Vec<WorkspaceDoctorResult> {
    doctor_report_probes()
        .flat_map(|probe| probe.run(runtime, deep))
        .collect()
}

/// One row as `orbit doctor --json` and the dashboard's `/api/doctor` emit it.
pub fn doctor_row_json(row: &WorkspaceDoctorResult) -> Value {
    json!({
        "check": row.check_name,
        "duration_ms": row.duration_ms,
        "status": match row.status {
            WorkspaceDoctorStatus::Ok => "ok",
            WorkspaceDoctorStatus::Warning => "warning",
            WorkspaceDoctorStatus::Error => "error",
            WorkspaceDoctorStatus::Skipped => "skipped",
            WorkspaceDoctorStatus::Info => "info",
        },
        "message": row.message,
        "remediation": row.remediation,
    })
}

/// Check only crews that normal workflow routing can select. A disabled crew
/// is never drawn, so a missing CLI or executor for it is not a readiness
/// failure; a disabled default or system lane is reported separately as a
/// config warning. Provider auth is deliberately not probed: a status command
/// may refresh credentials or call the network, while `doctor` must stay fast
/// and read-only.
fn routed_provider_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    use std::collections::BTreeSet;

    let config = match ResolvedConfig::load(&ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    )) {
        Ok(config) => config,
        Err(error) => {
            return vec![WorkspaceDoctorResult {
                duration_ms: 0,
                check_name: "provider-routing".to_string(),
                status: WorkspaceDoctorStatus::Error,
                message: format!("cannot inspect effective crew routing: {error}"),
                remediation: Some(
                    "Repair the config reported by `orbit doctor`, then rerun it.".to_string(),
                ),
            }];
        }
    };

    let mut names = BTreeSet::new();
    if let Some(name) = &config.default_crew
        && routing_selects_crew(&config, name)
    {
        names.insert(name.clone());
    }
    if routing_selects_crew(&config, &config.system_crew) {
        names.insert(config.system_crew.clone());
    }
    for (complexity, entries) in [
        ("low", &config.complexity_crews.low),
        ("medium", &config.complexity_crews.medium),
        ("hard", &config.complexity_crews.hard),
        ("xhard", &config.complexity_crews.xhard),
    ] {
        if let Some(entries) = entries {
            match canonical_crew_pool(
                entries,
                &config.crews,
                &format!("workflow.{complexity}_complexity_crews"),
            ) {
                Ok(pool) => names.extend(
                    pool.entries
                        .into_iter()
                        .filter(|entry| {
                            entry.weight > 0 && routing_selects_crew(&config, &entry.name)
                        })
                        .map(|entry| entry.name),
                ),
                Err(error) => {
                    return vec![WorkspaceDoctorResult {
                        duration_ms: 0,
                        check_name: "provider-routing".to_string(),
                        status: WorkspaceDoctorStatus::Error,
                        message: format!("cannot inspect {complexity} crew pool: {error}"),
                        remediation: Some(
                            "Repair the config reported by `orbit doctor`, then rerun it."
                                .to_string(),
                        ),
                    }];
                }
            }
        }
    }

    names.into_iter().map(|name| {
        let check_name = format!("provider:{name}");
        let Some(crew) = config.crews.get(&name) else {
            return WorkspaceDoctorResult {
                duration_ms: 0,
                check_name,
                status: WorkspaceDoctorStatus::Error,
                message: format!("routed crew '{name}' is not configured"),
                remediation: Some(format!("Define crew '{name}' in config.toml or change workflow routing.")),
            };
        };
        let provider = &crew.assignment.provider;
        match runtime.get_executor_def(provider) {
            Ok(Some(def)) => match def.command.as_deref() {
                Some(program) => match runtime.locate_provider_launcher(program) {
                    Some(path) => WorkspaceDoctorResult {
                        duration_ms: 0,
                        check_name,
                        status: WorkspaceDoctorStatus::Ok,
                        message: format!("crew '{name}' uses provider '{provider}'; CLI '{}' found at {} (authentication not checked)", program, path.display()),
                        remediation: None,
                    },
                    None => WorkspaceDoctorResult {
                        duration_ms: 0,
                        check_name,
                        status: WorkspaceDoctorStatus::Error,
                        message: format!("crew '{name}' uses provider '{provider}'; CLI '{program}' was not found"),
                        remediation: Some(format!("Install the '{program}' CLI or change crew '{name}' to an available provider.")),
                    },
                },
                None => WorkspaceDoctorResult {
                    duration_ms: 0,
                    check_name,
                    status: WorkspaceDoctorStatus::Skipped,
                    message: format!("crew '{name}' uses provider '{provider}', which has no CLI command"),
                    remediation: None,
                },
            },
            Ok(None) => WorkspaceDoctorResult {
                duration_ms: 0,
                check_name,
                status: WorkspaceDoctorStatus::Error,
                message: format!("crew '{name}' uses provider '{provider}', but no executor definition exists"),
                remediation: Some(format!("Restore the '{provider}' executor definition or change crew '{name}'.")),
            },
            Err(error) => WorkspaceDoctorResult {
                duration_ms: 0,
                check_name,
                status: WorkspaceDoctorStatus::Error,
                message: format!("cannot inspect provider '{provider}' for crew '{name}': {error}"),
                remediation: Some("Repair executor storage, then rerun `orbit doctor`.".to_string()),
            },
        }
    }).collect()
}

/// Provider readiness follows dispatch. A disabled crew is omitted. A name
/// absent from the registry stays included so the probe can report it missing.
fn routing_selects_crew(config: &ResolvedConfig, name: &str) -> bool {
    match config.crews.get(name) {
        Some(crew) => crew.enabled,
        None => true,
    }
}

/// One warning per provider an active drain excludes for failed authentication.
fn provider_auth_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    match runtime.active_pull_auth_exclusions() {
        Ok(exclusions) => exclusions
            .into_iter()
            .map(|exclusion| WorkspaceDoctorResult {
                duration_ms: 0,
                check_name: format!("provider-auth:{}", exclusion.provider),
                status: WorkspaceDoctorStatus::Warning,
                message: exclusion.describe(),
                remediation: Some(exclusion.relogin_hint),
            })
            .collect(),
        Err(error) => vec![WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "provider-auth".into(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!("could not inspect active drain authentication exclusions: {error}"),
            remediation: Some("Inspect the drain with `orbit run show`.".into()),
        }],
    }
}

/// The `provider-limits` rows of `orbit doctor`.
fn provider_limit_rows(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    provider_limit_findings(runtime)
        .into_iter()
        .map(|(_, row)| row)
        .collect()
}

/// [ORB-14698] One `provider-limits:<provider>` row per provider an enabled
/// crew uses, from the host's provider-limit view: `warning` while a reading
/// gates it (window, use, reset and skipped crews), `ok` below its threshold,
/// and `info` for a provider that reports no usage, whose limits Orbit learns
/// only from failures. The system and review lanes are not gated, so one
/// whose crew's provider is gated gets its own warning. Each row comes with
/// the provider it concerns; `None` for a row about every provider.
pub fn provider_limit_findings(
    runtime: &OrbitRuntime,
) -> Vec<(Option<String>, WorkspaceDoctorResult)> {
    let view = runtime.provider_limits_view(chrono::Utc::now());
    let now = view.as_of;
    let row = |check_name: String, status, message: String, remediation: Option<&str>| {
        WorkspaceDoctorResult {
            duration_ms: 0,
            check_name,
            status,
            message,
            remediation: remediation.map(ToString::to_string),
        }
    };
    if let Some(error) = &view.error {
        return vec![(
            None,
            row(
                "provider-limits".into(),
                WorkspaceDoctorStatus::Warning,
                format!(
                    "could not read this host's provider usage limits, so admission gates none: {error}"
                ),
                Some("Check the host store with `orbit doctor --deep`, then rerun `orbit doctor`."),
            ),
        )];
    }
    let mut rows = view
        .providers
        .iter()
        .map(|provider| {
            let check_name = format!("provider-limits:{}", provider.provider);
            let readings = view
                .provider_readings(&provider.provider)
                .collect::<Vec<_>>();
            let finding = if provider.gated {
                let gated = readings
                    .iter()
                    .filter(|reading| reading.gated)
                    .map(|reading| reading.skipped_line(now))
                    .collect::<Vec<_>>();
                row(
                    check_name,
                    WorkspaceDoctorStatus::Warning,
                    gated.join("; "),
                    Some(
                        "Admission draws these crews again after the reset. To run closer to \
                         the limit, raise `workflow.provider_limit_overrides` for this provider.",
                    ),
                )
            } else if !provider.reports_usage && readings.is_empty() {
                row(
                    check_name,
                    WorkspaceDoctorStatus::Info,
                    format!(
                        "{}: no usage signal; Orbit learns limits from failures",
                        provider.provider
                    ),
                    None,
                )
            } else if readings.is_empty() {
                row(
                    check_name,
                    WorkspaceDoctorStatus::Ok,
                    format!(
                        "{}: no live usage reading; its crews are skipped at {}% of a window",
                        provider.provider, provider.threshold
                    ),
                    None,
                )
            } else {
                row(
                    check_name,
                    WorkspaceDoctorStatus::Ok,
                    format!(
                        "{} below its {}% limit: {}",
                        provider.provider,
                        provider.threshold,
                        readings
                            .iter()
                            .map(|reading| reading.describe(now))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None,
                )
            };
            (Some(provider.provider.clone()), finding)
        })
        .collect::<Vec<_>>();
    rows.extend(view.ungated_lanes.iter().map(|lane| {
        (
            Some(lane.provider.clone()),
            row(
                format!("provider-limits:{}", lane.setting),
                WorkspaceDoctorStatus::Warning,
                format!(
                    "{} '{}' uses {}, which is at its usage limit until {}; this lane is not \
                     gated, so its runs may fail until then",
                    lane.setting,
                    lane.crew,
                    lane.provider,
                    orbit_core::application::task::short_time(lane.until, now),
                ),
                Some(
                    "Wait for the reset, or point this setting at a crew on another provider \
                     until then.",
                ),
            ),
        )
    }));
    rows
}

fn mcp_registration_row(
    runtime: &OrbitRuntime,
    home_dir: Option<&std::path::Path>,
) -> WorkspaceDoctorResult {
    let workspace_id = runtime
        .workspace_runtime_binding()
        .map(|binding| binding.logical_workspace_id.clone())
        .or_else(|| runtime.workspace_id().ok());
    let clients = crate::mcp_clients::registered_clients_for_workspace(
        &runtime.paths().repo_root,
        workspace_id.as_deref(),
        home_dir,
    );
    if clients.is_empty() {
        WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "mcp-registration".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: "no Orbit MCP client registration found for this workspace".to_string(),
            remediation: Some("Run `orbit mcp init --auto` in this workspace, or configure a client with `orbit mcp init --client <client>`.".to_string()),
        }
    } else {
        WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "mcp-registration".to_string(),
            status: WorkspaceDoctorStatus::Ok,
            message: format!(
                "Orbit MCP registered in: {} (connection not checked)",
                clients.join(", ")
            ),
            remediation: None,
        }
    }
}

/// Whether this machine still carries retired destination-side MCP caller
/// authorization files [ORB-12564].
///
/// One row, and never an error. The files grant and refuse nothing now: an SSH
/// login to this machine is ownership of it, so a session served over SSH holds
/// the authority its argv asks for, exactly as a local one does. What is worth
/// saying is that a ceiling an operator wrote is inert, because the operator
/// who wrote it has no other way to find out.
fn caller_authorization_rows() -> Vec<WorkspaceDoctorResult> {
    let Ok(home) = orbit_common::fs::path::home_dir() else {
        return Vec::new();
    };
    let ignored = orbit_mcp::ignored_caller_authorization_paths(&home.join(".orbit"));
    if ignored.is_empty() {
        return vec![WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "mcp-callers".to_string(),
            status: WorkspaceDoctorStatus::Ok,
            message: "no retired MCP caller-authorization files; a session served over SSH \
                      holds the authority its argv asks for"
                .to_string(),
            remediation: None,
        }];
    }
    let paths = ignored
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    vec![WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: "mcp-callers".to_string(),
        status: WorkspaceDoctorStatus::Warning,
        message: format!(
            "left over from destination-side caller authorization and ignored, capping no \
             remote MCP session: {}",
            paths.join(", ")
        ),
        remediation: Some(format!(
            "Delete it: `rm -r {}`. To deny a caller, remove its key from \
             `~/.ssh/authorized_keys` — that is the only boundary this machine ever had.",
            paths.join(" ")
        )),
    }]
}

/// Whether the OS sweep-clock unit invokes this binary [ORB-12244].
fn clock_unit_row(global_root: &std::path::Path) -> WorkspaceDoctorResult {
    match orbit_core::application::routines::inspect_clock_unit() {
        Ok(inspection) => {
            let mut row = clock_unit_row_from_inspection(&inspection);
            if !matches!(
                inspection.verdict,
                orbit_core::application::routines::ClockUnitVerdict::NoUnitInstalled
            ) && let Ok(status) = orbit_core::application::routines::clock_status(global_root)
                && let Some(issue) = status.health_issue
            {
                if row.status != WorkspaceDoctorStatus::Error {
                    row.status = WorkspaceDoctorStatus::Warning;
                }
                row.message.push_str(&format!("; {issue}"));
                row.remediation = Some("Inspect `orbit clock status` and the sweep service log, then run `orbit clock repair`.".into());
            }
            row
        }
        Err(error) => WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "clock-unit".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!("could not inspect the sweep clock unit: {error}"),
            remediation: Some(
                "Fix the home-directory or unit-file error named above, then rerun `orbit doctor`."
                    .to_string(),
            ),
        },
    }
}

fn clock_unit_row_from_inspection(
    inspection: &orbit_core::application::routines::ClockUnitInspection,
) -> WorkspaceDoctorResult {
    use orbit_core::application::routines::ClockUnitVerdict;

    let status = match inspection.verdict {
        ClockUnitVerdict::Matching => WorkspaceDoctorStatus::Ok,
        ClockUnitVerdict::NoUnitInstalled => WorkspaceDoctorStatus::Skipped,
        ClockUnitVerdict::PathMismatch
        | ClockUnitVerdict::InvocationMismatch
        | ClockUnitVerdict::SafetyMismatch { .. }
        | ClockUnitVerdict::Unrunnable { .. } => WorkspaceDoctorStatus::Warning,
        ClockUnitVerdict::VersionMismatch => WorkspaceDoctorStatus::Error,
    };
    WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: "clock-unit".to_string(),
        status,
        message: inspection.doctor_message(),
        remediation: inspection.doctor_remediation(),
    }
}
