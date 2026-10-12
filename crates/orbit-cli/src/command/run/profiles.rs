//! `orbit run profiles` — the host's delivery resource profiles.

use std::fmt::Write as _;

use clap::Args;
use orbit_core::OrbitRuntime;
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
memory_bytes, memory_stddev_bytes), its deliveries and its flagged deliveries.\n\n\
Examples:\n  orbit run profiles\n  orbit run profiles --json"
)]
pub struct RunProfilesArgs {}

impl Execute for RunProfilesArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
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
