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
    after_help = "One resource profile per workspace, work class and complexity, from the CPU and memory\nrecent deliveries recorded. `--accuracy` instead compares admission forecasts with the\nhost's measured load. Read-only.\nColumn reference: docs/runbooks/forecast-accuracy.md.\n\nExamples:\n  orbit run profiles\n  orbit run profiles --json\n  orbit run profiles --accuracy\n  orbit run profiles --accuracy --wide\n  orbit run profiles --accuracy --since 2026-10-15T00:00:00Z --drain <RUN> --mode enforce --json"
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
    /// Print every per-offset accuracy column instead of the summary (accuracy only)
    #[arg(long, requires = "accuracy")]
    pub wide: bool,
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
            return accuracy(runtime, selection, report_at, self.wide);
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
            Column::new("CPU_P50").fixed(),
            Column::new("CPU_P90").fixed(),
            Column::new("PEAK_MEM").fixed(),
            Column::new("MEM_P50").fixed(),
            Column::new("MEM_P90").fixed(),
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
                Cell::new(format!("{:.1}", profile.delivery_peak_cpu.p50)),
                Cell::new(format!("{:.1}", profile.delivery_peak_cpu.p90)),
                Cell::new(format!(
                    "{:.1} GiB @ +{}m",
                    profile.peak_memory_bytes.value as f64 / GIB,
                    profile.peak_memory_bytes.minute
                )),
                Cell::new(format!(
                    "{:.1} GiB",
                    profile.delivery_peak_memory_bytes.p50 as f64 / GIB
                )),
                Cell::new(format!(
                    "{:.1} GiB",
                    profile.delivery_peak_memory_bytes.p90 as f64 / GIB
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

/// `--accuracy`: per-offset forecast error (a summary, or every column when
/// `wide`), then per-drain shadow and enforce decisions.
fn accuracy(
    runtime: &OrbitRuntime,
    selection: AccuracySelection,
    report_at: DateTime<Utc>,
    wide: bool,
) -> CommandOut {
    use crate::output::table::{Column, Table};
    use comfy_table::Cell;
    use orbit_types::workflow::DrainPassOutcome;

    let report = runtime.forecast_accuracy_selected(selection, report_at)?;
    let doc = serde_json::to_value(&report).map_err(|error| {
        orbit_core::OrbitError::Execution(format!("encode forecast accuracy: {error}"))
    })?;
    let offsets = if wide {
        wide_offsets(&report)
    } else {
        summary_offsets(&report)
    };
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
    let mut columns = vec![
        Column::new("WORKSPACE").fixed(),
        Column::new("DRAIN").fixed(),
        Column::new("PASSES").fixed(),
    ];
    columns.extend(
        DrainPassOutcome::ALL
            .iter()
            .map(|outcome| Column::new(&outcome.as_str().to_uppercase()).fixed()),
    );
    columns.push(Column::new("DETAIL"));
    let mut limiters =
        Table::new(columns).empty_message("no drain has counted an admission pass on this host");
    for drain in &report.limiters {
        let counters = &drain.counters;
        let mut row = vec![
            Cell::new(&drain.workspace_id),
            Cell::new(&drain.run_id),
            Cell::new(counters.passes),
        ];
        row.extend(DrainPassOutcome::ALL.iter().map(|outcome| {
            let count = counters.outcome(*outcome);
            Cell::new(format!("{:.0}% ({count})", counters.share(count) * 100.0))
        }));
        let detail = counters.describe_detail();
        row.push(Cell::new(if detail.is_empty() {
            "-".to_string()
        } else {
            detail
        }));
        limiters.add_row(row);
    }
    Ok(Payload::blocks(
        doc,
        vec![
            Block::text(summary(&report)),
            Block::table(offsets),
            Block::table(drains),
            Block::table(limiters),
        ],
    )
    .into())
}

/// Every per-offset column: maturity counts, raw and attributed errors with
/// their worst misses (`--accuracy --wide`).
fn wide_offsets(report: &ForecastAccuracy) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    use comfy_table::Cell;

    let mut offsets = Table::new(vec![
        Column::new("OFFSET").fixed(),
        Column::new("PAIRS").fixed(),
        Column::new("PENDING").fixed(),
        Column::new("MISSING").fixed(),
        Column::new("UNSAMPLED").fixed(),
        Column::new("PARTIAL").fixed(),
        Column::new("GAP").fixed(),
        Column::new("CPU_MEAN_ERR").fixed(),
        Column::new("CPU_MAE").fixed(),
        Column::new("CPU_UNDER").fixed(),
        Column::new("CPU_WORST").fixed(),
        Column::new("MEM_MEAN_ERR").fixed(),
        Column::new("MEM_MAE").fixed(),
        Column::new("MEM_UNDER").fixed(),
        Column::new("MEM_WORST").fixed(),
        Column::new("ATTR_PAIRS").fixed(),
        Column::new("ATTR_CPU_MEAN_ERR").fixed(),
        Column::new("ATTR_CPU_MAE").fixed(),
        Column::new("ATTR_CPU_UNDER").fixed(),
        Column::new("ATTR_CPU_WORST").fixed(),
        Column::new("ATTR_MEM_MEAN_ERR").fixed(),
        Column::new("ATTR_MEM_MAE").fixed(),
        Column::new("ATTR_MEM_UNDER").fixed(),
        Column::new("ATTR_MEM_WORST"),
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
        row.push(Cell::new(offset.attributed_paired));
        row.extend(error_cells(&offset.attributed_cpu, |cores| {
            format!("{cores:+.1} cores")
        }));
        row.extend(error_cells(&offset.attributed_memory, |bytes| {
            format!("{:+.1} GiB", bytes / GIB)
        }));
        offsets.add_row(row);
    }
    offsets
}

/// The per-offset summary `--accuracy` prints by default: pairs and the raw
/// CPU and memory errors an operator reads first, with each unit named once
/// in its header.
fn summary_offsets(report: &ForecastAccuracy) -> crate::output::table::Table {
    use crate::output::table::{Column, Table};
    use comfy_table::Cell;

    let mut offsets = Table::new(vec![
        Column::new("OFFSET").fixed(),
        Column::new("PAIRS").number(),
        Column::new("CPU_MEAN_ERR (cores)").number(),
        Column::new("CPU_MAE (cores)").number(),
        Column::new("CPU_UNDER (%)").number(),
        Column::new("MEM_MEAN_ERR (GiB)").number(),
        Column::new("MEM_UNDER (%)").number(),
    ]);
    let signed = |value: Option<f64>| value.map_or("-".to_string(), |value| format!("{value:+.1}"));
    let percent =
        |rate: Option<f64>| rate.map_or("-".to_string(), |rate| format!("{:.0}", rate * 100.0));
    for offset in &report.offsets {
        offsets.add_row(vec![
            Cell::new(format!("+{}m", offset.offset_minutes)),
            Cell::new(offset.paired),
            Cell::new(signed(offset.cpu.mean_error)),
            Cell::new(
                offset
                    .cpu
                    .mean_absolute_error
                    .map_or("-".to_string(), |mae| format!("{mae:.1}")),
            ),
            Cell::new(percent(offset.cpu.under_rate)),
            Cell::new(signed(offset.memory.mean_error.map(|bytes| bytes / GIB))),
            Cell::new(percent(offset.memory.under_rate)),
        ]);
    }
    offsets
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

/// Mean error, MAE, under-forecast rate and worst miss, or `-` without pairs.
fn error_cells(summary: &ErrorSummary, value: impl Fn(f64) -> String) -> Vec<comfy_table::Cell> {
    use comfy_table::Cell;
    let (Some(mean), Some(mae), Some(under), Some(worst)) = (
        summary.mean_error,
        summary.mean_absolute_error,
        summary.under_rate,
        summary.worst_miss.as_ref(),
    ) else {
        return vec![Cell::new("-"); 4];
    };
    vec![
        Cell::new(value(mean)),
        Cell::new(value(mae)),
        Cell::new(format!("{:.0}%", under * 100.0)),
        Cell::new(format!(
            "{} at {}",
            value(worst.error),
            worst.minute_at.format("%m-%d %H:%MZ")
        )),
    ]
}
