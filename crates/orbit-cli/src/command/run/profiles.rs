//! `orbit run profiles` — the host's delivery resource profiles, and with
//! `--accuracy` how well admission forecasts matched the measured host.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::host_resource::accuracy::{
    AccuracySelection, ErrorSummary, ForecastAccuracy,
};
use orbit_core::runtime::host_resource::profile::{
    BUILD_LIKE_CORES, BUILD_LIKE_MINUTES, ResourceProfile,
};
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Args)]
#[command(
    about = "Show what each kind of delivery costs on this host, minute by minute",
    after_help = "Lists one resource profile per workspace, work class and complexity, learned \
from the CPU and memory each delivery's workers recorded. A delivery is the chain of runs for \
one task, counted from its admission; resumed or retried deliveries are left out. Each profile \
covers the key's last `machine.auto_concurrency.profile_window` completed deliveries from the \
last 30 days. `auto-task:<template>` profiles have no complexity.\n\n\
HORIZON is the minute at which fewer than one in ten deliveries is still running, capped at \
`horizon_cap_minutes`. PEAK_CPU is the highest mean cores before it and PEAK_MEM the highest \
90th-percentile memory, each with the minute it occurs. SPREAD says how far deliveries disagree \
with that average (per-minute standard deviation over mean, for CPU and memory): near 0 is a \
profile stable enough to forecast with, near or above 1 one that is not. DELIVERIES marks a key \
with fewer than `min_profile_deliveries` as sparse; a lookup for it falls back to a wider \
profile.\n\n\
BUILD_FLAG marks no-diff profiles, and templates whose tasks are `no-diff-expected`, where a \
delivery used more than 1 core for 2 minutes in a row: work that should make no diff is \
building. The deliveries behind each flag are named below the table. Templates that build by \
design, such as qa-sweep, are never flagged.\n\n\
`--json` emits every profile with its full per-minute series (alive, cpu_cores, cpu_stddev, \
memory_bytes, memory_mean_bytes, memory_stddev_bytes), its deliveries and its flagged \
deliveries.\n\n\
`--accuracy` reports instead how well the host load forecast held up, from what drains in \
effective auto-concurrency mode `shadow` or `enforce` recorded each admission pass over the last \
30 days by default: the forecast 5, 10, 20 and 30 minutes ahead, counting what the pass admitted, against \
the host's measured CPU and memory for that minute. Error is forecast minus measured: MEAN_ERR \
is its mean, UNDER how often the forecast was below the measured load, and WORST the largest \
miss with the minute it forecast. PENDING counts target minutes that have not fully ended at \
report time, even if partly measured; MISSING counts ended minutes without measurements. \
MISSING splits into UNSAMPLED, when no delivery run was live in that minute, PARTIAL, when a \
delivery overlapped the minute but none was running for all of it, and GAP, when a delivery was \
running for the whole minute but wrote no host row. The sampler records a host minute only when \
a worker saw it whole, so the minute a delivery starts or finishes in is PARTIAL, not a sampling \
defect. PENDING plus MISSING is the compatible unmeasured \
count. Only ended measured minutes form error pairs. \
Below, per drain, its shadow passes against what it admitted: WOULD_HOLD counts the \
tasks it admitted that gap fit would have held, WOULD_ADD those gap fit would have admitted \
that it did not. ENFORCE_ADMITTED counts the distinct tasks local enforce passes admitted \
and GAP_HELD the distinct tasks gap fit held; both stay on enforce passes only. \
Pull drains record one forecast per request, counting a returned claim. \
PULL_REQUESTS counts these forecasts, PULL_SENT the requests sent to the owner, \
PULL_WOULD_HOLD the shadow requests enforce would have held because no work fit, and \
PULL_HELD the enforce requests held for the same reason. \
`--json` adds every forecast point paired with its measured minute, and names the \
gap-fit-held task ids.\n\n\
Accuracy selectors compose: `--since` is an inclusive RFC3339 forecast timestamp (default: \
30 days ago), `--drain` matches an exact drain run id, and `--mode shadow|enforce` matches the \
recorded mode. Forecast timestamps are bounded by one report-time snapshot. Text and JSON echo the effective \
selection. This read-only report does not attest source revision, build budget or thresholds: \
a drain/time cohort is an operator selection, not proof of homogeneous configuration. \
Per-task profiles are a separate population; sparse deliveries or wide spread do not become \
calibrated confidence from selecting an accuracy cohort.\n\n\
Examples:\n  orbit run profiles\n  orbit run profiles --json\n  orbit run profiles --accuracy\n  \
orbit run profiles --accuracy --json\n  \
orbit run profiles --accuracy --since 2026-10-15T00:00:00Z --drain <RUN> --mode enforce --json"
)]
pub struct RunProfilesArgs {
    /// Compare recorded admission forecasts with the host's measured load
    #[arg(long)]
    pub accuracy: bool,
    /// Include forecasts at or after this RFC3339 timestamp (accuracy only)
    #[arg(long, requires = "accuracy", value_name = "TIMESTAMP")]
    pub since: Option<DateTime<Utc>>,
    /// Select an exact drain run id (accuracy only)
    #[arg(long, requires = "accuracy", value_name = "RUN")]
    pub drain: Option<String>,
    /// Select the recorded admission mode: shadow or enforce (accuracy only)
    #[arg(long, requires = "accuracy", value_parser = ["shadow", "enforce"], hide_possible_values = true)]
    pub mode: Option<String>,
}

impl Execute for RunProfilesArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if self.accuracy {
            let report_at = Utc::now();
            let mut selection = AccuracySelection::last_30_days(report_at);
            if let Some(since) = self.since {
                selection.since = since;
            }
            selection.drain = self.drain;
            selection.mode = self.mode;
            return accuracy(runtime, selection, report_at);
        }
        let profiles = runtime.resource_profiles()?;
        let doc = json!({
            "built_at": profiles.built_at,
            "profiles": profiles
                .profiles
                .iter()
                .map(|profile| serde_json::to_value(profile.as_ref()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    orbit_core::OrbitError::Execution(format!("encode resource profiles: {error}"))
                })?,
        });

        use crate::output::table::{Column, Table};
        use comfy_table::Cell;
        let mut table = Table::new(vec![
            Column::new("WORKSPACE").fixed(),
            Column::new("CLASS").fixed(),
            Column::new("COMPLEXITY").fixed(),
            Column::new("DELIVERIES").fixed(),
            Column::new("HORIZON").fixed(),
            Column::new("PEAK_CPU").fixed(),
            Column::new("PEAK_MEM").fixed(),
            Column::new("SPREAD").fixed(),
            Column::new("BUILD_FLAG"),
        ])
        .empty_message("no resource profiles recorded on this host");
        for profile in &profiles.profiles {
            let deliveries = if profile.sparse {
                format!("{} (sparse)", profile.delivery_count)
            } else {
                profile.delivery_count.to_string()
            };
            table.add_row(vec![
                Cell::new(&profile.key.workspace_id),
                Cell::new(&profile.key.work_class),
                Cell::new(complexity(profile)),
                Cell::new(deliveries),
                Cell::new(format!("{}m", profile.horizon_minutes)),
                Cell::new(format!(
                    "{:.1} cores @ +{}m",
                    profile.peak_cpu_cores.value, profile.peak_cpu_cores.minute
                )),
                Cell::new(format!(
                    "{:.1} GiB @ +{}m",
                    profile.peak_memory_bytes.value as f64 / GIB,
                    profile.peak_memory_bytes.minute
                )),
                Cell::new(format!(
                    "cpu {:.2} mem {:.2}",
                    profile.cpu_spread, profile.memory_spread
                )),
                Cell::new(if profile.build_like.is_empty() {
                    "-".to_string()
                } else {
                    format!("build-like ({})", profile.build_like.len())
                }),
            ]);
        }
        let mut blocks = vec![Block::table(table)];
        if let Some(flags) = build_flags(&profiles.profiles) {
            blocks.push(Block::text(flags));
        }
        Ok(Payload::blocks(doc, blocks).into())
    }
}

fn complexity(profile: &ResourceProfile) -> &str {
    match (&profile.key.complexity, profile.key.template()) {
        (_, Some(_)) => "-",
        (Some(complexity), None) => complexity,
        (None, None) => "unset",
    }
}

/// The deliveries behind each build flag, or `None` when nothing is flagged.
fn build_flags(profiles: &[std::sync::Arc<ResourceProfile>]) -> Option<String> {
    let flagged: Vec<_> = profiles
        .iter()
        .filter(|profile| !profile.build_like.is_empty())
        .collect();
    if flagged.is_empty() {
        return None;
    }
    let mut text = format!(
        "No-diff work with build-like load (over {BUILD_LIKE_CORES} core for \
         {BUILD_LIKE_MINUTES} minutes):\n"
    );
    for profile in flagged {
        let _ = writeln!(
            text,
            "  {} {} {}:",
            profile.key.workspace_id,
            profile.key.work_class,
            complexity(profile)
        );
        for delivery in &profile.build_like {
            let _ = writeln!(
                text,
                "    {} from +{}m, peak {:.1} cores (runs {})",
                delivery.task_id,
                delivery.from_minute,
                delivery.peak_cpu_cores,
                delivery.run_ids.join(", ")
            );
        }
    }
    Some(text)
}

/// `--accuracy`: per-offset forecast error, then per-drain shadow and enforce decisions.
fn accuracy(
    runtime: &OrbitRuntime,
    selection: AccuracySelection,
    report_at: DateTime<Utc>,
) -> CommandOut {
    use crate::output::table::{Column, Table};
    use comfy_table::Cell;

    let report = runtime.forecast_accuracy_selected(selection, report_at)?;
    let doc = serde_json::to_value(&report).map_err(|error| {
        orbit_core::OrbitError::Execution(format!("encode forecast accuracy: {error}"))
    })?;
    let mut offsets = Table::new(vec![
        Column::new("OFFSET").fixed(),
        Column::new("PAIRS").fixed(),
        Column::new("PENDING").fixed(),
        Column::new("MISSING").fixed(),
        Column::new("UNSAMPLED").fixed(),
        Column::new("PARTIAL").fixed(),
        Column::new("GAP").fixed(),
        Column::new("CPU_MEAN_ERR").fixed(),
        Column::new("CPU_UNDER").fixed(),
        Column::new("CPU_WORST").fixed(),
        Column::new("MEM_MEAN_ERR").fixed(),
        Column::new("MEM_UNDER").fixed(),
        Column::new("MEM_WORST"),
    ]);
    for offset in &report.offsets {
        let mut row = vec![
            Cell::new(format!("+{}m", offset.offset_minutes)),
            Cell::new(if offset.unmeasured > 0 {
                format!("{} ({} unmeasured)", offset.paired, offset.unmeasured)
            } else {
                offset.paired.to_string()
            }),
            Cell::new(offset.pending),
            Cell::new(offset.missing),
            Cell::new(offset.unsampled),
            Cell::new(offset.partial),
            Cell::new(offset.gap),
        ];
        row.extend(error_cells(&offset.cpu, |cores| {
            format!("{cores:+.1} cores")
        }));
        row.extend(error_cells(&offset.memory, |bytes| {
            format!("{:+.1} GiB", bytes / GIB)
        }));
        offsets.add_row(row);
    }
    let mut drains = Table::new(vec![
        Column::new("WORKSPACE").fixed(),
        Column::new("DRAIN").fixed(),
        Column::new("SHADOW_PASSES").fixed(),
        Column::new("ENFORCE_PASSES").fixed(),
        Column::new("ADMITTED").fixed(),
        Column::new("WOULD_HOLD").fixed(),
        Column::new("WOULD_ADD").fixed(),
        Column::new("PULL_REQUESTS").fixed(),
        Column::new("PULL_SENT").fixed(),
        Column::new("PULL_WOULD_HOLD").fixed(),
        Column::new("ENFORCE_ADMITTED").fixed(),
        Column::new("GAP_HELD").fixed(),
        Column::new("PULL_HELD"),
    ])
    .empty_message("no admission forecasts recorded on this host");
    for drain in &report.drains {
        drains.add_row(vec![
            Cell::new(&drain.workspace_id),
            Cell::new(&drain.run_id),
            Cell::new(drain.shadow_passes),
            Cell::new(drain.enforce_passes),
            Cell::new(drain.admitted),
            Cell::new(drain.would_hold.len()),
            Cell::new(drain.would_add.len()),
            Cell::new(drain.pull_requests),
            Cell::new(drain.pull_sent),
            Cell::new(drain.pull_would_hold),
            Cell::new(drain.enforce_admitted),
            Cell::new(drain.gap_held.len()),
            Cell::new(drain.pull_held),
        ]);
    }
    Ok(Payload::blocks(
        doc,
        vec![
            Block::text(summary(&report)),
            Block::table(offsets),
            Block::table(drains),
        ],
    )
    .into())
}

fn summary(report: &ForecastAccuracy) -> String {
    format!(
        "{} admission forecasts since {} through {}; drain={}; mode={}; error is forecast minus measured.",
        report.forecasts,
        report.since.to_rfc3339(),
        report.report_at.to_rfc3339(),
        report.selection.drain.as_deref().unwrap_or("all"),
        report.selection.mode.as_deref().unwrap_or("all"),
    )
}

/// Mean error, under-forecast rate and worst miss, or `-` without pairs.
fn error_cells(summary: &ErrorSummary, value: impl Fn(f64) -> String) -> Vec<comfy_table::Cell> {
    use comfy_table::Cell;
    let (Some(mean), Some(under), Some(worst)) = (
        summary.mean_error,
        summary.under_rate,
        summary.worst_miss.as_ref(),
    ) else {
        return vec![Cell::new("-"), Cell::new("-"), Cell::new("-")];
    };
    vec![
        Cell::new(value(mean)),
        Cell::new(format!("{:.0}%", under * 100.0)),
        Cell::new(format!(
            "{} at {}",
            value(worst.error),
            worst.minute_at.format("%m-%d %H:%MZ")
        )),
    ]
}
