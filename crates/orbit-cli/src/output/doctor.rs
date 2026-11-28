//! Human and structured output for `orbit doctor` (`providers`, `fs-access`,
//! `admission-receipts`, and workspace diagnostics), including severity-ordered
//! findings shown after the terminal table.

use comfy_table::{Cell, ColumnConstraint, ContentArrangement, Table as Grid, Width, presets};
use orbit_common::OrbitError;
use orbit_core::app::doctor::{
    AttemptPinCleanup, OrphanTaskStoreRemoval, Outcome, Providers, Repaired,
    RetiredConfigKeyRepair, WorkspaceDoctorResult, WorkspaceDoctorStatus, doctor_row_json,
};
use orbit_types::policy::DEFAULT_POLICY_NAME;
use orbit_types::telemetry::incident::DOCTOR_FINDINGS_MESSAGE_PREFIX;
use orbit_types::workflow::ExecutorDef;
use serde_json::{Value, json};

use crate::output::common::color::{Domain, Role};
use crate::output::common::payload::{Block, CommandOutput, Payload};
use crate::output::common::sink::OutputMode;
use crate::output::common::table::{Column, Table};

/// Which diagnostic rows `orbit doctor` renders.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoctorReportFilter {
    /// Every diagnostic row.
    AllChecks,
    /// Only `Error` and `Warning` rows.
    ProblemsOnly,
}

/// Whether an executor's effective arguments yield tool-call traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolTelemetryStatus {
    /// The configured transport carries tool frames Orbit parses.
    Supported,
    /// The configured transport is valid for answers and usage but carries no
    /// tool frames, so no tool calls or first-edit samples can be recorded.
    Unavailable,
    /// Orbit does not recognise this configuration as supplying traces.
    Unknown,
    /// The executor launches no provider CLI.
    NotApplicable,
}

impl ToolTelemetryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Unavailable => "unavailable",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// The classification of one executor definition.
struct ToolTelemetry {
    status: ToolTelemetryStatus,
    /// The `--output-format` the effective arguments select, when one does.
    output_format: Option<String>,
    cause: Option<String>,
    remediation: Option<String>,
}

impl ToolTelemetry {
    /// The short text-table label.
    fn label(&self) -> String {
        match (self.status, self.output_format.as_deref()) {
            (ToolTelemetryStatus::NotApplicable, _) => "-".to_string(),
            (status, Some(format)) if status != ToolTelemetryStatus::Unknown => {
                format!("{} ({format})", status.as_str())
            }
            (status, _) => status.as_str().to_string(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "status": self.status.as_str(),
            "output_format": self.output_format,
            "cause": self.cause,
            "remediation": self.remediation,
        })
    }
}

/// Full doctor findings block shown only after the terminal table.
pub fn findings_block(rows: Vec<WorkspaceDoctorResult>) -> Block {
    Block::custom(move |sink| {
        if sink.mode() == OutputMode::Table {
            let findings = doctor_findings(&rows, sink.width());
            if !findings.is_empty() {
                println!("\n{findings}");
            }
        }
    })
}

/// Report one finished repair on stderr and add its row.
pub(crate) fn push_repaired(
    results: &mut Vec<WorkspaceDoctorResult>,
    repair: Repaired,
    started: std::time::Instant,
) {
    let (check_name, status, message) = match repair {
        Repaired::MissingPrRefs(repaired) => (
            "fix-missing-pr-refs",
            WorkspaceDoctorStatus::Ok,
            format!(
                "Restored pull request references on {} review tasks.",
                repaired.len()
            ),
        ),
        Repaired::StateDirectoryPermissions(repaired) => (
            "fix-state-directory-permissions",
            WorkspaceDoctorStatus::Ok,
            format!(
                "Restricted {repaired} writable Orbit state director{} to owner-only access.",
                if repaired == 1 { "y" } else { "ies" }
            ),
        ),
        Repaired::StaleLocks(cleared) => (
            "fix-stale-locks",
            WorkspaceDoctorStatus::Ok,
            format!("Cleared {cleared} stale lock holder record(s)."),
        ),
        Repaired::StaleTaskLocks(released) => (
            "fix-stale-task-locks",
            WorkspaceDoctorStatus::Ok,
            format!("Released {released} stale task reservation(s)."),
        ),
        Repaired::Graph(removed) => (
            "remove-graph",
            WorkspaceDoctorStatus::Ok,
            format!("Removed {removed} retired graph location(s)."),
        ),
        Repaired::StaleArtifacts(removed) => (
            "fix-stale-artifacts",
            WorkspaceDoctorStatus::Ok,
            format!("Retired {removed} deprecated definition artifact(s)."),
        ),
        Repaired::OrphanTaskStores(removed) => (
            "fix-orphan-task-stores",
            WorkspaceDoctorStatus::Ok,
            orphan_task_store_removal_message(&removed),
        ),
        Repaired::AutomationPins(cleanup) => (
            "fix-automation-pins",
            if cleanup.kept.is_empty()
                && (cleanup.refused.is_none() || cleanup.retained_unproven == 0)
            {
                WorkspaceDoctorStatus::Ok
            } else {
                WorkspaceDoctorStatus::Warning
            },
            automation_pin_cleanup_message(&cleanup),
        ),
        Repaired::RetiredConfigKeys(repair) => (
            "fix-retired-config-keys",
            WorkspaceDoctorStatus::Ok,
            retired_config_key_repair_message(&repair),
        ),
    };
    push_repair_result(results, check_name, started, status, message);
}

/// Render the checks as `orbit doctor` reports them: a table sorted by
/// severity, JSON check rows, an NDJSON summary trailer, and a nonzero exit
/// when any check failed.
pub(crate) fn report_output(
    mut results: Vec<WorkspaceDoctorResult>,
    filter: DoctorReportFilter,
    check: Option<&str>,
) -> Result<CommandOutput, OrbitError> {
    let problems = filter == DoctorReportFilter::ProblemsOnly;
    if let Some(check) = check {
        let row = results
            .iter()
            .find(|row| row.check_name == check)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "no workspace diagnostic named '{check}'; run `orbit doctor --json` to list valid check names"
                ))
            })?;
        let failed = row.status == WorkspaceDoctorStatus::Error;
        let mut payload = Payload::detail(
            doctor_row_json(row),
            format!(
                "Check: {}\nStatus: {}\nDetails: {}",
                row.check_name,
                status_label(row.status),
                human_detail(row)
            ),
        )
        .with_exit_code(i32::from(failed));
        if failed {
            payload =
                payload.with_audit_message(findings_audit_message(std::slice::from_ref(row), 0));
        }
        return Ok(payload.into());
    }
    let failures = results
        .iter()
        .filter(|row| row.status == WorkspaceDoctorStatus::Error)
        .count();
    let warnings = results
        .iter()
        .filter(|row| row.status == WorkspaceDoctorStatus::Warning)
        .count();

    let summary = format!("{failures} failure(s), {warnings} warning(s).");
    let summary_record = json!({"summary": {
        "failures": failures,
        "warnings": warnings,
        "text": summary,
    }});
    let values = results.iter().map(doctor_row_json).collect::<Vec<_>>();
    let mut records = values.clone();
    records.push(summary_record);
    // Human modes always show the count first. JSON and NDJSON keep stderr
    // silent unless a check failed, so a nonzero exit still names its cause.
    let summary_block = if failures > 0 {
        Block::notice(summary.clone())
    } else {
        let summary_notice = summary.clone();
        Block::custom(move |sink| {
            eprintln!("{}", sink.wrap_prose(&summary_notice));
        })
    };
    let mut blocks = vec![summary_block];

    results.sort_by_key(|row| match row.status {
        WorkspaceDoctorStatus::Error => 0,
        WorkspaceDoctorStatus::Warning => 1,
        WorkspaceDoctorStatus::Ok => 2,
        WorkspaceDoctorStatus::Info => 3,
        WorkspaceDoctorStatus::Skipped => 4,
    });

    if problems {
        results.retain(|row| {
            matches!(
                row.status,
                WorkspaceDoctorStatus::Error | WorkspaceDoctorStatus::Warning
            )
        });
    }

    {
        let mut table = Table::new(vec![
            Column::new("CHECK").fixed(),
            Column::new("STATUS").fixed(),
            Column::new("DETAILS"),
        ])
        .empty_message(if problems {
            "no problems found"
        } else {
            "no workspace checks ran"
        });
        for row in &results {
            table.add_row(vec![
                Cell::new(&row.check_name),
                crate::output::common::color::cell(status_label(row.status), Domain::DoctorStatus),
                Cell::new(human_detail(row).replace('\n', " | ")),
            ]);
        }
        blocks.push(Block::table(table));
        blocks.push(findings_block(results.clone()));

        if failures == 0 && warnings == 0 {
            let healthy_notice = crate::output::common::color::text("Workspace healthy.", Role::Ok);
            blocks.push(Block::custom(move |sink| {
                eprintln!("{}", sink.wrap_prose(&healthy_notice));
            }));
        }
    }

    // Unlike `skill doctor` / `tool doctor`, a failed check exits nonzero
    // so unattended callers (cron, CI, systemd) can alert on it.
    let exit_code = i32::from(failures > 0);
    let mut payload = Payload::blocks(Value::Array(values), blocks)
        .with_records(records)
        .with_exit_code(exit_code);
    if failures > 0 {
        payload = payload.with_audit_message(findings_audit_message(&results, warnings));
    }
    Ok(payload.into())
}

/// Build the payload for `orbit doctor providers`.
pub(crate) fn providers_payload(providers: Providers) -> Payload {
    let Providers {
        executors,
        linux_probe,
        limit_findings,
    } = providers;
    let mut values = Vec::with_capacity(executors.len());
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
    for (def, launcher) in &executors {
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
        let telemetry = classify_tool_telemetry(def);
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
                .map(|row| doctor_row_json(row))
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
                |status| status_label(status).to_string(),
            ),
            telemetry.label(),
            launcher
                .as_ref()
                .map_or_else(|| "-".to_string(), |path| path.display().to_string()),
        ]);
    }
    let mut blocks = vec![Block::table(table)];
    blocks.extend(telemetry_notes.into_iter().map(Block::notice));
    let records = values.clone();
    Payload::blocks(Value::Array(values), blocks).with_records(records)
}

/// Build the payload for `orbit doctor fs-access`.
pub(crate) fn fs_access_payload(profile: &str, path: &str, outcome: Outcome) -> Payload {
    let Outcome::FsAccess { read, modify } = outcome else {
        unreachable!("an fs-access command answers with its verdicts")
    };
    let doc = json!({
        "policy": DEFAULT_POLICY_NAME,
        "profile": profile,
        "path": path,
        "read": {
            "allowed": read.allowed,
            "matched_rule": read.matched_rule,
        },
        "modify": {
            "allowed": modify.allowed,
            "matched_rule": modify.matched_rule,
        },
    });
    let verdict = |allowed: bool| if allowed { "allowed" } else { "denied" };
    let text = format!(
        "Policy:  {DEFAULT_POLICY_NAME}\nProfile: {profile}\nPath:    {path}\nread:    {} ({})\nmodify:  {} ({})",
        verdict(read.allowed),
        read.matched_rule,
        verdict(modify.allowed),
        modify.matched_rule
    );
    Payload::detail(doc, text)
}

/// Build the payload for `orbit doctor admission-receipts`.
pub(crate) fn admission_receipts_payload(outcome: Outcome) -> Payload {
    let Outcome::AdmissionReceipts {
        compacted_receipts,
        storage,
    } = outcome
    else {
        unreachable!("an admission-receipts command answers with its storage")
    };
    let text = format!(
        "Compacted admission receipts: {compacted_receipts}\nFull admission receipts: {} ({} logical payload bytes)\nPermanent tombstones: {} ({} logical payload bytes)\nClaim mutation receipts: {} ({} logical payload bytes; pruned separately on settlement after 7 days)\nLogical payload bytes exclude row keys, indexes, free pages and WAL.",
        storage.full_receipts.count,
        storage.full_receipts.logical_payload_bytes,
        storage.tombstones.count,
        storage.tombstones.logical_payload_bytes,
        storage.claim_mutation_receipts.count,
        storage.claim_mutation_receipts.logical_payload_bytes,
    );
    Payload::blocks(
        json!({"compacted_receipts": compacted_receipts, "storage": storage}),
        vec![Block::prose(text)],
    )
}

/// Full non-ok diagnostics in severity order. Long findings and repair
/// actions wrap onto indented lines when exceeding the terminal width.
fn doctor_findings(rows: &[WorkspaceDoctorResult], width: u16) -> String {
    use orbit_core::app::doctor::WorkspaceDoctorStatus as Status;
    let mut findings = Vec::new();
    for status in [Status::Error, Status::Warning, Status::Skipped] {
        for row in rows.iter().filter(|row| row.status == status) {
            let label = match status {
                Status::Error => "ERROR",
                Status::Warning => "WARN",
                Status::Skipped => "SKIP",
                // Informational rows are facts, not findings; the table
                // shows them.
                Status::Ok | Status::Info => continue,
            };
            let mut finding = format!(
                "{}  {label}\n{}",
                row.check_name,
                wrap_finding_message(&row.message, width)
            );
            if let Some(fix) = &row.remediation {
                finding.push('\n');
                finding.push_str(&wrap_action(fix, width));
            }
            findings.push(finding);
        }
    }
    if findings.is_empty() {
        String::new()
    } else {
        format!("Findings:\n\n{}", findings.join("\n\n"))
    }
}

/// Wrap a finding message onto indented lines, keeping each line within the terminal width.
fn wrap_finding_message(message: &str, width: u16) -> String {
    let indent = "  ";
    if width == 0 {
        return message
            .lines()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{indent}{line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    let available = width.saturating_sub(indent.len() as u16).max(1);
    let wrapped = wrap_message(message, available);
    wrapped
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap repair action text at the finding's two-space indent, with wrapped
/// lines on a four-space hanging indent so the remedy reads as part of its
/// finding.
fn wrap_action(fix: &str, width: u16) -> String {
    let indent = "  ";
    let hanging = "    ";
    let prefix = format!("{indent}Fix: ");
    let single_line = format!("{indent}Fix: {fix}");
    if width == 0 || single_line.chars().count() <= width as usize {
        return single_line;
    }
    let first_available = width.saturating_sub(prefix.chars().count() as u16).max(1) as usize;
    let rest_available = width.saturating_sub(hanging.len() as u16).max(1) as usize;

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_max = first_available;

    for word in fix.split_whitespace() {
        if current_line.is_empty() {
            current_line.push_str(word);
        } else if current_line.chars().count() + 1 + word.chars().count() <= current_max {
            current_line.push(' ');
            current_line.push_str(word);
        } else {
            lines.push(current_line);
            current_line = word.to_string();
            current_max = rest_available;
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }

    let mut out = String::new();
    let mut iter = lines.into_iter();
    if let Some(first) = iter.next() {
        if format!("{indent}Fix: {first}").chars().count() <= width as usize {
            out.push_str(&format!("{indent}Fix: {first}"));
        } else {
            out.push_str(&format!("{indent}Fix:\n{hanging}{first}"));
        }
    }
    for line in iter {
        out.push('\n');
        out.push_str(hanging);
        out.push_str(&line);
    }
    out
}

/// Use the table library's display-width-aware wrapping without the list's
/// one-line cap. No message text is truncated.
fn wrap_message(message: &str, width: u16) -> String {
    if width == 0 {
        return message.to_string();
    }
    let mut prose = Grid::new();
    prose.load_style(presets::NOTHING);
    prose.force_no_tty();
    prose.set_content_arrangement(ContentArrangement::Disabled);
    prose.add_row(vec![Cell::new(message)]);
    if let Some(column) = prose.column_mut(0) {
        column.set_padding((0, 0));
        column.set_constraint(ColumnConstraint::Absolute(Width::Fixed(width)));
    }
    prose
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn status_label(status: WorkspaceDoctorStatus) -> &'static str {
    match status {
        WorkspaceDoctorStatus::Ok => "ok",
        WorkspaceDoctorStatus::Warning => "warning",
        WorkspaceDoctorStatus::Error => "ERROR",
        WorkspaceDoctorStatus::Skipped => "skipped",
        WorkspaceDoctorStatus::Info => "info",
    }
}

fn push_repair_result(
    results: &mut Vec<WorkspaceDoctorResult>,
    check_name: &'static str,
    started: std::time::Instant,
    status: WorkspaceDoctorStatus,
    message: String,
) {
    eprintln!("{message}");
    results.push(WorkspaceDoctorResult {
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        check_name: check_name.to_string(),
        status,
        message,
        remediation: None,
    });
}

/// The audit message for a completed run that reported failed checks, e.g.
/// `doctor reported findings: 1 failure (review), 3 warnings`. The prefix is
/// what lets the incident classifier tell this verdict from a crash.
fn findings_audit_message(results: &[WorkspaceDoctorResult], warnings: usize) -> String {
    let failed = results
        .iter()
        .filter(|row| row.status == WorkspaceDoctorStatus::Error)
        .map(|row| row.check_name.as_str())
        .collect::<Vec<_>>();
    format!(
        "{DOCTOR_FINDINGS_MESSAGE_PREFIX}{} {} ({}), {warnings} {}",
        failed.len(),
        if failed.len() == 1 {
            "failure"
        } else {
            "failures"
        },
        failed.join(", "),
        if warnings == 1 { "warning" } else { "warnings" },
    )
}

/// Render `--fix-orphan-task-stores --confirm`'s outcome, naming the empty
/// and populated partition counts separately so a run that deleted task
/// bundles is never described as removing only empty partitions [ORB-12144].
fn orphan_task_store_removal_message(removed: &OrphanTaskStoreRemoval) -> String {
    format!(
        "Removed {} empty orphaned task-store partition(s) and {} populated partition(s) \
         ({} task bundle(s)).",
        removed.empty_partitions, removed.populated_partitions, removed.task_bundles
    )
}

/// Render `--fix-retired-config-keys`: which replacement keys were written,
/// which keys were removed, and the backup written beside each edited file.
/// An empty repair changed nothing.
fn retired_config_key_repair_message(repair: &RetiredConfigKeyRepair) -> String {
    if repair.removed.is_empty() && repair.written.is_empty() {
        return "No retired, deprecated or unknown config keys were present.".to_string();
    }
    let mut groups: Vec<(String, String, Vec<&str>)> = Vec::new();
    for entry in &repair.removed {
        let config = entry.config_path.display().to_string();
        let backup = entry.backup_path.display().to_string();
        if let Some(group) = groups.iter_mut().find(|group| group.0 == config) {
            group.2.push(entry.key.as_str());
        } else {
            groups.push((config, backup, vec![entry.key.as_str()]));
        }
    }
    let detail = groups
        .iter()
        .map(|(config, backup, keys)| {
            format!("{} from {config} (backup {backup})", keys.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let count = repair.removed.len();
    let removed = format!(
        "Removed {count} retired, deprecated or unknown config key{}: {detail}",
        if count == 1 { "" } else { "s" }
    );
    let written = repair
        .written
        .iter()
        .map(|entry| format!("{} in {}", entry.key, entry.config_path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    match (count, written.is_empty()) {
        (_, true) => format!("{removed}."),
        (0, false) => format!("Rewrote deprecated config values: {written}."),
        (_, false) => format!("{removed}; wrote their replacements first: {written}."),
    }
}

/// Render `--fix-automation-pins`'s outcome: what it reclaimed, why the rest
/// of its own namespace stayed, and every pin it retained without proof of
/// ownership.
fn automation_pin_cleanup_message(cleanup: &AttemptPinCleanup) -> String {
    let mut message = match &cleanup.refused {
        Some(reason) => format!(
            "Reclaimed no automation attempt pins: ownership of {} is not proved ({reason}); \
             retained {} owned pin(s).",
            cleanup.namespace, cleanup.retained_unproven,
        ),
        None => format!(
            "Reclaimed {} unreferenced automation attempt pin(s) under {}; kept {} in flight, \
             {} live-run.",
            cleanup.released.len(),
            cleanup.namespace,
            cleanup.retained_active,
            cleanup.retained_live_run,
        ),
    };
    message.push_str(&format!(
        " Retained {} legacy shared pin(s) under refs/orbit/automation/ with no recorded owner \
         and {} pin namespace(s) owned by other Orbit roots or workspaces.",
        cleanup.retained_legacy, cleanup.foreign_owners,
    ));
    if !cleanup.kept.is_empty() {
        message.push_str(&format!(
            " Could not delete (moved or locked): {}.",
            cleanup.kept.join(", ")
        ));
    }
    if !cleanup.unrecognized.is_empty() {
        message.push_str(&format!(
            " Left unrecognized refs untouched: {}.",
            cleanup.unrecognized.join(", ")
        ));
    }
    message
}

fn human_detail(row: &WorkspaceDoctorResult) -> String {
    let mut detail = row.remediation.as_ref().map_or_else(
        || row.message.clone(),
        |remediation| format!("{}\nAction: {remediation}", row.message),
    );
    if row.duration_ms > 1000 {
        detail.push_str(&format!(" ({:.2} s)", row.duration_ms as f64 / 1000.0));
    }
    detail
}

/// Classify `def` by its effective `args` for tool-telemetry output.
fn classify_tool_telemetry(def: &ExecutorDef) -> ToolTelemetry {
    let Some(command) = def.command.as_deref() else {
        return ToolTelemetry {
            status: ToolTelemetryStatus::NotApplicable,
            output_format: None,
            cause: None,
            remediation: None,
        };
    };
    let program = std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    if def.name != "grok" && program != "grok" {
        return ToolTelemetry {
            status: ToolTelemetryStatus::Unknown,
            output_format: None,
            cause: Some(
                "Orbit does not classify tool-call output for this executor's arguments; \
                 its runs are not claimed to record tool calls or first-edit samples."
                    .to_string(),
            ),
            remediation: None,
        };
    }
    classify_grok(output_format(&def.args))
}

fn classify_grok(format: Option<String>) -> ToolTelemetry {
    let (status, cause, remediation) = match format.as_deref() {
        Some("streaming-json") => (ToolTelemetryStatus::Supported, None, None),
        Some("json") => (
            ToolTelemetryStatus::Unavailable,
            Some(
                "The executor passes `--output-format json`, so grok prints one result \
                 document with no tool-call frames. Runs still succeed and their answer \
                 and usage are read from that document, but no tool calls are recorded: \
                 first-edit (turns-to-edit) analytics show every such run as an empty \
                 trace."
                    .to_string(),
            ),
            Some(
                "Change `--output-format json` to `--output-format streaming-json` in the \
                 installed grok executor definition (resources/executors/grok.yaml in the \
                 Orbit home), as the shipped definition does. Orbit does not edit it for \
                 you; only runs started afterwards record tool calls."
                    .to_string(),
            ),
        ),
        Some(_) => (
            ToolTelemetryStatus::Unknown,
            Some(
                "The executor selects an `--output-format` Orbit does not recognise as \
                 carrying grok tool-call frames; it is not claimed to record tool calls."
                    .to_string(),
            ),
            Some(
                "Use `--output-format streaming-json` to record tool calls and first-edit \
                 samples."
                    .to_string(),
            ),
        ),
        None => (
            ToolTelemetryStatus::Unknown,
            Some(
                "The executor sets no `--output-format`, so the transport is grok's own \
                 default and is not claimed to record tool calls."
                    .to_string(),
            ),
            Some(
                "Add `--output-format streaming-json` to record tool calls and first-edit \
                 samples."
                    .to_string(),
            ),
        ),
    };
    ToolTelemetry {
        status,
        output_format: format,
        cause,
        remediation,
    }
}

/// The last `--output-format <value>` or `--output-format=<value>` in `args`.
fn output_format(args: &[String]) -> Option<String> {
    let mut found = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--output-format" {
            found = iter.next().cloned();
        } else if let Some(value) = arg.strip_prefix("--output-format=") {
            found = Some(value.to_string());
        }
    }
    found
}
